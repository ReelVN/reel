use reel_engine::{
    Epoch, Session, SessionId,
    aside::{self, RunOutcome, SessionStep, SessionVm, SliceBudget, VmLimits},
};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::alloc::System;
#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

#[test]
fn particle_clock_and_sprite_sampling_reuse_constant_storage() {
    for rate in [64, 1024, 10000] {
        let mut session = Session::new(SessionId(1), Epoch(1)).unwrap();
        let story = aside::source::compile_story(
            &r#"fn opening() {
        let snow = particles("snow.png", width: 1920, rate: 10000, lifetime: 2);
        stage.replace(snow); emit(snow);
    }"#
            .replace("10000", &rate.to_string()),
            "particles",
            1,
            "opening",
        )
        .unwrap();
        let mut vm = SessionVm::load(&session, story.program, VmLimits::default()).unwrap();
        for entry in [story.initializer, story.entry] {
            vm.start(&session, entry).unwrap();
            loop {
                match vm.step(&mut session, SliceBudget::default(), || 0).unwrap() {
                    SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                        vm.dispatch(&mut session, ticket, "en", 64).unwrap();
                    }
                    SessionStep::Vm(RunOutcome::Returned(_)) => {
                        vm.discard_delivery(&mut session).unwrap();
                        break;
                    }
                    SessionStep::Vm(RunOutcome::Fault(fault)) => panic!("particle fault {fault:?}"),
                    _ => {}
                }
            }
        }
        while vm.advance_time(&session, 3., 64).unwrap().pending {}
        let region = Region::new(GLOBAL);
        let mut checksum = 0.;
        let start = std::time::Instant::now();
        for frame in 1..=120 {
            while vm
                .advance_time(&session, 3. + f64::from(frame) / 60., 64)
                .unwrap()
                .pending
            {}
            let reel_ui::stage::NodeKind::Particles(batch) =
                &vm.stage().nodes().next().unwrap().kind
            else {
                unreachable!()
            };
            checksum += batch
                .samples()
                .map(|sprite| sprite.position[1])
                .sum::<f64>();
        }
        let elapsed = start.elapsed();
        let stats = region.change();
        assert!(checksum > 0.);
        assert_eq!(stats.allocations, 0, "{stats:?}");
        assert_eq!(stats.reallocations, 0, "{stats:?}");
        eprintln!(
            "120 x {} sprite samples: {elapsed:?}; allocations=0 reallocations=0; VM peak_charged_bytes={}",
            rate * 2,
            vm.vm().heap().stats().peak_charged_bytes
        );
    }
}
