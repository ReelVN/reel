use super::*;
use reel_ui::stage::{NodeKind, particles::ParticleNode};

#[test]
fn particle_size_scale_preserves_trajectories_bindings_pause_and_restore_source_asb() {
    let code = r#"use std::math::curve;
        fn opening() {
            let state = model({scale: 1});
            let p = particles("spark.png", seed: 42, size_scale: bind(state.scale));
            stage.replace(p);
            let active = emit(p, burst: 0, rate: 4, lifetime: 3, prewarm: 1,
                size: 20, size_spread: 0, velocity_x: 10,
                size_curve: curve([{time: 0, value: 0}, {time: 1, value: 1}]));
            narrator("original");
            state.scale = 0.5; pause(active);
            narrator("half");
            resume(active); run(tween(p, size_scale: 0, duration: 1));
            narrator("shrinking");
            p.remove();
            let mut cancelled = false;
            try { await active; } catch error { cancelled = true; }
            if !cancelled { throw "retired emission"; }
            narrator("retired");
        }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        let original = batch(&vm).samples().collect::<Vec<_>>();
        assert_eq!(original.len(), 4);
        dialogue(&mut vm, &mut session);
        let half = batch(&vm).samples().collect::<Vec<_>>();
        for (mut expected, actual) in original.into_iter().zip(&half) {
            expected.size *= 0.5;
            assert_eq!(expected, *actual);
        }
        (session, vm) = scopes::restored(&mut vm, &session);
        activities::advance_clock(&mut vm, &session, 8., 32);
        assert_eq!(batch(&vm).samples().collect::<Vec<_>>(), half);
        dialogue(&mut vm, &mut session);
        for (time, scale) in [(8.5, 0.25), (9., 0.)] {
            activities::advance_clock(&mut vm, &session, time, 32);
            vm.sync_stage(&session).unwrap();
            let node = batch(&vm);
            assert_eq!(node.size_scale, scale);
            assert!(
                node.samples().count() > half.len(),
                "new births share the scale"
            );
            for (mut expected, actual) in node
                .batches
                .iter()
                .flat_map(|batch| batch.samples(node.time))
                .zip(node.samples())
            {
                expected.size *= scale;
                assert_eq!(expected, actual);
            }
            assert!(vm.stage_ready());
            assert_eq!(vm.maintain_particles(64).unwrap(), 0);
        }
        dialogue(&mut vm, &mut session);
        assert!(!vm.particles.has_active());
    }
}

#[test]
fn particle_envelope_radial_motion_prewarm_pause_and_restore() {
    let code = r#"use std::math::curve;
        fn opening() {
            let p = particles("spark.png", seed: 42);
            stage.replace(p);
            let active = emit(p, burst: 0, rate: 64, lifetime: 2, prewarm: 2,
                velocity_x: 0, velocity_y: 0, spread_x: 0, spread_y: 0,
                radial_speed: 100, radial_spread: 20, damping: 2,
                angle: 180, angle_spread: 180, spin_spread: 0,
                size: 10, size_spread: 0,
                size_curve: curve([{time: 0, value: 0}, {time: 0.5, value: 1}, {time: 1, value: 0}]));
            narrator("running"); pause(active);
            narrator("paused"); resume(active);
            narrator("resumed"); p.remove();
            narrator("retired");
        }"#;
    let mut canonical = None;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        let first = batch(&vm);
        assert_eq!(first.samples().count(), 128);
        let emission = &first.batches[0];
        let frame = emission.frame_at(0.);
        assert_eq!(emission.config.sample(frame, 128).size, 0.);
        assert_eq!(emission.config.sample(frame, 64).size, 10.);
        assert_eq!(emission.config.sample(frame, 0).size, 0.);
        let sprites: Vec<_> = first.samples().collect();
        for axis in 0..2 {
            assert!(sprites.iter().any(|p| p.position[axis] > 20.));
            assert!(sprites.iter().any(|p| p.position[axis] < -20.));
        }
        assert!(sprites.iter().any(|p| p.rotation > 270.));
        assert!(sprites.iter().any(|p| p.rotation < 90.));
        if let Some(expected) = &canonical {
            assert_eq!(&sprites, expected);
        } else {
            canonical = Some(sprites);
        }
        let (mut split_session, mut split) = scopes::restored(&mut vm, &session);
        for tick in 1..=60 {
            activities::advance_clock(&mut split, &split_session, f64::from(tick) / 60., 32);
        }
        activities::advance_clock(&mut vm, &session, 1., 32);
        assert_eq!(
            batch(&vm).samples().collect::<Vec<_>>(),
            batch(&split).samples().collect::<Vec<_>>()
        );
        dialogue(&mut split, &mut split_session);
        let paused = batch(&split).samples().collect::<Vec<_>>();
        (split_session, split) = scopes::restored(&mut split, &split_session);
        activities::advance_clock(&mut split, &split_session, 8., 32);
        assert_eq!(paused, batch(&split).samples().collect::<Vec<_>>());
        dialogue(&mut split, &mut split_session);
        activities::advance_clock(&mut split, &split_session, 8.5, 32);
        assert_ne!(paused, batch(&split).samples().collect::<Vec<_>>());
        dialogue(&mut split, &mut split_session);
        assert!(!split.particles.has_active());
    }
}

