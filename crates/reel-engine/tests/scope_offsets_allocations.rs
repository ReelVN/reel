//! Scope layers reuse their admitted storage during idle projection and motion.
use reel_engine::{
    Epoch, InputAction, Session, SessionId,
    aside::{self, RunOutcome, SessionBudget, SessionStep, SessionVm, SliceBudget, VmLimits},
};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::{alloc::System, time::Instant};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

#[test]
fn scope_offsets_reuse_storage_for_idle_projection_motion_and_removal() {
    let mut baseline = None;
    let mut removal_baseline = None;
    for layers in [0, 1, 64, 256] {
        let (mut session, mut vm) = prepared(&format!(
            r#"fn opening() {{
                let panel=stack(x:10,width:40,height:20);stage.replace(panel);
                run(tween(panel,x:110,duration:100));
                scope(body:|s|{{
                    let mut i=0;
                    while i<{layers} {{
                        let layer=s.offset(panel,x:1,y:-1);
                        run(tween(layer,x:11,duration:100));i+=1;
                    }}
                    narrator("measure");
                }});
                narrator("removed");
            }}"#
        ));
        sample(&mut vm, &session, 1.);
        let revision = vm.stage().revision();
        let heap = vm.vm().heap().stats();
        let region = Region::new(GLOBAL);
        let started = Instant::now();
        for _ in 0..1000 {
            vm.sync_stage(&session).unwrap();
        }
        let idle_time = started.elapsed();
        let idle = region.change();
        assert_eq!(vm.stage().revision(), revision);
        assert_eq!(
            vm.vm().heap().stats().chunk_allocations,
            heap.chunk_allocations
        );
        let region = Region::new(GLOBAL);
        let started = Instant::now();
        for frame in 1..=120 {
            sample(&mut vm, &session, 1. + f64::from(frame) / 60.);
        }
        let motion_time = started.elapsed();
        let motion = region.change();
        assert_eq!(
            vm.vm().heap().stats().chunk_allocations,
            heap.chunk_allocations
        );
        let panel = vm.stage().nodes().next().unwrap();
        assert!((panel.props.x - (13. + f64::from(layers) * 1.3)).abs() < 1e-8);
        assert_eq!(panel.props.y, -f64::from(layers));
        let counts = (
            idle.allocations,
            idle.reallocations,
            motion.allocations,
            motion.reallocations,
        );
        if let Some(baseline) = baseline {
            assert_eq!(
                counts, baseline,
                "layer count added steady-state allocations"
            );
        } else {
            baseline = Some(counts);
        }
        eprintln!(
            "offsets={layers}; idle 1000={idle_time:?}, allocations={} reallocations={}; motion 120={motion_time:?}, allocations={} reallocations={}; VM charged={} peak={} new_chunks=0",
            idle.allocations,
            idle.reallocations,
            motion.allocations,
            motion.reallocations,
            heap.charged_bytes,
            heap.peak_charged_bytes,
        );

        let interaction = session.view().interaction.unwrap().id();
        for _ in 0..2 {
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(interaction))
                .unwrap();
        }
        let mut removed = false;
        for _ in 0..1000 {
            match vm
                .step(
                    &mut session,
                    SessionBudget::RetryMinimum(SliceBudget::default()),
                    || 0,
                )
                .unwrap()
            {
                SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                    let package = vm.vm().program().package();
                    let closing = package.symbols
                        [package.natives[ticket.native.index()].symbol.index()]
                        == "reel::scope"
                        && ticket.input == aside_vm::executor::NativeInput::CallbackReturned;
                    let region = Region::new(GLOBAL);
                    let started = Instant::now();
                    vm.dispatch(&mut session, ticket, "en", 64).unwrap();
                    if closing {
                        let elapsed = started.elapsed();
                        let removal = region.change();
                        let counts = (removal.allocations, removal.reallocations);
                        if let Some(baseline) = removal_baseline {
                            assert_eq!(counts, baseline, "layer cleanup added allocations");
                        } else {
                            removal_baseline = Some(counts);
                        }
                        assert_eq!(
                            vm.vm().heap().stats().chunk_allocations,
                            heap.chunk_allocations
                        );
                        eprintln!(
                            "offsets={layers}; remove={elapsed:?}, allocations={} reallocations={}",
                            removal.allocations, removal.reallocations
                        );
                        removed = true;
                        break;
                    }
                }
                SessionStep::Progress | SessionStep::Vm(RunOutcome::BudgetYield(_)) => {}
                other => panic!("scope did not return: {other:?}"),
            }
        }
        assert!(removed);
        drive(&mut vm, &mut session);
        assert_eq!(vm.stage().nodes().next().unwrap().props.x, 13.);
    }
}

fn sample(vm: &mut SessionVm, session: &Session, time: f64) {
    while vm.advance_time(session, time, 4096).unwrap().pending {}
    vm.sync_stage(session).unwrap();
}

fn prepared(code: &str) -> (Session, SessionVm) {
    let mut session = Session::new(SessionId(1), Epoch(1)).unwrap();
    let story = aside::source::compile_story(code, "offset_allocations", 1, "opening").unwrap();
    let mut vm = SessionVm::load(&session, story.program, VmLimits::default()).unwrap();
    for entry in [story.initializer, story.entry] {
        vm.start(&session, entry).unwrap();
        drive(&mut vm, &mut session);
    }
    (session, vm)
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
                vm.dispatch(session, ticket, "en", 64).unwrap();
            }
            SessionStep::Vm(RunOutcome::Returned(_)) => vm.discard_delivery(session).unwrap(),
            SessionStep::Vm(RunOutcome::Fault(fault)) => panic!("offset fault {fault:?}"),
            SessionStep::Vm(RunOutcome::ResourceBlocked(error)) => {
                panic!("offset resource {error:?}")
            }
            SessionStep::Idle | SessionStep::Vm(RunOutcome::Waiting(_))
                if !vm.has_runnable_tasks() && !vm.activities_pending() =>
            {
                vm.sync_stage(session).unwrap();
                return;
            }
            _ => {}
        }
    }
    panic!("offset task failed to settle");
}
