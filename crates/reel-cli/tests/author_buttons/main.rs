//! Source and ASB execute the same interactive scene without a window/GPU adapter.
use reel_engine::{
    Epoch, Session, SessionId,
    aside::{SliceBudget, VmLimits, source::CompiledStory},
};
use reel_player::aside_session::{AsideSessionUi, StoryState};
use reel_ui::{bridge::PresentationBridge, core::*, dpi::PhysicalPosition, stage::NodeKind};

#[path = "../support/author.rs"]
mod support;
use support::*;

const SOURCE: &str = include_str!("../../../../examples/author_buttons/src/main.aside");

#[test]
fn conditional_table_tails_configure_input_from_source_and_asb() {
    let source = r#"fn main() {
        for flag in [true, false] {
            let actions=if flag { {accept: {bindings: ["Enter"]}} }
                else { {back: {bindings: ["Escape"]}} };
            if actions.has_own("accept") != flag || actions.has_own("back") == flag {
                throw "wrong conditional input table";
            }
            stage.replace(input(actions: actions) {
                column { text("Conditional input"); }
            });
        }
        stage.replace(stack());
    }"#;
    let checked = reel_cli::check(source, "conditional-input.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for story in [
        reel_cli::into_story(checked, "main::main").unwrap(),
        reel_player::distribution::decode_story(&artifact.bytes).unwrap(),
    ] {
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut driver);
        assert_eq!(driver.state(), StoryState::Finished);
    }
}

#[test]
fn source_and_asb_retire_a_batch_of_live_completion_owners() {
    let source = r#"fn main() {
        let owner=stack(); stage.replace(owner);
        let requests=[];
        for i in 0..1024 {requests.append(completion(owner:owner));}
        for request in requests {
            if !request.pending {throw "live result ended";}
        }
        owner.remove();
        for request in requests {
            if request.pending {throw "retired owner retained result";}
        }
    }"#;
    let checked = reel_cli::check(source, "completion-batch.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for story in [
        reel_engine::aside::source::compile_story(source, "main", 1, "main").unwrap(),
        reel_player::distribution::decode_story(&artifact.bytes).unwrap(),
    ] {
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut driver);
        assert_eq!(driver.state(), StoryState::Finished);
    }
}

#[test]
fn bound_class_method_keeps_its_receiver_after_entry_returns() {
    let source = r#"
    class Counter {
        let mut count: int = 0;
        let caption;
        fn init(self, caption) { self.caption = caption; }
        fn click(self, event) {
            self.count += 1;
            self.caption.value = f"{self.count}";
        }
    }
    fn main() {
        let caption = text("0", y: 150);
        let counter = Counter(caption);
        stage.replace(stack(width: 640, height: 420) {
            caption;
            button("Count", x: 20, y: 20, width: 180, height: 60,
                on_click: counter.click);
        });
    }"#;
    let checked = reel_cli::check(source, "class-callback.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for story in [
        reel_player::distribution::decode_story(&artifact.bytes).unwrap(),
        reel_engine::aside::source::compile_story(source, "main", 1, "main").unwrap(),
    ] {
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut driver);
        assert_eq!(driver.state(), StoryState::Running);
        let (mut bridge, root) = bridge(&driver);
        present(&mut driver, &mut bridge, root, 1);
        for count in 1..=2 {
            click_at(&mut driver, &mut bridge, 50., 50.);
            present(&mut driver, &mut bridge, root, count + 1);
            assert!(
                driver
                    .adapter()
                    .stage()
                    .nodes()
                    .any(|node| { node.kind == NodeKind::Text(count.to_string().into()) })
            );
        }
    }
}