#[test]
fn is_checked_node_keeps_particle_type_in_owned_task_after_restore() {
    let code = r#"use reel::types::{Node,ParticlesNode};
        fn spray(node:Node) {
            if node is ParticlesNode {
                return spawn(||{
                    emit(node,burst:1,lifetime:10);
                    await delay(1).run();
                    emit(node,burst:1,lifetime:10);
                },owner:node);
            } else {throw "expected particles";}
        }
        fn opening(){
            let node=particles("spark.png");stage.replace(node);
            let task=spray(node);
            narrator("first");
            await task;
            narrator("second");
        }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        assert_eq!(batch(&vm).samples().count(), 1);
        (session, vm) = scopes::restored(&mut vm, &session);
        dialogue(&mut vm, &mut session);
        activities::advance_clock(&mut vm, &session, 1., 64);
        models::drive(&mut vm, &mut session, false);
        assert_eq!(batch(&vm).samples().count(), 2);
        assert!(matches!(session.view().interaction,
            Some(crate::InteractionView::Dialogue(d)) if d.text()=="second"));
    }
}

fn batch(vm: &SessionVm) -> ParticleNode {
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
fn particle_fade_publishes_middle_frames_without_restarting_owner_maintenance() {
    let code = r#"fn opening() {
        let p=particles("spark.png");
        let owner=stack();
        stage.replace(stack() {p; owner;});
        emit(p,burst:2,lifetime:3,velocity_x:100,velocity_y:0,spread_x:0,spread_y:0);
        let paused=emit(p,owner:owner);pause(paused);
        run(tween(p,opacity:0,duration:2));
        await stage.presented();
        narrator("middle frame accepted");
        owner.remove();
        let mut cancelled=false;
        try {await paused;} catch error {cancelled=true;}
        if !cancelled {throw "paused owner survived retirement";}
        stage.replace(text("retired"));
        narrator("done");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        activities::advance_clock(&mut vm, &session, 0.5, 1);
        vm.sync_stage(&session).unwrap();
        let particles = vm
            .stage()
            .nodes()
            .find(|node| matches!(node.kind, NodeKind::Particles(_)))
            .unwrap();
        assert_eq!(particles.props.opacity, 0.75);
        assert_eq!(batch(&vm).samples().next().unwrap().position[0], 50.);
        assert!(
            vm.stage_ready(),
            "ordinary opacity publication must not invalidate a completed particle clock pass"
        );
        assert_eq!(
            vm.maintain_particles(64).unwrap(),
            0,
            "property-only revision must not rescan particle owners"
        );
        vm.accept_presentation(&session, vm.presentation_request())
            .unwrap();
        models::drive(&mut vm, &mut session, false);
        assert!(matches!(session.view().interaction,
            Some(crate::InteractionView::Dialogue(d)) if d.text()=="middle frame accepted"));
        dialogue(&mut vm, &mut session);
        assert!(
            !vm.particles.has_active(),
            "owner retirement and stage replacement clear paused and live emissions at the same time"
        );
    }
}
#[test]
fn restored_paused_particle_owner_retires_without_advancing_time() {
    use aside_vm::snapshot::SnapshotBudget;
    let code = r#"fn opening() {
        let old=stack();old.remove();
        let p=particles("spark.png");stage.replace(p);
        let owner=stack();
        let flight=emit(p,owner:owner);pause(flight);
        narrator("save");
        owner.remove();
        let mut cancelled=false;
        try {await flight;} catch error {cancelled=true;}
        if !cancelled {throw "restored owner survived";}
        narrator("cancelled");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        assert!(!vm.activities_need_time());
        let program = vm.vm.program().clone();
        let mut capture = vm.snapshot_begin(&session).unwrap();
        while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let bytes = capture.finish().unwrap().encode().unwrap();
        let mut image = snapshot::SessionSnapshot::decode(&bytes).unwrap();
        let mut restore = SessionVm::restore_begin(
            &mut image,
            program,
            VmLimits::default(),
            SessionIdentity {
                session_id: SessionId(91),
                epoch: Epoch(12),
            },
            crate::ReadProgress::default(),
            None,
            None,
        )
        .unwrap();
        while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let (mut session, mut vm) = restore.finish().unwrap();
        dialogue(&mut vm, &mut session);
        assert!(matches!(session.view().interaction,
            Some(crate::InteractionView::Dialogue(d)) if d.text()=="cancelled"));
        assert!(!vm.particles.has_active());
        assert_eq!(batch(&vm).samples().count(), 0);
    }
}

