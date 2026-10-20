//! Real source/ASB editing changes published geometry without replacing its root.
use reel_core::{
    budget::{BudgetCategory, BudgetPool, QueueLimits},
    identity::{FrameId, SurfaceEpoch},
};
use reel_engine::{
    Epoch, Session, SessionId,
    aside::{RunOutcome, SliceBudget, VmLimits, source::CompiledStory},
};
use reel_player::aside_session::{AsideSessionUi, StoryProgress};
use reel_ui::{
    app::{GeometryLimits, RenderRoot, RenderRootOptions, WindowSizePolicy},
    bridge::{
        BridgeLimits, NoticeResult, PresentationBridge, PresentationNotice, PresentationOutcome,
    },
    core::{DefaultProperties, Widget},
    dpi::PhysicalSize,
    frame::{BuildLimits, FrameBuild, FrameDraw, FrameInfo, FrameOp, StepBudget},
    peniko::Brush,
    resources::ResourceSnapshot,
};
use std::sync::Arc;

#[test]
fn children_compose_owned_subtrees_without_duplicate_mounting() {
    let source = r#"fn component(children) { column(children: children) }
    fn main() {
        let outside_child = text("outside");
        let outside = stack(children: [outside_child]);
        let old = stack();
        stage.replace(old);
        let retired = text("retired");
        retired.remove();
        let mut rejected = 0;
        try { column { outside_child; } } catch e { rejected += 1; }
        try { column { outside; outside; } } catch e { rejected += 1; }
        try { column { old; } } catch e { rejected += 1; }
        try { column { retired; } } catch e { rejected += 1; }
        if rejected != 4 { throw "ownership validation"; }
        let constructor = component;
        let result = constructor {
            let panel = stack();
            let control = button("control");
            control.add(text("caption"));
            if true { control.add(text("conditional")); }
            panel.add(control);
            panel;
            for name in ["one", "two"] {
                let card = stack();
                card.add(text(name));
                card;
            }
            // A failed construction above left this detached component intact.
            row(children: [outside_child, outside]);
        };
        stage.replace(result);
        narrator("composed");
        result.clear();
        result.add(text("replacement"));
        narrator("cleared");
    }"#;
    let checked = reel_cli::check(source, "composition.aside").unwrap();
    let bytes = reel_cli::build(&checked, Some("main::main")).unwrap().bytes;
    for story in [
        reel_engine::aside::source::compile_story(source, "main", 1, "main").unwrap(),
        reel_cli::decode_story(&bytes).unwrap(),
    ] {
        let mut session = Session::new(SessionId(1), Epoch(1)).unwrap();
        let mut vm =
            reel_engine::aside::SessionVm::load(&session, story.program, VmLimits::default())
                .unwrap();
        for function in [story.initializer, story.entry] {
            vm.start(&session, function).unwrap();
            loop {
                match vm
                    .step(
                        &mut session,
                        reel_engine::aside::SessionBudget::Exact(SliceBudget::default()),
                        || 0,
                    )
                    .unwrap()
                {
                    reel_engine::aside::SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                        if let Err(error) = vm.dispatch(&mut session, ticket, "en", 1024) {
                            vm.reject_native(&session, ticket, error).unwrap();
                        }
                    }
                    reel_engine::aside::SessionStep::Vm(RunOutcome::Returned(_)) => {
                        vm.discard_delivery(&mut session).unwrap();
                        break;
                    }
                    reel_engine::aside::SessionStep::Vm(RunOutcome::Waiting(_)) => break,
                    reel_engine::aside::SessionStep::Vm(RunOutcome::Fault(id)) => {
                        panic!("{:?}", vm.vm().delivery_fault(id).unwrap())
                    }
                    _ => {}
                }
            }
        }
        vm.begin_collection().unwrap();
        while !vm
            .gc_step(reel_engine::aside::GcBudget {
                units: 4096,
                ..Default::default()
            })
            .unwrap()
            .cycle_complete
        {}
        vm.sync_stage(&session).unwrap();
        let root = vm.stage().root().unwrap();
        assert_eq!(vm.stage().node(root).unwrap().children.len(), 4);
        let mut pending = vec![root];
        let mut labels = Vec::new();
        while let Some(id) = pending.pop() {
            let node = vm.stage().node(id).unwrap();
            if let reel_ui::stage::NodeKind::Text(text) = &node.kind {
                labels.push(text.to_string());
            }
            for child in node.children.iter().rev() {
                assert_eq!(vm.stage().node(*child).unwrap().parent, Some(id));
                pending.push(*child);
            }
        }
        assert_eq!(labels, ["caption", "conditional", "one", "two", "outside"]);
        let interaction = session.view().interaction.unwrap().id();
        for _ in 0..2 {
            session
                .handle_local_input(
                    session.root_owner(),
                    reel_engine::InputAction::Advance(interaction),
                )
                .unwrap();
        }
        loop {
            match vm
                .step(
                    &mut session,
                    reel_engine::aside::SessionBudget::Exact(SliceBudget::default()),
                    || 0,
                )
                .unwrap()
            {
                reel_engine::aside::SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                    vm.dispatch(&mut session, ticket, "en", 1024).unwrap();
                }
                reel_engine::aside::SessionStep::Vm(RunOutcome::Waiting(_)) => break,
                reel_engine::aside::SessionStep::Vm(RunOutcome::Fault(id)) => {
                    panic!("{:?}", vm.vm().delivery_fault(id).unwrap())
                }
                _ => {}
            }
        }
        vm.sync_stage(&session).unwrap();
        assert_eq!(vm.stage().root(), Some(root));
        assert_eq!(
            vm.stage().nodes().count(),
            2,
            "clear retires every composed descendant"
        );
    }
}

