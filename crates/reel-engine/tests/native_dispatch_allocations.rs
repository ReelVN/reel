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