#[test]
fn particles_source_asb_seed_pause_overlap_and_retirement() {
    let code = r#"fn dynamic(value) {value} fn opening() {
        let snow = particles("snow.png", seed: 42, width: 100, height: 20);
        stage.replace(snow);
        let mut errors = 0;
        try { emit(snow, rate: -1); } catch e { errors += 1; }
        try { emit(snow, size: 1, size_spread: 2); } catch e { errors += 1; }
        try { dynamic(snow).seed = 2; } catch e { errors += 1; }
        try { emit(snow, x:dynamic("invalid")); } catch e { errors += 1; }
        if errors != 4 { throw "configuration"; }
        let first = emit(snow, burst:0, rate:40, lifetime:5);
        let consumer = spawn(|| await first); consumer.cancel();
        narrator("running");
        pause(first);
        narrator("paused");
        resume(first);
        narrator("resumed");
        let second = emit(snow, burst:0, rate:40, lifetime:5, duration:1);
        narrator("overlapping");
        stop(first);
        if await first != "Stopped" || await first != "Stopped" { throw "stop first"; }
        if await second != "Completed" { throw "finite"; }
        let infinite = emit(snow, burst:0, rate:40);
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
        assert_eq!(
            batch(&vm).batches[0].sequence,
            1,
            "invalid parameters do not consume the sequence"
        );
        dialogue(&mut vm, &mut session);
        assert_eq!(vm.activities_poll_after(), None);
        activities::advance_clock(&mut vm, &session, 50., 1);
        assert_eq!(batch(&vm).samples().collect::<Vec<_>>(), initial);
        dialogue(&mut vm, &mut session);
        activities::advance_clock(&mut vm, &session, 51., 1);
        assert_eq!(batch(&vm).samples().count(), 80);
        dialogue(&mut vm, &mut session);
        activities::advance_clock(&mut vm, &session, 52., 1);
        assert_eq!(batch(&vm).batches.len(), 2);
        assert_eq!(batch(&vm).samples().count(), 160);
        assert_ne!(
            batch(&vm).batches[0].config.seed,
            batch(&vm).batches[1].config.seed
        );
        dialogue(&mut vm, &mut session);
        assert_eq!(
            batch(&vm).batches.len(),
            1,
            "stopping first leaves second visible"
        );
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
            let snow = root.add(particles("snow.png", seed: i));
            emit(snow, burst:0, rate:10000, lifetime:2);
        }
        run(tween(root, x: 10, duration: 2000000));
        narrator("running");
    }"#;
    let (mut session, mut vm) = models::prepared(code, false);
    models::drive(&mut vm, &mut session, false);
    activities::advance_clock(&mut vm, &session, 1., 1);
    let before = vm.vm.heap().stats();
    let config = batch(&vm).batches;
    let started = std::time::Instant::now();
    for step in 1..=120 {
        activities::advance_clock(&mut vm, &session, 1. + f64::from(step) / 60., 1);
    }
    assert_eq!(
        vm.vm.heap().stats().chunk_allocations,
        before.chunk_allocations
    );
    assert_eq!(config, batch(&vm).batches);
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
        include_str!("../../../../../examples/author_particles/src/main.aside"),
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
        let snow = particles("snow.png");
        stage.replace(snow);
        emit(snow, burst:0, rate:4, lifetime:1, duration:1, owner:owner)
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
        let snow=particles("snow.png"); stage.replace(snow);
        let first = emit(snow, burst:0, rate:4, lifetime:0.5, duration:0.25);
        let second = emit(snow, burst:0, rate:4, lifetime:0.5, duration:0.25);
        let third = emit(snow, burst:0, rate:4, lifetime:0.5, duration:0.25);
        stop(second);
        if await second != "Stopped" { throw "stop result"; }
        if await first != "Completed" || await third != "Completed" { throw "completion"; }
        stage.replace(text("done"));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        let stop = loop {
            match vm
                .step(
                    &mut session,
                    SessionBudget::Exact(SliceBudget::default()),
                    || 0,
                )
                .unwrap()
            {
                SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                    let package = vm.vm.program().package();
                    if package.symbols[package.natives[ticket.native.index()].symbol.index()]
                        == "reel::stop"
                    {
                        break ticket;
                    }
                    vm.dispatch(&mut session, ticket, "en", NativeBudget::Exact(32))
                        .unwrap();
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
        vm.dispatch(&mut session, stop, "en", NativeBudget::Exact(32))
            .unwrap();
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

#[test]
fn particle_scope_and_owner_cancel_only_their_batches() {
    let code = r#"fn opening() {
        let p=particles("spark.png", seed:42); stage.replace(p);
        let owner=stack();
        let a=emit(p,burst:2,lifetime:1,owner:owner);
        let b=emit(p,burst:3,lifetime:1,owner:p);
        scope(body: || { emit(p,burst:7,lifetime:1); });
        owner.remove();
        let mut cancelled=false;
        try { await a; } catch e { cancelled=true; }
        if !cancelled { throw "owner survived"; }
        narrator("survivor");
        finish(b);
        if await b != "Completed" || await b != "Completed" { throw "terminal"; }
        stage.replace(text("done"));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        assert_eq!(batch(&vm).samples().count(), 3);
        assert_eq!(batch(&vm).batches.len(), 1);
        dialogue(&mut vm, &mut session);
        collect(&mut vm);
        assert!(!vm.particles.has_active());
        assert_eq!(vm.particles.projection_pool().usage().bytes, 0);
        assert!(
            vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("done".into()))
        );
    }
}

