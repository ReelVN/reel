//! Input ownership, read-only feedback and authored visuals through the real UI/VM.
#[path = "support/author.rs"]
mod support;
use reel_engine::{
    Epoch, Session, SessionId,
    aside::{VmLimits, source::compile_story},
};
use reel_player::aside_session::AsideSessionUi;
use reel_ui::{core::*, dpi::PhysicalPosition, input::*, stage::NodeKind};
use support::*;

const SOURCE: &str = r##"
fn main() {
    let count = text("count", y: 180);
    let control = button("Activate", width: 120, height: 60, decorated: false,
        on_click: |event| {
            if event.target.pressed { throw "callback must observe release"; }
            count.x += 1;
        });
    control.add(stack(width: 120, height: 60, background: "#ff0000", visible: bind(control.pressed)));
    let parent = stack(width: 120, height: 60) { control; };
    let mut failures = 0;
    try { control.pressed = true; } catch e { failures += 1; }
    try { control.hover = bind(control.enabled); } catch e { failures += 1; }
    try { unbind(bind(control.focused)); } catch e { failures += 1; }
    try { toggle(value: bind(control.pressed)); } catch e { failures += 1; }
    if failures != 4 { throw "interaction permissions"; }
    stage.replace(stack(width: 640, height: 400) {
        parent;
        button("Enable", x: 150, width: 100, height: 60, on_click: |e| { parent.enabled = !parent.enabled; });
        button("Show", x: 280, width: 100, height: 60, on_click: |e| { parent.visible = !parent.visible; });
        button("Remove", x: 410, width: 100, height: 60, on_click: |e| { control.remove(); });
        text("hover", visible: bind(control.hover), y: 100);
        text("pressed", visible: bind(control.pressed), y: 120);
        text("focused", visible: bind(control.focused), y: 140);
        text("disabled", visible: bind(control.disabled), y: 160);
        count;
    });
    narrator("Read-only state drives authored visuals without callbacks.");
}
"##;

fn visible(driver: &AsideSessionUi, name: &str) -> bool {
    driver
        .adapter()
        .stage()
        .nodes()
        .find(|n| n.kind == NodeKind::Text(name.into()))
        .unwrap()
        .props
        .visible
}
fn clicks(driver: &AsideSessionUi) -> f64 {
    driver
        .adapter()
        .stage()
        .nodes()
        .find(|n| n.kind == NodeKind::Text("count".into()))
        .unwrap()
        .props
        .x
}
fn pointer(kind: PointerType, down: Option<bool>) -> PointerEvent {
    let pointer = PointerInfo {
        pointer_id: Some(PointerId::PRIMARY),
        persistent_device_id: None,
        pointer_type: kind,
    };
    let state = PointerState {
        position: PhysicalPosition::new(40., 25.),
        ..Default::default()
    };
    match down {
        Some(down) => {
            let event = PointerButtonEvent {
                pointer,
                state,
                button: (kind == PointerType::Mouse).then_some(PointerButton::Primary),
            };
            if down {
                PointerEvent::Down(event)
            } else {
                PointerEvent::Up(event)
            }
        }
        None => PointerEvent::Move(PointerUpdate {
            pointer,
            current: state,
            coalesced: vec![],
            predicted: vec![],
        }),
    }
}

