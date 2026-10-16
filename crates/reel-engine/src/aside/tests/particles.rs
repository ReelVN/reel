use super::*;
use reel_ui::stage::{NodeKind, particles::ParticleBatch};

fn batch(vm: &SessionVm) -> ParticleBatch {
    vm.stage()
        .nodes()
        .find_map(|n| match &n.kind {
            NodeKind::Particles(batch) => Some(batch.clone()),
            _ => None,
        })
        .unwrap()
}
fn dialogue(vm: &mut SessionVm, session: &mut Session) {
    let id = session.view().interaction.unwrap().id();
    for _ in 0..2 {
        session
            .handle_local_input(session.root_owner(), InputAction::Advance(id))
            .unwrap();
    }
    models::drive(vm, session, false);
}
#[test]
fn particles_source_asb_seed_pause_replacement_and_retirement() {
    let code = r#"fn opening() {
        let snow = particles("snow.png", rate: 40, lifetime: 5, seed: 42, width: 100, height: 20);
        stage.replace(snow);
        let mut errors = 0;
        try { particles("snow.png", rate: -1); } catch e { errors += 1; }
        try { particles("snow.png", size: 1, size_spread: 2); } catch e { errors += 1; }
        try { snow.rate = 2; } catch e { errors += 1; }
        if errors != 3 { throw "configuration"; }
        let first = emit(snow);
        let consumer = spawn(|| await first); consumer.cancel();
        narrator("running");
        pause(first);
        narrator("paused");
        resume(first);
        narrator("resumed");
        let second = emit(snow, duration: 1);
        if await first != "Stopped" || await first != "Stopped" { throw "replacement"; }
        narrator("restarted");
        if await second != "Completed" { throw "finite"; }
        let infinite = emit(snow);
        let mut rejected = false;
        try { finish(infinite); } catch e { rejected = true; }
        if !rejected { throw "infinite finish"; }
        stop(infinite);
        if await infinite != "Stopped" { throw "stop"; }
        let ended = emit(snow, duration: 1); finish(ended);
        if await ended != "Completed" { throw "finish"; }
        let owner = stack();
        snow.remove();
        let snow2 = particles("snow.png"); stage.replace(snow2);
        let owned = emit(snow2, owner: owner); owner.remove();
        let mut cancelled = false;
        try { await owned; } catch e { cancelled = true; }
        if !cancelled { throw "owner cancellation"; }
        let retired = emit(snow2); snow2.remove();
        cancelled = false;
        try { await retired; } catch e { cancelled = true; }
        if !cancelled { throw "target cancellation"; }
        stage.replace(text("passed"));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        activities::advance_clock(&mut vm, &session, 1., 1);
        let initial = batch(&vm).samples().collect::<Vec<_>>();
        assert_eq!(initial.len(), 40);
        dialogue(&mut vm, &mut session);
        assert_eq!(vm.activities_poll_after(), None);
        activities::advance_clock(&mut vm, &session, 50., 1);
        assert_eq!(batch(&vm).samples().collect::<Vec<_>>(), initial);
        dialogue(&mut vm, &mut session);
        activities::advance_clock(&mut vm, &session, 51., 1);
        assert_eq!(batch(&vm).samples().count(), 80);
        dialogue(&mut vm, &mut session);
        activities::advance_clock(&mut vm, &session, 52., 1);
        assert_eq!(batch(&vm).samples().collect::<Vec<_>>(), initial);
        dialogue(&mut vm, &mut session);
        activities::advance_clock(&mut vm, &session, 57., 1);
        models::drive(&mut vm, &mut session, false);
        assert!(
            vm.stage()
                .nodes()
                .any(|node| node.kind == NodeKind::Text("passed".into()))
        );
        assert!(!vm.activities_need_time());
        collect(&mut vm);
    }
}