#[test]
fn particle_snapshot_restores_overlap_pause_and_next_seed() {
    use aside_vm::snapshot::SnapshotBudget;
    let code = r#"fn opening() {
        let p=particles("spark.png", seed:42,width:100,height:30); stage.replace(p);
        let a=emit(p,burst:80,lifetime:1,lifetime_spread:0.5,size_end:0);
        let b=emit(p,burst:3,rate:10,duration:0.5,lifetime:2,velocity_x:100);
        pause(b);
        narrator("save");
        p.width=300;
        emit(p,burst:11,lifetime:1,lifetime_spread:0.2,x:-2.5,y:12);
        narrator("third");
        stop(a); resume(b);
        if await b != "Completed" { throw "restored terminal"; }
        stage.replace(text("done"));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        let partial = vm.advance_time(&session, 0.2, 1).unwrap();
        assert!(partial.pending);
        assert!(
            vm.snapshot_begin(&session).is_err(),
            "capture waits for complete particle maintenance"
        );
        activities::advance_clock(&mut vm, &session, 0.2, 1);
        let program = vm.vm.program().clone();
        let mut capture = vm.snapshot_begin(&session).unwrap();
        while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let bytes = capture.finish().unwrap().encode().unwrap();
        let mut image = snapshot::SessionSnapshot::decode(&bytes).unwrap();
        let mut restore = SessionVm::restore_begin(
            &mut image,
            program,
            VmLimits::default(),
            SessionIdentity {
                session_id: SessionId(91),
                epoch: Epoch(12),
            },
            crate::ReadProgress::default(),
            None,
            None,
        )
        .unwrap();
        while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let (mut rs, mut rv) = restore.finish().unwrap();
        assert_eq!(
            batch(&vm).samples().collect::<Vec<_>>(),
            batch(&rv).samples().collect::<Vec<_>>()
        );
        for (s, v) in [(&mut session, &mut vm), (&mut rs, &mut rv)] {
            activities::advance_clock(v, s, 0.5, 1);
            dialogue(v, s);
            assert_eq!(batch(v).batches.len(), 3);
            assert_eq!(batch(v).batches[2].sequence, 3);
            assert_eq!(batch(v).batches[2].frame.extent, [300., 30.]);
            assert_eq!(batch(v).batches[2].frame.origin, [-2.5, 12.]);
        }
        assert_eq!(
            batch(&vm).samples().collect::<Vec<_>>(),
            batch(&rv).samples().collect::<Vec<_>>()
        );
        for (s, v) in [(&mut session, &mut vm), (&mut rs, &mut rv)] {
            dialogue(v, s);
            activities::advance_clock(v, s, 3., 1);
            models::drive(v, s, false);
            assert!(
                v.stage()
                    .nodes()
                    .any(|n| n.kind == NodeKind::Text("done".into()))
            );
        }
    }
}

