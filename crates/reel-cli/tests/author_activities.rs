//! Source and ASB use the same real activity clock, stage and frame pipeline.
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

const SOURCE: &str = r##"fn main() {
    let card = stack(width: 20, height: 20, x: 10, y: 10, background: "#ff0000");
    stage.replace(card);
    await run(parallel(
        sequence(tween(card, x: 50, y: 30, duration: 1), tween(card, x: 90, y: 50, duration: 1)),
        sequence(tween(card, rotation: 45, duration: 1), repeat(tween(card, rotation: 90, duration: 0.5), times: 2)),
        tween(card, opacity: 0.5, duration: 2)
    ));
}"##;

fn frames(story: CompiledStory, canvas: Option<[u32; 2]>) -> Vec<[f64; 6]> {
    let mut driver = AsideSessionUi::from_compiled(
        Session::new(SessionId(1), Epoch(1)).unwrap(),
        story,
        "en",
        VmLimits::default(),
    )
    .unwrap();
    for _ in 0..10000 {
        match driver.tick(SliceBudget::default(), 256, || 0).unwrap() {
            StoryProgress::Vm(RunOutcome::Waiting(_)) => break,
            StoryProgress::Faulted(_) => panic!("{:?}", driver.fault().unwrap()),
            StoryProgress::HostFailed(_) => panic!("{:?}", driver.host_failure()),
            _ => {}
        }
    }
    assert!(driver.activities_need_time());
    let widget = driver.widget().prepare();
    let root_id = widget.id();
    let widget = if let Some(size) = canvas {
        reel_ui::widgets::Canvas::new(widget, size)
            .prepare()
            .erased()
    } else {
        widget.erased()
    };
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
    let mut transforms = Vec::new();
    for second in 0..=2 {
        let mut woke = false;
        loop {
            let progress = driver.advance_time(second as f64, 8).unwrap();
            assert!(progress.work <= 8);
            woke |= progress.woke;
            if !progress.pending {
                break;
            }
        }
        assert_eq!(woke, second == 2);
        driver.sync(&mut bridge, root_id).unwrap();
        let (recording, receipt) = driver
            .record(&mut bridge, root_id, FrameId(second as u64 + 1))
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
        let red = frame
            .commands()
            .iter()
            .filter_map(|command| match &command.op {
                FrameOp::Draw(FrameDraw::Fill {
                    transform,
                    brush: Brush::Solid(color),
                    ..
                }) if color.to_rgba8().to_u8_array() == [255, 0, 0, 255] => {
                    Some(transform.as_coeffs())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(red.len(), 1);
        transforms.push(red[0]);
        assert_eq!(
            bridge.resolve(PresentationNotice::new(
                key,
                SurfaceEpoch(1),
                PresentationOutcome::Accepted(FrameId(second as u64 + 1))
            )),
            NoticeResult::Accepted
        );
        driver.presented(&bridge, &receipt).unwrap();
    }
    for _ in 0..10000 {
        if matches!(
            driver.tick(SliceBudget::default(), 256, || 0).unwrap(),
            StoryProgress::Finished
        ) {
            return transforms;
        }
    }
    panic!("await did not resume after final frame");
}

#[test]
fn source_and_asb_animate_the_same_presented_geometry() {
    let checked = reel_cli::check(SOURCE, "activity.aside").unwrap();
    let built = reel_cli::build(&checked, Some("main::main")).unwrap();
    let direct = frames(reel_cli::into_story(checked, "main::main").unwrap(), None);
    let packaged = frames(
        reel_player::distribution::decode_story(&built.bytes).unwrap(),
        None,
    );
    assert_eq!(direct, packaged);
    let c = std::f64::consts::FRAC_1_SQRT_2;
    for (actual, expected) in direct.iter().zip([
        [1., 0., 0., 1., 10., 10.],
        [c, c, -c, c, 50., 30.],
        [0., 1., -1., 0., 90., 50.],
    ]) {
        for (a, b) in actual.iter().zip(expected) {
            assert!((a - b).abs() < 1e-9, "{actual:?}");
        }
    }
}

#[test]
fn fixed_canvas_uses_the_same_composed_frame_with_uniform_scale_and_letterboxing() {
    let checked = reel_cli::check(SOURCE, "activity.aside").unwrap();
    let built = reel_cli::build(&checked, Some("main::main")).unwrap();
    let transforms = frames(
        reel_player::distribution::decode_story(&built.bytes).unwrap(),
        Some([1280, 420]),
    );
    assert_eq!(transforms[0], [0.5, 0., 0., 0.5, 5., 110.]);
    let end = transforms.last().unwrap();
    for (actual, expected) in end.iter().zip([0., 0.5, -0.5, 0., 45., 130.]) {
        assert!((actual - expected).abs() < 1e-9, "{end:?}");
    }
}

#[path = "support/author.rs"]
#[allow(dead_code)]
mod support;

mod author_transitions {
    //! Transition barriers and input ownership through the real accepted CPU stage.
    use super::support;
    use reel_core::identity::{FrameId, SurfaceEpoch};
    use reel_engine::{
        Epoch, Session, SessionId,
        aside::{VmLimits, transitions::TransitionPhase},
    };
    use reel_player::aside_session::AsideSessionUi;
    use reel_ui::bridge::{PresentationNotice, PresentationOutcome};
    use support::*;

    #[test]
    fn transition_freezes_only_accepted_content_and_gates_its_target_from_source_and_asb() {
        let source = r##"
        fn main() {
            let state = model({enabled: true});
            let panel = stack(x: 10, y: 10, width: 120, height: 60,
                enabled: derive(|| state.enabled), background: "#ff0000") {
                button("", width: 100, height: 50,
                    on_click: |event| game.configure(title: "inside"));
            };
            stage.replace(stack(width: 640, height: 420) {
                panel;
                button("", x: 200, y: 10, width: 100, height: 50,
                    on_click: |event| game.configure(title: "outside"));
            });
            game.configure(title: "before");
            stage.transition(target: panel, duration: 0.2, change: || {
                panel.background = "#0000ff";
                game.configure(title: "changing");
            });
            if !panel.enabled || !state.enabled { throw "transition overwrote enabled"; }
            game.configure(title: "completed");
            await completion();
        }
    "##;
        let checked = reel_cli::check(source, "transitions.aside").unwrap();
        let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
        for compiled in [
            reel_cli::into_story(checked, "main::main").unwrap(),
            reel_player::distribution::decode_story(&artifact.bytes).unwrap(),
        ] {
            let mut driver = AsideSessionUi::from_compiled(
                Session::new(SessionId(1), Epoch(1)).unwrap(),
                compiled,
                "en",
                VmLimits::default(),
            )
            .unwrap();
            settle(&mut driver);
            let (mut bridge, root) = bridge(&driver);
            driver.sync(&mut bridge, root).unwrap();
            let (recording, receipt) = driver
                .record(&mut bridge, root, FrameId(1))
                .unwrap()
                .unwrap();
            driver.presented(&bridge, &receipt).unwrap();
            settle(&mut driver);
            assert_eq!(driver.adapter().game_settings().title, "before");
            bridge.resolve(PresentationNotice::new(
                recording.key(),
                SurfaceEpoch(1),
                PresentationOutcome::Skipped,
            ));
            driver.presented(&bridge, &receipt).unwrap();
            settle(&mut driver);
            assert_eq!(driver.adapter().game_settings().title, "before");
            let mut frame = 1;
            for _ in 0..30 {
                frame += 1;
                present(&mut driver, &mut bridge, root, frame);
                settle(&mut driver);
                if driver
                    .adapter()
                    .transition_views()
                    .any(|view| view.phase == TransitionPhase::Playing)
                {
                    break;
                }
            }
            assert_eq!(driver.adapter().game_settings().title, "changing");
            assert!(
                driver
                    .adapter()
                    .transition_views()
                    .any(|view| view.phase == TransitionPhase::Playing)
            );
            click_at(&mut driver, &mut bridge, 50., 30.);
            assert_eq!(driver.adapter().game_settings().title, "changing");
            click_at(&mut driver, &mut bridge, 250., 30.);
            assert_eq!(driver.adapter().game_settings().title, "outside");
            assert!(
                driver
                    .adapter()
                    .stage()
                    .nodes()
                    .all(|node| node.props.enabled)
            );
            while driver.advance_time(1., 64).unwrap().pending {}
            settle(&mut driver);
            assert_eq!(
                driver.adapter().game_settings().title,
                "outside",
                "time alone finished a transition"
            );
            frame += 1;
            present(&mut driver, &mut bridge, root, frame);
            settle(&mut driver);
            assert_eq!(driver.adapter().game_settings().title, "completed");
            frame += 1;
            present(&mut driver, &mut bridge, root, frame);
            click_at(&mut driver, &mut bridge, 50., 30.);
            assert_eq!(driver.adapter().game_settings().title, "inside");
            driver.cancel().unwrap();
        }
    }
}

mod author_scopes {
    //! The creator example starts and retires one effect through presented UI input.
    use super::support;
    use reel_engine::{Epoch, Session, SessionId, aside::VmLimits};
    use reel_player::aside_session::AsideSessionUi;
    use reel_ui::{input::GamepadButton, stage::NodeKind};
    use support::*;

    #[test]
    fn scope_example_start_stop_retires_child_work_from_source_and_asb() {
        let checked = reel_cli::check(
            include_str!("../../../examples/author_scope/src/main.aside"),
            "author_scope.aside",
        )
        .unwrap();
        let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
        for compiled in [
            reel_cli::into_story(checked, "main::main").unwrap(),
            reel_player::distribution::decode_story(&artifact.bytes).unwrap(),
        ] {
            let mut driver = AsideSessionUi::from_compiled(
                Session::new(SessionId(1), Epoch(1)).unwrap(),
                compiled,
                "en",
                VmLimits::default(),
            )
            .unwrap();
            settle(&mut driver);
            let (mut bridge, root) = bridge(&driver);
            present(&mut driver, &mut bridge, root, 1);
            gamepad_press(&mut driver, &mut bridge, GamepadButton::DpadDown);
            gamepad_press(&mut driver, &mut bridge, GamepadButton::South);
            present(&mut driver, &mut bridge, root, 2);
            settle(&mut driver);
            while driver.advance_time(0.21, 16).unwrap().pending {}
            settle(&mut driver);
            assert!(
                driver
                    .adapter()
                    .stage()
                    .nodes()
                    .any(|n| n.kind == NodeKind::Text("Scope ticks: 1".into()))
            );
            assert!(driver.activities_need_time());
            assert!(
                driver
                    .adapter()
                    .stage()
                    .nodes()
                    .any(|n| n.kind == NodeKind::Text("Scoped effect is running".into()))
            );
            present(&mut driver, &mut bridge, root, 3);
            gamepad_press(&mut driver, &mut bridge, GamepadButton::DpadRight);
            gamepad_press(&mut driver, &mut bridge, GamepadButton::South);
            settle(&mut driver);
            assert!(!driver.activities_need_time());
            assert!(
                !driver
                    .adapter()
                    .stage()
                    .nodes()
                    .any(|n| n.kind == NodeKind::Text("Scoped effect is running".into()))
            );
            let panel = driver
                .adapter()
                .stage()
                .nodes()
                .find(|n| n.props.width == Some(220.))
                .unwrap();
            assert_eq!(
                panel.props.opacity, 0.6,
                "Stop exposes the latest read-only derived base value"
            );
            while driver.advance_time(2., 16).unwrap().pending {}
            settle(&mut driver);
            assert!(
                driver
                    .adapter()
                    .stage()
                    .nodes()
                    .any(|n| n.kind == NodeKind::Text("Scope ticks: 1".into()))
            );
            driver.cancel().unwrap();
        }
    }

    #[test]
    fn scope_offset_example_keeps_base_motion_during_shake_stop_and_completion() {
        let checked = reel_cli::check(
            include_str!("../../../examples/author_offsets/src/main.aside"),
            "author_offsets.aside",
        )
        .unwrap();
        let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
        for compiled in [
            reel_cli::into_story(checked, "main::main").unwrap(),
            reel_player::distribution::decode_story(&artifact.bytes).unwrap(),
        ] {
            let mut driver = AsideSessionUi::from_compiled(
                Session::new(SessionId(1), Epoch(1)).unwrap(),
                compiled,
                "en",
                VmLimits::default(),
            )
            .unwrap();
            settle(&mut driver);
            let (mut bridge, root) = bridge(&driver);
            while driver.advance_time(1., 64).unwrap().pending {}
            settle(&mut driver);
            present(&mut driver, &mut bridge, root, 1);
            let position = |driver: &AsideSessionUi| {
                let panel = driver
                    .adapter()
                    .stage()
                    .nodes()
                    .find(|n| {
                        matches!(&n.kind,
                NodeKind::Button{label:Some(text),..} if text=="Catch me")
                    })
                    .unwrap();
                (panel.props.x, panel.props.y)
            };
            assert_eq!(position(&driver), (120., 100.));
            click_at(&mut driver, &mut bridge, 60., 40.);
            assert_eq!(position(&driver), (120., 97.));
            while driver.advance_time(1.025, 64).unwrap().pending {}
            settle(&mut driver);
            let (x, y) = position(&driver);
            assert!((x - 125.).abs() < 0.001);
            assert_eq!(y, 97.);
            present(&mut driver, &mut bridge, root, 2);
            click_at(&mut driver, &mut bridge, 180., 40.);
            assert!((position(&driver).0 - 122.).abs() < 0.001);
            assert_eq!(position(&driver).1, 100.);
            assert!(driver.activities_need_time());
            while driver.advance_time(2., 64).unwrap().pending {}
            settle(&mut driver);
            present(&mut driver, &mut bridge, root, 3);
            click_at(&mut driver, &mut bridge, 60., 40.);
            while driver.advance_time(2.8, 64).unwrap().pending {}
            settle(&mut driver);
            assert!((position(&driver).0 - 264.).abs() < 0.001);
            assert_eq!(position(&driver).1, 100.);
            assert!(
                driver
                    .adapter()
                    .stage()
                    .nodes()
                    .any(|n| n.kind == NodeKind::Text("Base movement continues".into()))
            );
            driver.cancel().unwrap();
        }
    }

    #[test]
    fn scope_offset_parent_coordinates_share_accepted_geometry_and_pointer_hit_testing() {
        let source = r#"fn near(a,b){if a-b>0.001 || b-a>0.001 {throw f"geometry {a} != {b}";}}
    fn main(){
        game.configure(width:640,height:420);
        let data=model({hits:0,phase:"waiting"});let done=completion();
        let target=button("",x:10,y:20,width:40,height:20,on_click:||{data.hits+=1;});
        let parent=stack(x:100,y:50,width:100,height:100,rotation:90,scale_x:2,scale_y:2){target};
        stage.replace(stack(width:640,height:420){
            parent;button("Reset",x:400,y:20,width:100,height:40,on_click:||done.resolve());
            text(derive(||f"hits:{data.hits}"),x:300,y:220);
            text(derive(||data.phase),x:300,y:250);
        });
        scope(body:|s|{
            s.offset(target,x:20,y:-15);
            let g=await stage.geometry(target,relative_to:parent);
            near(g.transform.tx,30);near(g.transform.ty,5);near(g.width,40);near(g.height,20);
            if target.x!=10 || target.y!=20 {throw "changed base";}
            data.phase="offset";await done;
        });
        let g=await stage.geometry(target,relative_to:parent);
        near(g.transform.tx,10);near(g.transform.ty,20);data.phase="base";
    }"#;
        let checked = reel_cli::check(source, "offset_geometry.aside").unwrap();
        let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
        for compiled in [
            reel_cli::into_story(checked, "main::main").unwrap(),
            reel_player::distribution::decode_story(&artifact.bytes).unwrap(),
        ] {
            let mut driver = AsideSessionUi::from_compiled(
                Session::new(SessionId(1), Epoch(1)).unwrap(),
                compiled,
                "en",
                VmLimits::default(),
            )
            .unwrap();
            settle(&mut driver);
            let (mut bridge, root) = bridge(&driver);
            let has = |driver: &AsideSessionUi, text: &str| {
                driver
                    .adapter()
                    .stage()
                    .nodes()
                    .any(|n| n.kind == NodeKind::Text(text.into()))
            };
            for frame in 1..12 {
                present(&mut driver, &mut bridge, root, frame);
                settle(&mut driver);
                if has(&driver, "offset") {
                    break;
                }
            }
            assert!(has(&driver, "offset"));
            click_at(&mut driver, &mut bridge, 80., 180.);
            assert!(has(&driver, "hits:1"));
            click_at(&mut driver, &mut bridge, 450., 40.);
            for frame in 12..24 {
                present(&mut driver, &mut bridge, root, frame);
                settle(&mut driver);
                if has(&driver, "base") {
                    break;
                }
            }
            assert!(has(&driver, "base"));
            click_at(&mut driver, &mut bridge, 80., 180.);
            assert!(has(&driver, "hits:1"));
            click_at(&mut driver, &mut bridge, 40., 100.);
            assert!(has(&driver, "hits:2"));
            driver.cancel().unwrap();
        }
    }
}
