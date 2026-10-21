use super::*;
use reel_ui::stage::NodeKind;
use std::time::Instant;

fn source(clock: bool, noise: bool) -> String {
    let setup = if noise {
        r#"let frames=["frame-a.png","frame-b.png","frame-c.png","frame-d.png"];
        let owner=stack(width:64,height:64);"#
    } else {
        r#"let frames=["frame-a.png","frame-b.png"];
        let owner=cursor(width:64,height:64);"#
    };
    let body = if clock {
        r#"let timer=clock(interval:0.125,paused:true,owner:owner);
        owner.add(image(source:derive(||frames[timer.ticks%frames.len()]),width:64,height:64));
        stage.replace(owner);resume(timer);"#
    } else if noise {
        r#"let images=[];
        for index in 0..frames.len() {
            let picture=image(frames[index],width:64,height:64,visible:index==0);
            images.append(picture);owner.add(picture);
        }
        stage.replace(owner);
        spawn(|| {let mut frame=0;loop {
            await run(delay(0.125));images[frame].visible=false;
            frame=(frame+1)%frames.len();images[frame].visible=true;
        }},owner:owner);"#
    } else {
        r#"let picture=image(frames[0],width:64,height:64);owner.add(picture);
        stage.replace(owner);
        spawn(|| {loop {
            await run(delay(0.125));picture.source=frames[1];
            await run(delay(0.125));picture.source=frames[0];
        }},owner:owner);"#
    };
    format!("fn opening() {{{setup}{body}narrator(\"running\");}}")
        .replace("frames.len()", if noise { "4" } else { "2" })
}

#[derive(Debug, Default)]
struct Work {
    slices: u64,
    native_calls: u64,
    activity_units: u64,
}

fn settle(vm: &mut SessionVm, session: &mut Session, work: &mut Work) {
    for _ in 0..100_000 {
        work.slices += 1;
        match vm
            .step(
                session,
                SessionBudget::Exact(SliceBudget {
                    instructions: 1,
                    ..Default::default()
                }),
                || 0,
            )
            .unwrap()
        {
            SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                work.native_calls += 1;
                vm.dispatch(session, ticket, "en", 32).unwrap();
            }
            SessionStep::Vm(RunOutcome::Returned(_)) => vm.discard_delivery(session).unwrap(),
            SessionStep::Vm(RunOutcome::Fault(id)) => {
                panic!("{:?}", vm.vm.delivery_fault(id).unwrap())
            }
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
    panic!("animation did not settle");
}

fn shown(vm: &SessionVm) -> String {
    vm.stage()
        .nodes()
        .find_map(|node| {
            if node.props.visible {
                if let NodeKind::Image { source, .. } = &node.kind {
                    return Some(source.key().to_owned());
                }
            }
            None
        })
        .unwrap()
}

fn advance_dialogue(vm: &mut SessionVm, session: &mut Session) {
    let id = session.view().interaction.unwrap().id();
    for _ in 0..2 {
        session
            .handle_local_input(session.root_owner(), InputAction::Advance(id))
            .unwrap();
    }
    models::drive(vm, session, false);
}

#[test]
fn frame_animation_example_runs_from_source_and_asb() {
    let example = include_str!("../../../../../examples/author_frame_animation.aside");
    for encoded in [false, true] {
        let code = format!("{example}\nfn opening() {{ main(); }}");
        let (mut session, mut vm) = models::prepared(&code, encoded);
        models::drive(&mut vm, &mut session, false);
        vm.accept_presentation(&session, vm.presentation_request())
            .unwrap();
        models::drive(&mut vm, &mut session, false);
        for (time, expected) in [
            (0.125, "assets/sun.jpg"),
            (2., "assets/panels.png"),
            (3., "assets/panels.png"),
            (3.125, "assets/sun.jpg"),
            (5., "assets/panels.png"),
        ] {
            activities::advance_clock(&mut vm, &session, time, 1);
            models::drive(&mut vm, &mut session, false);
            assert_eq!(shown(&vm), expected);
        }
        assert!(!vm.activities_need_time());
    }
}

#[test]
fn frame_animation_coalesces_missed_frames_and_reuses_the_image_node() {
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(&source(true, true), encoded);
        models::drive(&mut vm, &mut session, false);
        let image = vm
            .stage()
            .nodes()
            .find(|n| matches!(n.kind, NodeKind::Image { .. }))
            .unwrap()
            .id;
        for (time, expected) in [
            (0.124, "a"),
            (0.125, "b"),
            (0.249, "b"),
            (0.25, "c"),
            (0.875, "d"),
            (1., "a"),
        ] {
            activities::advance_clock(&mut vm, &session, time, 1);
            models::drive(&mut vm, &mut session, false);
            assert_eq!(shown(&vm), format!("frame-{expected}.png"));
            assert!(vm.stage().node(image).is_some());
            assert_eq!(
                vm.stage()
                    .nodes()
                    .filter(|n| matches!(n.kind, NodeKind::Image { .. }))
                    .count(),
                1
            );
        }
    }
}

