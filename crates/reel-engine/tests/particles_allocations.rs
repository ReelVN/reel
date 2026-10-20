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
        let (session, mut vm) = prepared(
            &r#"fn opening() {
            let snow=particles("snow.png",width:1920); stage.replace(snow);
            emit(snow,burst:0,rate:10000,lifetime:2);
        }"#
            .replace("10000", &rate.to_string()),
        );
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
    let (session, mut vm) = prepared(
        r#"fn opening() {
        let p=particles("spark.png"); stage.replace(p);
        let mut i=0;
        while i<64 { emit(p,lifetime:0.5); i+=1; }
    }"#,
    );
    let reel_ui::stage::NodeKind::Particles(old) = &vm.stage().nodes().next().unwrap().kind else {
        unreachable!()
    };
    let old = old.clone();
    assert_eq!(old.batches.len(), 64);
    while vm.advance_time(&session, 1., 8).unwrap().pending {}
    let reel_ui::stage::NodeKind::Particles(now) = &vm.stage().nodes().next().unwrap().kind else {
        unreachable!()
    };
    assert!(now.batches.is_empty());
    drop(vm);
    let region = Region::new(GLOBAL);
    drop(old);
    let released = region.change();
    assert_eq!(released.allocations, 0);
    assert!(
        released.bytes_deallocated >= 64 * size_of::<reel_ui::stage::particles::ParticleEmission>()
    );
    eprintln!(
        "last retained 64-batch view release: deallocations={} bytes_deallocated={} (allocator requests, not RSS)",
        released.deallocations, released.bytes_deallocated
    );
}

fn prepared(code: &str) -> (Session, SessionVm) {
    let mut session = Session::new(SessionId(1), Epoch(1)).unwrap();
    let story = aside::source::compile_story(code, "particles", 1, "opening").unwrap();
    let mut vm = SessionVm::load(&session, story.program, VmLimits::default()).unwrap();
    for entry in [story.initializer, story.entry] {
        vm.start(&session, entry).unwrap();
        loop {
            match vm
                .step(
                    &mut session,
                    reel_engine::aside::SessionBudget::Exact(SliceBudget::default()),
                    || 0,
                )
                .unwrap()
            {
                SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                    vm.dispatch(&mut session, ticket, "en", 64).unwrap();
                }
                SessionStep::Vm(RunOutcome::Returned(_)) => {
                    vm.discard_delivery(&mut session).unwrap();
                    break;
                }
                SessionStep::Vm(RunOutcome::Fault(fault)) => panic!("particle fault {fault:?}"),
                SessionStep::Vm(RunOutcome::ResourceBlocked(error)) => {
                    panic!("particle resource {error:?}")
                }
                _ => {}
            }
        }
    }
    (session, vm)
}