fn run(story: CompiledStory, pointer_type: Option<PointerType>) {
    let mut driver = AsideSessionUi::from_compiled(
        Session::new(SessionId(1), Epoch(1)).unwrap(),
        story,
        "en",
        VmLimits::default(),
    )
    .unwrap();
    settle(&mut driver);
    assert_eq!(
        driver.state(),
        StoryState::Running,
        "main returned, mounted handlers remain usable"
    );
    let (mut bridge, root_id) = bridge(&driver);
    present(&mut driver, &mut bridge, root_id, 1);
    let caption = driver
        .adapter()
        .stage()
        .nodes()
        .find(|node| node.kind == NodeKind::Text("Ready".into()))
        .unwrap()
        .id;
    let moving = driver.adapter().stage().nodes().find(|node| matches!(&node.kind, NodeKind::Button { label: Some(label), .. } if label == "Move")).unwrap().id;
    if let Some(pointer_type) = pointer_type {
        click_with_pointer(&mut driver, &mut bridge, 50., 50., pointer_type);
    } else {
        gamepad_press(
            &mut driver,
            &mut bridge,
            reel_ui::input::GamepadButton::South,
        );
    }
    present(&mut driver, &mut bridge, root_id, 2);
    assert!(!driver.adapter().stage().node(moving).unwrap().props.enabled);
    assert!(driver.activities_need_time());
    driver.advance_time(0.2, 16).unwrap();
    present(&mut driver, &mut bridge, root_id, 3);
    assert_eq!(driver.adapter().stage().node(caption).unwrap().props.x, 60.);
    assert!(driver.advance_time(0.4, 16).unwrap().woke);
    settle(&mut driver);
    present(&mut driver, &mut bridge, root_id, 4);
    let node = driver.adapter().stage().node(caption).unwrap();
    assert_eq!(node.kind, NodeKind::Text("Arrived".into()));
    assert_eq!(node.props.x, 120.);
    assert!(driver.adapter().stage().node(moving).unwrap().props.enabled);
    if let Some(pointer_type) = pointer_type {
        click_with_pointer(&mut driver, &mut bridge, 270., 50., pointer_type);
    } else {
        gamepad_press(
            &mut driver,
            &mut bridge,
            reel_ui::input::GamepadButton::DpadRight,
        );
        gamepad_press(
            &mut driver,
            &mut bridge,
            reel_ui::input::GamepadButton::DpadRight,
        );
        gamepad_press(
            &mut driver,
            &mut bridge,
            reel_ui::input::GamepadButton::South,
        );
    }
    assert_eq!(driver.state(), StoryState::Finished);
    present(&mut driver, &mut bridge, root_id, 5);
    while driver.cleanup_pending() {
        driver
            .cleanup_step(1, SliceBudget::default(), || 0)
            .unwrap();
    }
}
#[test]
fn source_and_asb_keep_buttons_live_then_finish_after_last_handler_retires() {
    let checked = reel_cli::check(SOURCE, "author_buttons.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for pointer_type in [Some(PointerType::Mouse), Some(PointerType::Touch), None] {
        run(
            reel_player::distribution::decode_story(&artifact.bytes).unwrap(),
            pointer_type,
        );
        run(
            reel_engine::aside::source::compile_story(SOURCE, "main", 1, "main").unwrap(),
            pointer_type,
        );
    }
}

mod author_exit {
    //! Author shutdown uses the same cleanup path in source and encoded projects.
    use super::support;

    use reel_engine::{
        Epoch, OwnerLifecycle, Session, SessionId,
        aside::{SliceBudget, VmLimits, source::compile_story},
    };
    use reel_player::{aside_session::AsideSessionUi, story_runtime::StoryRuntime};

    #[test]
    fn source_and_asb_exit_from_story_or_waiting_callback_retire_the_session() {
        for callback in [false, true] {
            let source = if callback {
                r#"fn main() {
                stage.replace(button("Exit", width: 200, height: 80, on_click: |event| {
                    await delay(0.5).run();
                    game.exit();
                    throw "callback continued after exit";
                }));
                await completion();
                throw "main continued after exit";
            }"#
            } else {
                r#"fn main() {
                repeat(delay(60)).run();
                game.exit();
                throw "main continued after exit";
            }"#
            };
            let checked = reel_cli::check(source, "main.aside").unwrap();
            let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
            for story in [
                compile_story(source, "main", 1, "main").unwrap(),
                reel_player::distribution::decode_story(&artifact.bytes).unwrap(),
            ] {
                let mut driver = AsideSessionUi::from_compiled(
                    Session::new(SessionId(1), Epoch(1)).unwrap(),
                    story,
                    "en",
                    VmLimits::default(),
                )
                .unwrap();
                let owner = driver.ui().session().root_owner();
                if callback {
                    support::settle(&mut driver);
                    let (mut bridge, root) = support::bridge(&driver);
                    support::present(&mut driver, &mut bridge, root, 1);
                    support::click_at(&mut driver, &mut bridge, 50., 40.);
                    assert!(!driver.adapter().exit_requested());
                    driver.advance_time(1., 64).unwrap();
                }
                let mut runtime = StoryRuntime::new(driver);
                for _ in 0..10_000 {
                    runtime
                        .tick(
                            SliceBudget {
                                instructions: 3,
                                ..Default::default()
                            },
                            || 1_000_000_000,
                        )
                        .unwrap();
                    if runtime.close_complete() {
                        break;
                    }
                }
                assert!(runtime.driver().adapter().exit_requested());
                assert!(runtime.close_complete());
                assert!(!runtime.needs_tick());
                assert!(runtime.driver().adapter().task().is_none());
                assert_eq!(
                    runtime.driver().ui().session().owner_lifecycle(owner),
                    Some(OwnerLifecycle::Retired)
                );
                assert!(runtime.driver().fault().unwrap().is_none());
            }
        }
    }
}