#[test]
fn particles_clock_slices_storage_and_tween_coexist_without_particle_objects() {
    let code = r#"fn opening() {
        let root = stack(); stage.replace(root);
        for i in [0,1,2,3,4,5,6,7,8,9] {
            let snow = root.add(particles("snow.png", rate: 10000, lifetime: 2, seed: i));
            emit(snow);
        }
        run(tween(root, x: 10, duration: 2000000));
        narrator("running");
    }"#;
    let (mut session, mut vm) = models::prepared(code, false);
    models::drive(&mut vm, &mut session, false);
    activities::advance_clock(&mut vm, &session, 1., 1);
    let before = vm.vm.heap().stats();
    let config = batch(&vm).config;
    let started = std::time::Instant::now();
    for step in 1..=120 {
        activities::advance_clock(&mut vm, &session, 1. + f64::from(step) / 60., 1);
    }
    assert_eq!(
        vm.vm.heap().stats().chunk_allocations,
        before.chunk_allocations
    );
    assert!(std::sync::Arc::ptr_eq(&config, &batch(&vm).config));
    assert_eq!(vm.stage().nodes().count(), 11);
    assert_eq!(batch(&vm).samples().count(), 20_000);
    let progress = activities::advance_clock(&mut vm, &session, 1_000_000., 32);
    assert!(
        progress.work < 100,
        "skip expired births directly: {progress:?}"
    );
    eprintln!(
        "10 emitters x 20,000 live sprites, 120 ticks: {:?}; zero VM chunks; 11 stage nodes",
        started.elapsed()
    );
}

#[test]
fn particle_example_uses_registered_author_interfaces() {
    source::compile_story(
        include_str!("../../../../../examples/author_particles.aside"),
        "author_particles",
        1,
        "main",
    )
    .unwrap();
}

#[test]
fn particles_trace_detached_owner_and_release_terminal_producer_roots() {
    let code = r#"fn build() {
        let owner = stack();
        let snow = particles("snow.png", rate: 4, lifetime: 1);
        stage.replace(snow);
        emit(snow, duration: 1, owner: owner)
    }
    fn opening() {
        let active = build();
        narrator("owner retained");
        if await active != "Completed" { throw "owner lost"; }
        stage.replace(text("done"));
    }"#;
    let (mut session, mut vm) = models::prepared(code, true);
    models::drive(&mut vm, &mut session, false);
    collect(&mut vm);
    vm.sync_stage(&session).unwrap();
    activities::advance_clock(&mut vm, &session, 0.5, 1);
    assert_eq!(batch(&vm).samples().count(), 2);
    dialogue(&mut vm, &mut session);
    activities::advance_clock(&mut vm, &session, 2., 1);
    models::drive(&mut vm, &mut session, false);
    collect(&mut vm);
    vm.sync_stage(&session).unwrap();
    assert!(!vm.particles.has_active());
    assert_eq!(vm.stage().nodes().count(), 1);
    let roots = vm.vm.heap().stats().roots;
    assert_eq!(roots, 1, "only mounted stage root remains");
}

#[test]
fn particles_stop_during_a_partial_clock_pass_keeps_the_pending_prefix_exact() {
    let code = r#"fn opening() {
        let root = stack(); stage.replace(root);
        let first = emit(root.add(particles("snow.png", rate: 4, lifetime: 0.5)), duration: 0.25);
        let second = emit(root.add(particles("snow.png", rate: 4, lifetime: 0.5)), duration: 0.25);
        let third = emit(root.add(particles("snow.png", rate: 4, lifetime: 0.5)), duration: 0.25);
        stop(second);
        if await second != "Stopped" { throw "stop result"; }
        if await first != "Completed" || await third != "Completed" { throw "completion"; }
        stage.replace(text("done"));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        let stop = loop {
            match vm.step(&mut session, SliceBudget::default(), || 0).unwrap() {
                SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                    let package = vm.vm.program().package();
                    if package.symbols[package.natives[ticket.native.index()].symbol.index()]
                        == "reel::stop"
                    {
                        break ticket;
                    }
                    vm.dispatch(&mut session, ticket, "en", 32).unwrap();
                }
                SessionStep::Vm(RunOutcome::Fault(id)) => {
                    panic!("{:?}", vm.vm.delivery_fault(id).unwrap());
                }
                SessionStep::Idle | SessionStep::Vm(RunOutcome::Waiting(_)) => {
                    panic!("story must reach stop");
                }
                _ => {}
            }
        };
        let first = vm.advance_time(&session, 1., 1).unwrap();
        assert!(first.pending);
        assert_eq!(first.work, 1);
        // The first producer has completed; stop the unprocessed second one
        // before resuming the public, budgeted clock operation.
        vm.dispatch(&mut session, stop, "en", 32).unwrap();
        let last = vm.advance_time(&session, 1., 32).unwrap();
        assert_eq!(last.work, 1, "only the third producer remains unprocessed");
        assert!(!last.pending);
        assert!(!vm.particles.has_active());
        models::drive(&mut vm, &mut session, false);
        assert!(
            vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("done".into()))
        );
    }
}
