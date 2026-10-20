//! Compare authored reset segments with explicit starts on the same activity path.
use reel_engine::{
    Epoch, Session, SessionId,
    aside::{self, RunOutcome, SessionBudget, SessionStep, SessionVm, SliceBudget, VmLimits},
};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::alloc::System;

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

#[test]
fn tween_from_steady_samples_and_repeated_activation_cost() {
    for (name, action) in [
        (
            "reset_segment",
            "repeat(sequence(tween(node,x:100,y:200,duration:2),tween(node,x:0,y:0,duration:0)))",
        ),
        (
            "explicit_from",
            "repeat(tween(node,from:{x:0,y:0},x:100,y:200,duration:2))",
        ),
    ] {
        let source = format!(
            "fn opening() {{let node=stack();stage.replace(node);run({action});narrator(\"running\");}}"
        );
        let mut session = Session::new(SessionId(1), Epoch(1)).unwrap();
        let story = aside::source::compile_story(&source, name, 1, "opening").unwrap();
        let mut vm = SessionVm::load(&session, story.program, VmLimits::default()).unwrap();
        vm.start(&session, story.initializer).unwrap();
        drive(&mut vm, &mut session);
        let chunks = vm.vm().heap().stats().chunk_allocations;
        let region = Region::new(GLOBAL);
        vm.start(&session, story.entry).unwrap();
        drive(&mut vm, &mut session);
        let construction = region.change();
        let build_chunks = vm.vm().heap().stats().chunk_allocations - chunks;
        eprintln!(
            "{name}: construct allocations={} reallocations={} VM_chunks={build_chunks}",
            construction.allocations, construction.reallocations
        );
        for (phase, first, last) in [("steady", 1, 120), ("repeat", 121, 480)] {
            let chunks = vm.vm().heap().stats().chunk_allocations;
            let region = Region::new(GLOBAL);
            let mut work = 0;
            for frame in first..=last {
                loop {
                    let progress = vm
                        .advance_time(&session, f64::from(frame) / 120., 8)
                        .unwrap();
                    work += progress.work;
                    if !progress.pending {
                        break;
                    }
                }
            }
            let measured = region.change();
            let chunks = vm.vm().heap().stats().chunk_allocations - chunks;
            eprintln!(
                "{name}: {phase} samples={} allocations={} reallocations={} VM_chunks={chunks} work={work}",
                last - first + 1,
                measured.allocations,
                measured.reallocations
            );
            assert_eq!(chunks, 0);
            if phase == "steady" {
                assert_eq!((measured.allocations, measured.reallocations), (0, 0));
            }
        }
    }
}

fn drive(vm: &mut SessionVm, session: &mut Session) {
    for _ in 0..100_000 {
        match vm
            .step(
                session,
                SessionBudget::RetryMinimum(SliceBudget::default()),
                || 0,
            )
            .unwrap()
        {
            SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                vm.dispatch(session, ticket, "en", 1024).unwrap();
            }
            SessionStep::Vm(RunOutcome::Returned(_)) => vm.discard_delivery(session).unwrap(),
            SessionStep::Vm(RunOutcome::Fault(id)) => panic!("activity fault: {id:?}"),
            SessionStep::Vm(RunOutcome::ResourceBlocked(error)) => panic!("{error:?}"),
            SessionStep::Idle | SessionStep::Vm(RunOutcome::Waiting(_))
                if !vm.has_runnable_tasks() && !vm.activities_pending() =>
            {
                vm.sync_stage(session).unwrap();
                return;
            }
            _ => {}
        }
    }
    panic!("activity did not settle");
}