#[test]
fn particle_projection_lease_outlives_vm_and_drops_on_consumer_thread() {
    let (mut session, mut vm) = models::prepared(
        r#"fn opening() {
        let p=particles("spark.png"); stage.replace(p);
        emit(p,burst:10,lifetime:1); narrator("live");
    }"#,
        true,
    );
    models::drive(&mut vm, &mut session, false);
    let pool = vm.particles.projection_pool().clone();
    let old = batch(&vm);
    let retained = pool.usage().bytes;
    assert!(retained > 0);
    activities::advance_clock(&mut vm, &session, 2., 1);
    assert!(batch(&vm).batches.is_empty());
    assert_eq!(
        pool.usage().bytes,
        retained,
        "old view remains charged after death"
    );
    drop(vm);
    assert_eq!(
        pool.usage().bytes,
        retained,
        "VM drop must not refund the old view"
    );
    std::thread::spawn(move || {
        assert_eq!(
            old.samples().count(),
            10,
            "immutable old view keeps its original time"
        );
        drop(old);
    })
    .join()
    .unwrap();
    assert_eq!(pool.usage().bytes, 0);
    assert_eq!(pool.usage().tickets, 0);
}

#[test]
fn particle_failed_admission_keeps_old_batch_and_sequence() {
    use reel_core::budget::{BudgetCategory, BudgetPool, QueueLimits};
    let code = r#"fn opening() {
        let p=particles("spark.png",seed:77); stage.replace(p);
        emit(p,burst:3,lifetime:10);
        narrator("first");
        let mut failed=false;
        try { emit(p,burst:9); } catch e { failed=true; }
        if !failed { throw "quota must reject"; }
        narrator("failed");
        emit(p,burst:2,lifetime:10);
        narrator("retry");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        let old = batch(&vm);
        let pool = vm.particles.projection_pool().clone();
        vm.particles.set_projection_pool(BudgetPool::new(
            BudgetCategory::CpuOutput,
            QueueLimits {
                max_requests: 0,
                max_bytes: 0,
            },
        ));
        dialogue(&mut vm, &mut session);
        assert_eq!(batch(&vm).batches, old.batches);
        assert_eq!(batch(&vm).samples().count(), 3);
        vm.particles.set_projection_pool(pool);
        dialogue(&mut vm, &mut session);
        assert_eq!(batch(&vm).batches.len(), 2);
        assert_eq!(
            batch(&vm).batches[1].sequence,
            2,
            "failed emit does not consume seed sequence"
        );
    }
}

