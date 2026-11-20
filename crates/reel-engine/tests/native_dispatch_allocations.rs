//! Measure native symbol dispatch separately from VM execution and setup.
use reel_engine::{
    Epoch, Session, SessionId,
    aside::{
        NativeBudget, NativeDispatch, RunOutcome, SessionBudget, SessionStep, SessionVm,
        SliceBudget, VmLimits, source,
    },
};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::alloc::System;

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

#[test]
fn reading_an_absent_stage_pointer_does_not_allocate() {
    let story = source::compile_story(
        "fn opening() { return stage.pointer(); }",
        "native_dispatch",
        1,
        "opening",
    )
    .unwrap();
    let mut session = Session::new(SessionId(1), Epoch(1)).unwrap();
    let entries = [story.initializer, story.entry];
    let mut vm = SessionVm::load_story(&session, story, VmLimits::default()).unwrap();
    let budget = SessionBudget::RetryMinimum(SliceBudget::default());
    vm.start(&session, entries[0]).unwrap();
    assert!(matches!(
        vm.step(&mut session, budget, || 0).unwrap(),
        SessionStep::Vm(RunOutcome::Returned(_))
    ));
    vm.discard_delivery(&mut session).unwrap();
    vm.start(&session, entries[1]).unwrap();
    let SessionStep::Vm(RunOutcome::HostCall(ticket)) =
        vm.step(&mut session, budget, || 0).unwrap()
    else {
        panic!("expected stage.pointer native call");
    };

    let region = Region::new(GLOBAL);
    let outcome = vm
        .dispatch(&mut session, ticket, "en", NativeBudget::Exact(1))
        .unwrap();
    let allocations = region.change();
    assert!(matches!(outcome, NativeDispatch::Ready { work: 1 }));
    assert_eq!(allocations.allocations, 0, "{allocations:?}");
    assert_eq!(allocations.reallocations, 0, "{allocations:?}");
}

#[test]
#[ignore = "release allocation measurement of ordinary stage construction"]
fn constant_stage_construction_cost() {
    let story = source::compile_story(
        r#"fn opening() {
            let root = stack();
            for i in 0..256 { root.add(stack(x: i, y: 10)); }
            stage.replace(root);
        }"#,
        "native_dispatch",
        1,
        "opening",
    )
    .unwrap();
    let mut session = Session::new(SessionId(1), Epoch(1)).unwrap();
    let entries = [story.initializer, story.entry];
    let mut vm = SessionVm::load_story(&session, story, VmLimits::default()).unwrap();
    let budget = SessionBudget::RetryMinimum(SliceBudget::default());
    let mut calls = 0;
    let mut allocations = 0;
    let mut bytes = 0;
    let mut elapsed = std::time::Duration::ZERO;
    for entry in entries {
        vm.start(&session, entry).unwrap();
        for _ in 0..10_000 {
            match vm.step(&mut session, budget, || 0).unwrap() {
                SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                    let package = vm.vm().program().package();
                    let track = package.symbols
                        [package.natives[ticket.native.index()].symbol.index()]
                        == "reel::stack";
                    let region = Region::new(GLOBAL);
                    let start = std::time::Instant::now();
                    let outcome = vm
                        .dispatch(&mut session, ticket, "en", NativeBudget::Exact(1024))
                        .unwrap();
                    assert!(matches!(outcome, NativeDispatch::Ready { .. }));
                    if track {
                        elapsed += start.elapsed();
                        let stats = region.change();
                        calls += 1;
                        allocations += stats.allocations;
                        bytes += stats.bytes_allocated;
                    }
                }
                SessionStep::Vm(RunOutcome::Returned(_)) => {
                    vm.discard_delivery(&mut session).unwrap();
                    break;
                }
                SessionStep::Progress | SessionStep::Vm(RunOutcome::BudgetYield(_)) => {}
                outcome => panic!("unexpected construction step: {outcome:?}"),
            }
        }
    }
    vm.sync_stage(&session).unwrap();
    let root = vm.stage().root().unwrap();
    let children = &vm.stage().node(root).unwrap().children;
    assert_eq!(children.len(), 256);
    assert_eq!(calls, children.len() + 1);
    for (index, &child) in children.iter().enumerate() {
        let props = &vm.stage().node(child).unwrap().props;
        assert_eq!((props.x, props.y), (index as f64, 10.));
    }
    println!(
        "stage constructors={calls} allocations={allocations} bytes={bytes} elapsed_us={}",
        elapsed.as_micros()
    );
}