mod author_pointer {
    //! Source and actual ASB exercise the example through accepted UI input.
    //! Host completion is covered by the engine contract checks; this has no native window.
    use super::support;
    use reel_engine::{Epoch, Session, SessionId, aside::VmLimits};
    use reel_player::aside_session::AsideSessionUi;
    use reel_ui::stage::NodeKind;
    use support::*;

    #[test]
    fn source_and_asb_handle_refusal_and_destination_pointer_input() {
        let checked = reel_cli::check(
            r#"fn main() {
    game.configure(width: 640, height: 360, fonts: ["fonts/body.ttf"]);
    let status = text("Click Move while using a mouse.", x: 24, y: 24);
    let root = stack(width: 640, height: 360);
    root.add(status);
    root.add(
        button(
            "Move pointer", x: 24, y: 100, width: 180, height: 60,
            on_click: |event| {
                let moved = await stage.move_pointer(440, 120, owner: root);
                status.value = if moved {
                    "The pointer moved through ordinary hit testing."
                } else {
                    "Pointer movement is unavailable for the current input or host."
                };
            }
        )
    );
    root.add(
        button(
            "Destination", x: 400, y: 100, width: 180, height: 60,
            on_click: |event| {
                status.value = "Destination clicked.";
            }
        )
    );
    stage.replace(root);
}
"#,
            "author_pointer.aside",
        )
        .unwrap();
        let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
        for story in [
            reel_cli::into_story(checked, "main::main").unwrap(),
            reel_player::distribution::decode_story(&artifact.bytes).unwrap(),
        ] {
            let mut driver = AsideSessionUi::from_compiled(
                Session::new(SessionId(1), Epoch(1)).unwrap(),
                story,
                "en",
                VmLimits::default(),
            )
            .unwrap();
            settle(&mut driver);
            let (mut bridge, root) = bridge(&driver);
            present(&mut driver, &mut bridge, root, 1);
            click_at(&mut driver, &mut bridge, 30., 110.);
            assert!(driver.adapter().stage().nodes().any(|node| node.kind
                == NodeKind::Text(
                    "Pointer movement is unavailable for the current input or host.".into()
                )));
            present(&mut driver, &mut bridge, root, 2);
            click_at(&mut driver, &mut bridge, 440., 120.);
            assert!(
                driver
                    .adapter()
                    .stage()
                    .nodes()
                    .any(|node| node.kind == NodeKind::Text("Destination clicked.".into()))
            );
            driver.cancel().unwrap();
        }
    }
}

mod dynamic_stage {
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
        try { let _ = column { outside_child; }; } catch e { rejected += 1; }
        try { let _ = column { outside; outside; }; } catch e { rejected += 1; }
        try { let _ = column { old; }; } catch e { rejected += 1; }
        try { let _ = column { retired; }; } catch e { rejected += 1; }
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
            reel_player::distribution::decode_story(&bytes).unwrap(),
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
                            if let Err(error) = vm.dispatch(
                                &mut session,
                                ticket,
                                "en",
                                reel_engine::aside::NativeBudget::Exact(1024),
                            ) {
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
                        vm.dispatch(
                            &mut session,
                            ticket,
                            "en",
                            reel_engine::aside::NativeBudget::Exact(1024),
                        )
                        .unwrap();
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
    await tween(root, duration: 1).run();
    root.add(stack(width: 20, height: 20, background: "#00ff00"), index: 1);
    await tween(root, duration: 1).run();
    red.remove();
    await tween(root, duration: 1).run();
    root.clear();
    await tween(root, duration: 1).run();
    root.add(stack(width: 20, height: 20, background: "#ffff00"));
    await tween(root, duration: 1).run();
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
            frames(reel_player::distribution::decode_story(&built.bytes).unwrap())
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
}

mod dialogue;
mod input;
mod modal;