#[test]
#[ignore = "release CPU and separately charged VM/projection residency probe"]
fn particle_repeated_small_emissions_cost_and_retirement() {
    use std::{
        collections::VecDeque,
        time::{Duration, Instant},
    };
    let code = r#"fn opening() {
        let p=particles("spark.png",seed:42); stage.replace(p);
        emit(p,burst:1000,lifetime:0.4,lifetime_spread:0.15,size_end:0);
        for vx in [-300,-100,100,300] {
            emit(p,burst:0,rate:120,duration:0.25,lifetime:0.4,
                lifetime_spread:0.15,velocity_x:vx,velocity_y:-200,
                acceleration_y:800,size_end:0);
        }
        let rng=random(seed:19);
        let mut i=0;
        while i<1200 {
            if rng.float()<0.25 {
                emit(p,lifetime:0.4,lifetime_spread:0.15,
                    velocity_y:100,acceleration_y:800,size_end:0);
            }
            i+=1;
            await run(delay(0.008));
        }
        narrator("drain");
    }"#;
    for retained_frames in [0, 8] {
        let (mut session, mut vm) = models::prepared(code, true);
        models::drive(&mut vm, &mut session, false);
        let mut retained = VecDeque::new();
        let mut peak_live = 0;
        let mut samples = 0;
        let mut checksum = 0.;
        let mut advance_cpu = Duration::ZERO;
        let mut script_cpu = Duration::ZERO;
        let mut sample_cpu = Duration::ZERO;
        for frame in 1..=1300 {
            let started = Instant::now();
            activities::advance_clock(&mut vm, &session, f64::from(frame) * 0.01, 8);
            advance_cpu += started.elapsed();
            let started = Instant::now();
            models::drive(&mut vm, &mut session, false);
            script_cpu += started.elapsed();
            let view = batch(&vm);
            peak_live = peak_live.max(view.batches.len());
            let started = Instant::now();
            for sprite in view.samples() {
                samples += 1;
                checksum += sprite.position[0] + sprite.position[1] + sprite.size;
            }
            sample_cpu += started.elapsed();
            if retained_frames > 0 {
                retained.push_back(view);
                if retained.len() > retained_frames {
                    retained.pop_front();
                }
            }
        }
        assert!(checksum.is_finite());
        assert!(
            session.view().interaction.is_some(),
            "producer loop reached drain dialogue"
        );
        assert!(!vm.particles.has_active());
        assert!(batch(&vm).batches.is_empty());
        retained.clear();
        let projection = vm.particles.projection_pool().usage();
        assert_eq!(
            projection.bytes, 0,
            "all dead batch arrays actually dropped"
        );
        let before_gc = vm.vm.heap().stats().charged_bytes;
        collect(&mut vm);
        let after_gc = vm.vm.heap().stats().charged_bytes;
        assert_eq!(vm.stage().nodes().count(), 1);
        eprintln!(
            "particle repeated: retained_frames={retained_frames} frames=1300 peak_live_batches={peak_live} samples={samples} advance_cpu={advance_cpu:?} script_cpu={script_cpu:?} sample_cpu={sample_cpu:?} VM_before_gc={before_gc} VM_after_gc={after_gc} projection_peak={} projection_final={} (charged bytes; not RSS)",
            projection.peak_bytes, projection.bytes
        );
    }
}

#[test]
fn particle_restore_keeps_paint_order_after_emit_during_a_clock_slice() {
    use aside_vm::snapshot::SnapshotBudget;
    let code = r#"fn opening() {
        let p=particles("spark.png",seed:99); stage.replace(p);
        emit(p,burst:3,lifetime:2,velocity_x:100);
        emit(p,burst:4,lifetime:2,velocity_x:-100);
        emit(p,burst:5,lifetime:2,velocity_y:200);
        narrator("resumed");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        let mut emissions = 0;
        let third = loop {
            match vm
                .step(
                    &mut session,
                    SessionBudget::Exact(SliceBudget::default()),
                    || 0,
                )
                .unwrap()
            {
                SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                    let package = vm.vm.program().package();
                    if package.symbols[package.natives[ticket.native.index()].symbol.index()]
                        == "reel::emit"
                    {
                        emissions += 1;
                        if emissions == 3 {
                            break ticket;
                        }
                    }
                    vm.dispatch(&mut session, ticket, "en", NativeBudget::Exact(32))
                        .unwrap();
                }
                SessionStep::Vm(RunOutcome::Fault(id)) => {
                    panic!("{:?}", vm.vm.delivery_fault(id).unwrap())
                }
                SessionStep::Idle | SessionStep::Vm(RunOutcome::Waiting(_)) => {
                    panic!("third emission not reached")
                }
                _ => {}
            }
        };
        assert!(vm.advance_time(&session, 0.2, 1).unwrap().pending);
        vm.dispatch(&mut session, third, "en", NativeBudget::Exact(1))
            .unwrap();
        activities::advance_clock(&mut vm, &session, 0.2, 1);
        let expected = batch(&vm).samples().collect::<Vec<_>>();
        let program = vm.vm.program().clone();
        let mut capture = vm.snapshot_begin(&session).unwrap();
        while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let mut image = capture.finish().unwrap();
        let mut restore = SessionVm::restore_begin(
            &mut image,
            program,
            VmLimits::default(),
            SessionIdentity {
                session_id: SessionId(92),
                epoch: Epoch(13),
            },
            crate::ReadProgress::default(),
            None,
            None,
        )
        .unwrap();
        while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let (mut session, mut restored) = restore.finish().unwrap();
        assert_eq!(
            batch(&restored)
                .batches
                .iter()
                .map(|b| b.sequence)
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(batch(&restored).samples().collect::<Vec<_>>(), expected);
        models::drive(&mut restored, &mut session, false);
        assert!(session.view().interaction.is_some());
        assert_eq!(
            batch(&restored).batches.len(),
            3,
            "ready native continuation does not emit again"
        );
    }
}