const SOURCE: &str = r##"fn main() {
    let red = stack(width: 20, height: 20, background: "#ff0000");
    let blue = stack(width: 20, height: 20, background: "#0000ff");
    let root = row(x: 10, y: 10) { red; blue; };
    stage.replace(root);
    await run(tween(root, duration: 1));
    root.add(stack(width: 20, height: 20, background: "#00ff00"), index: 1);
    await run(tween(root, duration: 1));
    red.remove();
    await run(tween(root, duration: 1));
    root.clear();
    await run(tween(root, duration: 1));
    root.add(stack(width: 20, height: 20, background: "#ffff00"));
    await run(tween(root, duration: 1));
}"##;

fn wait(driver: &mut AsideSessionUi) {
    for _ in 0..10000 {
        match driver.tick(SliceBudget::default(), 256, || 0).unwrap() {
            StoryProgress::Vm(RunOutcome::Waiting(_)) if !driver.activities_pending() => return,
            StoryProgress::Faulted(_) => panic!("{:?}", driver.fault().unwrap()),
            StoryProgress::HostFailed(_) => panic!("{:?}", driver.host_failure()),
            StoryProgress::Finished => panic!("ended before expected wait"),
            _ => {}
        }
    }
    panic!("expected animation wait");
}
type Fill = ([u8; 4], [f64; 6]);
fn frames(story: CompiledStory) -> Vec<Vec<Fill>> {
    let mut driver = AsideSessionUi::from_compiled(
        Session::new(SessionId(1), Epoch(1)).unwrap(),
        story,
        "en",
        VmLimits::default(),
    )
    .unwrap();
    wait(&mut driver);
    let node_id = driver.adapter().stage().root();
    let widget = driver.widget().prepare();
    let root_id = widget.id();
    let root = RenderRoot::new(
        widget,
        |_| {},
        RenderRootOptions {
            default_properties: Arc::new(DefaultProperties::new()),
            use_system_fonts: false,
            size_policy: WindowSizePolicy::User,
            size: PhysicalSize::new(640, 420),
            scale_factor: 1.,
            default_font: None,
        },
    );
    let mut bridge = PresentationBridge::new(
        root,
        SurfaceEpoch(1),
        BridgeLimits {
            geometry: GeometryLimits { max_bytes: 1 << 20 },
            max_candidates: 4,
            max_bridge_bytes: 64 << 10,
            max_snapshot_bytes: 8 << 20,
        },
    )
    .unwrap();
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 32,
            max_bytes: 32 << 20,
        },
    );
    let mut frames = Vec::new();
    for second in 0..5 {
        if second > 0 {
            wait(&mut driver);
        }
        assert_eq!(driver.adapter().stage().root(), node_id);
        driver.sync(&mut bridge, root_id).unwrap();
        let (recording, receipt) = driver
            .record(&mut bridge, root_id, FrameId(second + 1))
            .unwrap()
            .expect("settled frame");
        let key = recording.key();

        let mut build = FrameBuild::new(
            recording,
            ResourceSnapshot::capture(std::iter::empty(), &pool).unwrap(),
            FrameInfo {
                logical_size: [640., 420.],
                physical_size: [640, 420],
            },
            BuildLimits { max_bytes: 8 << 20 },
            &pool,
        )
        .unwrap();
        while !build
            .step(StepBudget {
                max_commands: 64,
                max_units: 8192,
                max_bytes: 64 << 10,
            })
            .unwrap()
            .complete
        {}
        let frame = build.finish().unwrap();
        let fills = frame
            .commands()
            .iter()
            .filter_map(|command| match &command.op {
                FrameOp::Draw(FrameDraw::Fill {
                    transform,
                    brush: Brush::Solid(color),
                    ..
                }) if [
                    [255, 0, 0, 255],
                    [0, 0, 255, 255],
                    [0, 255, 0, 255],
                    [255, 255, 0, 255],
                ]
                .contains(&color.to_rgba8().to_u8_array()) =>
                {
                    Some((color.to_rgba8().to_u8_array(), transform.as_coeffs()))
                }
                _ => None,
            })
            .collect();
        frames.push(fills);
        assert_eq!(
            bridge.resolve(PresentationNotice::new(
                key,
                SurfaceEpoch(1),
                PresentationOutcome::Accepted(FrameId(second + 1))
            )),
            NoticeResult::Accepted
        );
        driver.presented(&bridge, &receipt).unwrap();
        assert!(driver.advance_time((second + 1) as f64, 64).unwrap().woke);
    }
    for _ in 0..10000 {
        match driver.tick(SliceBudget::default(), 256, || 0).unwrap() {
            StoryProgress::Finished => return frames,
            StoryProgress::Faulted(_) | StoryProgress::HostFailed(_) => panic!("ending failed"),
            _ => {}
        }
    }
    panic!("story did not finish");
}

#[test]
fn source_and_asb_publish_the_same_dynamic_child_order_and_removal() {
    let checked = reel_cli::check(SOURCE, "dynamic-stage.aside").unwrap();
    let built = reel_cli::build(&checked, Some("main::main")).unwrap();
    let direct = frames(reel_cli::into_story(checked, "main::main").unwrap());
    assert_eq!(
        direct,
        frames(reel_cli::decode_story(&built.bytes).unwrap())
    );
    let red = [255, 0, 0, 255];
    let green = [0, 255, 0, 255];
    let blue = [0, 0, 255, 255];
    let yellow = [255, 255, 0, 255];
    for (actual, expected) in direct.iter().zip([
        vec![red, blue],
        vec![red, green, blue],
        vec![green, blue],
        vec![],
        vec![yellow],
    ]) {
        assert_eq!(actual.iter().map(|v| v.0).collect::<Vec<_>>(), expected);
        for (i, (_, transform)) in actual.iter().enumerate() {
            assert_eq!(*transform, [1., 0., 0., 1., 10. + 20. * i as f64, 10.]);
        }
    }
}