#[test]
fn frame_animation_waits_for_presentation_and_preserves_pause_restore_and_retirement() {
    use aside_vm::snapshot::SnapshotBudget;
    let code = r#"fn opening() {
        let frames=["frame-a.png","frame-b.png"];
        let owner=stack(width:64,height:64);
        let timer=clock(interval:0.125,paused:true,owner:owner);
        owner.add(image(source:derive(||frames[timer.ticks%frames.len()]),width:64,height:64));
        stage.replace(owner);
        await stage.presented(owner:owner);resume(timer);
        narrator("started");pause(timer);narrator("paused");
        resume(timer);narrator("resumed");stop(timer);narrator("stopped");
        let retiring=clock(interval:0.125,owner:owner);owner.remove();
        let mut cancelled=false;try {await retiring;}catch error {cancelled=true;}
        if !cancelled {throw "clock owner survived retirement";}
        narrator("done");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        let node = vm
            .stage()
            .nodes()
            .find(|n| matches!(n.kind, NodeKind::Image { .. }))
            .unwrap()
            .id;
        activities::advance_clock(&mut vm, &session, 5., 1);
        models::drive(&mut vm, &mut session, false);
        assert_eq!(shown(&vm), "frame-a.png");
        assert!(!vm.activities_need_time());
        vm.accept_presentation(&session, vm.presentation_request())
            .unwrap();
        models::drive(&mut vm, &mut session, false);
        activities::advance_clock(&mut vm, &session, 5.375, 1);
        models::drive(&mut vm, &mut session, false);
        assert_eq!(shown(&vm), "frame-b.png");
        assert!(
            vm.stage().node(node).is_some(),
            "the image node must retain its identity"
        );
        advance_dialogue(&mut vm, &mut session);
        assert!(!vm.activities_need_time());
        let program = vm.vm.program().clone();
        let mut capture = vm.snapshot_begin(&session).unwrap();
        while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let mut image =
            snapshot::SessionSnapshot::decode(&capture.finish().unwrap().encode().unwrap())
                .unwrap();
        let mut restore = SessionVm::restore_begin(
            &mut image,
            program,
            VmLimits::default(),
            SessionIdentity {
                session_id: SessionId(42),
                epoch: Epoch(21),
            },
            crate::ReadProgress::default(),
            None,
            None,
        )
        .unwrap();
        while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        (session, vm) = restore.finish().unwrap();
        activities::advance_clock(&mut vm, &session, 50., 1);
        models::drive(&mut vm, &mut session, false);
        assert_eq!(shown(&vm), "frame-b.png");
        advance_dialogue(&mut vm, &mut session);
        activities::advance_clock(&mut vm, &session, 50.125, 1);
        models::drive(&mut vm, &mut session, false);
        assert_eq!(shown(&vm), "frame-a.png");
        advance_dialogue(&mut vm, &mut session);
        activities::advance_clock(&mut vm, &session, 100., 1);
        models::drive(&mut vm, &mut session, false);
        assert_eq!(shown(&vm), "frame-a.png");
        advance_dialogue(&mut vm, &mut session);
        assert!(
            matches!(session.view().interaction,Some(crate::InteractionView::Dialogue(d)) if d.text()=="done")
        );
        assert!(!vm.activities_need_time());
    }
}

#[test]
#[ignore = "release comparison: cargo test --release --locked -p reel-engine --lib frame_animation_cost -- --ignored --nocapture --test-threads=1"]
fn frame_animation_cost() {
    for noise in [false, true] {
        for clock in [false, true] {
            let (mut session, mut vm) = models::prepared(&source(clock, noise), true);
            let before = vm.vm.heap().stats();
            let mut init = Work::default();
            settle(&mut vm, &mut session, &mut init);
            collect(&mut vm);
            let start = vm.vm.heap().stats();
            let mut work = Work::default();
            let began = Instant::now();
            for tick in 1..=6000 {
                work.activity_units += u64::from(
                    activities::advance_clock(&mut vm, &session, f64::from(tick) * 0.125, 64).work,
                );
                settle(&mut vm, &mut session, &mut work);
                // Include regular collection for both variants without collecting
                // every frame or letting the allocating baseline grow unchecked.
                if tick % 60 == 0 {
                    collect(&mut vm);
                }
            }
            let elapsed = began.elapsed();
            assert_eq!(shown(&vm), "frame-a.png");
            let end = vm.vm.heap().stats();
            let idle_start = Instant::now();
            let mut idle = Work::default();
            for _ in 0..6000 {
                idle.activity_units +=
                    u64::from(activities::advance_clock(&mut vm, &session, 750., 64).work);
                settle(&mut vm, &mut session, &mut idle);
            }
            println!(
                "kind={} mode={} init_chunks={} init_work={init:?} chunks={} live={}->{} bytes={}->{} peak={} gc_units={} work={work:?} elapsed_us={} idle={idle:?} idle_us={}",
                if noise { "noise" } else { "cursor" },
                if clock { "clock" } else { "delay" },
                start.chunk_allocations - before.chunk_allocations,
                end.chunk_allocations - start.chunk_allocations,
                start.live_objects,
                end.live_objects,
                start.charged_bytes,
                end.charged_bytes,
                end.peak_charged_bytes,
                end.gc_units - start.gc_units,
                elapsed.as_micros(),
                idle_start.elapsed().as_micros()
            );
        }
    }
}