#[test]
fn source_and_asb_interaction_feedback_survives_presentation_and_cancels_with_ownership() {
    let checked = reel_cli::check(SOURCE, "control_states.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for story in [
        compile_story(SOURCE, "main", 1, "main").unwrap(),
        reel_cli::decode_story(&artifact.bytes).unwrap(),
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
        let mut frame = 0;
        let mut show = |driver: &mut AsideSessionUi,
                        bridge: &mut reel_ui::bridge::PresentationBridge| {
            frame += 1;
            present(driver, bridge, root, frame);
        };
        show(&mut driver, &mut bridge);
        for kind in [PointerType::Mouse, PointerType::Touch] {
            driver
                .handle_pointer(&mut bridge, pointer(kind, None))
                .unwrap();
            assert!(visible(&driver, "hover"));
            if kind == PointerType::Mouse {
                let before = driver.adapter().vm().heap().stats();
                let revision = driver.adapter().stage().revision();
                let started = std::time::Instant::now();
                for _ in 0..2_000 {
                    driver
                        .handle_pointer(&mut bridge, pointer(kind, None))
                        .unwrap();
                }
                let after = driver.adapter().vm().heap().stats();
                assert_eq!(after.chunk_allocations, before.chunk_allocations);
                assert_eq!(after.charged_bytes, before.charged_bytes);
                assert_eq!(driver.adapter().stage().revision(), revision);
                eprintln!(
                    "2000 unchanged hover events: {:?}; VM allocation and stage revision unchanged",
                    started.elapsed()
                );
            }
            driver
                .handle_pointer(&mut bridge, pointer(kind, Some(true)))
                .unwrap();
            assert!(visible(&driver, "pressed"));
            assert!(visible(&driver, "focused"));
            // A newly presented pressed appearance must preserve the activation.
            show(&mut driver, &mut bridge);
            driver
                .handle_pointer(&mut bridge, pointer(kind, Some(false)))
                .unwrap();
            assert!(!visible(&driver, "pressed"));
            settle(&mut driver);
            driver.sync(&mut bridge, root).unwrap();
            show(&mut driver, &mut bridge);
        }
        assert_eq!(clicks(&driver), 2.);
        for state in [keyboard::KeyState::Down, keyboard::KeyState::Up] {
            driver
                .handle_text(
                    &mut bridge,
                    TextEvent::Keyboard(keyboard::KeyboardEvent {
                        key: keyboard::Key::Character(" ".into()),
                        code: keyboard::Code::Space,
                        state,
                        ..Default::default()
                    }),
                )
                .unwrap();
            assert_eq!(
                visible(&driver, "pressed"),
                state == keyboard::KeyState::Down
            );
            show(&mut driver, &mut bridge);
        }
        settle(&mut driver);
        driver.sync(&mut bridge, root).unwrap();
        assert_eq!(clicks(&driver), 3.);
        show(&mut driver, &mut bridge);
        gamepad_press(&mut driver, &mut bridge, GamepadButton::South);
        driver.sync(&mut bridge, root).unwrap();
        assert_eq!(clicks(&driver), 4.);
        show(&mut driver, &mut bridge);
        driver
            .handle_text(
                &mut bridge,
                TextEvent::Navigation(ActionEvent {
                    action: NavigationAction::Confirm,
                    phase: ActionPhase::Pressed,
                    value: 1.,
                }),
            )
            .unwrap();
        assert!(visible(&driver, "pressed"));
        driver
            .handle_text(&mut bridge, TextEvent::WindowFocusChange(false))
            .unwrap();
        assert!(!visible(&driver, "pressed"));
        assert!(!visible(&driver, "focused"));
        driver
            .handle_text(&mut bridge, TextEvent::WindowFocusChange(true))
            .unwrap();
        show(&mut driver, &mut bridge);
        // A second contact disables/hides the first contact's active control.
        for (x, disabled) in [(180., true), (310., false)] {
            driver
                .handle_pointer(&mut bridge, pointer(PointerType::Mouse, Some(true)))
                .unwrap();
            assert!(visible(&driver, "pressed"));
            click_with_pointer(&mut driver, &mut bridge, x, 25., PointerType::Touch);
            driver.sync(&mut bridge, root).unwrap();
            assert!(!visible(&driver, "pressed"));
            assert!(!visible(&driver, "focused"));
            assert_eq!(visible(&driver, "disabled"), disabled);
            driver
                .handle_pointer(&mut bridge, pointer(PointerType::Mouse, Some(false)))
                .unwrap();
            settle(&mut driver);
            assert_eq!(clicks(&driver), 4.);
            show(&mut driver, &mut bridge);
            click_at(&mut driver, &mut bridge, x, 25.);
            show(&mut driver, &mut bridge);
        }
        driver
            .handle_pointer(&mut bridge, pointer(PointerType::Mouse, Some(true)))
            .unwrap();
        assert!(visible(&driver, "pressed"));
        click_with_pointer(&mut driver, &mut bridge, 440., 25., PointerType::Touch);
        driver.sync(&mut bridge, root).unwrap();
        assert!(!visible(&driver, "pressed"));
        assert!(!visible(&driver, "focused"));
        assert!(!visible(&driver, "hover"));
    }
}

#[test]
fn author_control_states_example_initializes_from_source_and_asb() {
    let source = include_str!("../../../examples/author_control_states.aside");
    let checked = reel_cli::check(source, "author_control_states.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for story in [
        compile_story(source, "main", 1, "main").unwrap(),
        reel_cli::decode_story(&artifact.bytes).unwrap(),
    ] {
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut driver);
        assert_eq!(
            driver
                .adapter()
                .stage()
                .nodes()
                .filter(
                    |n| matches!(n.kind, NodeKind::Button { .. } | NodeKind::Toggle { .. })
                        && !n.props.decorated
                )
                .count(),
            2
        );
    }
}

#[test]
fn author_models_example_initializes_from_source_and_asb() {
    let source = include_str!("../../../examples/author_models.aside");
    let checked = reel_cli::check(source, "author_models.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for story in [
        compile_story(source, "main", 1, "main").unwrap(),
        reel_cli::decode_story(&artifact.bytes).unwrap(),
    ] {
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut driver);
        assert!(
            driver
                .adapter()
                .stage()
                .nodes()
                .any(|node| matches!(node.kind, NodeKind::Slider { value: 160., .. }))
        );
        assert!(
            driver
                .adapter()
                .stage()
                .nodes()
                .any(|node| node.props.width == Some(160.) && node.props.height == Some(12.))
        );
    }
}
