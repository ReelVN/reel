//! Fixed image properties add no temporary allocations to transform projection.
use reel_engine::{
    Epoch, Session, SessionId,
    aside::{
        NativeBudget, RunOutcome, SessionBudget, SessionStep, SessionVm, SliceBudget, VmLimits,
        source,
    },
};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::{alloc::System, time::Instant};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

#[test]
fn moving_images_reuse_fixed_property_storage() {
    let mut baseline = None;
    for constructor in [
        "image(\"scene.png\")",
        "image(\"scene.png\", sampling: \"nearest\", alpha_layout: \"top_bottom\")",
    ] {
        let mut session = Session::new(SessionId(1), Epoch(1)).unwrap();
        let script = format!(
            r#"fn opening() {{
            let panel = {constructor};
            stage.replace(panel);
            tween(panel, x: 100, duration: 100).run();
            narrator("measure");
        }}"#
        );
        let story = source::compile_story(&script, "image_projection", 1, "opening").unwrap();
        let entries = [story.initializer, story.entry];
        let mut vm = SessionVm::load_story(&session, story, VmLimits::default()).unwrap();
        for entry in entries {
            vm.start(&session, entry).unwrap();
            drive(&mut vm, &mut session);
        }
        sample(&mut vm, &session, 1.);
        let before = vm.vm().heap().stats();
        let region = Region::new(GLOBAL);
        let started = Instant::now();
        for frame in 1..=1200 {
            sample(&mut vm, &session, 1. + f64::from(frame) / 60.);
        }
        let elapsed = started.elapsed();
        let allocations = region.change();
        assert_eq!(vm.stage().nodes().next().unwrap().props.x, 21.);
        assert_eq!(
            vm.vm().heap().stats().chunk_allocations,
            before.chunk_allocations
        );
        let cost = (
            allocations.allocations,
            allocations.reallocations,
            allocations.bytes_allocated,
        );
        assert!(
            allocations.allocations <= 1200,
            "single-node motion allocated a staging buffer: {allocations:?}"
        );
        if let Some(baseline) = baseline {
            assert_eq!(
                cost, baseline,
                "image enum decoding added temporary storage"
            );
        } else {
            baseline = Some(cost);
        }
        println!(
            "projection constructor={constructor} frames=1200 elapsed_us={} allocations={} reallocations={} bytes={}",
            elapsed.as_micros(),
            cost.0,
            cost.1,
            cost.2
        );
    }
}

fn sample(vm: &mut SessionVm, session: &Session, time: f64) {
    while vm.advance_time(session, time, 64).unwrap().pending {}
    vm.sync_stage(session).unwrap();
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
                vm.dispatch(session, ticket, "en", NativeBudget::Exact(64))
                    .unwrap();
            }
            SessionStep::Vm(RunOutcome::Returned(_)) => vm.discard_delivery(session).unwrap(),
            SessionStep::Vm(RunOutcome::Fault(fault)) => panic!("image projection fault {fault:?}"),
            SessionStep::Vm(RunOutcome::ResourceBlocked(error)) => {
                panic!("image projection resource {error:?}")
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
    panic!("image projection failed to settle");
}
