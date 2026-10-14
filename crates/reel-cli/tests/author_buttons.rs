//! Source and ASB execute the same interactive scene without a window/GPU adapter.
use reel_core::identity::{FrameId, SurfaceEpoch};
use reel_engine::{
    Epoch, Session, SessionId,
    aside::{SliceBudget, VmLimits, source::CompiledStory},
};
use reel_player::aside_session::{AsideSessionUi, StoryProgress, StoryState};
use reel_ui::{
    app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
    bridge::{
        BridgeLimits, NoticeResult, PresentationBridge, PresentationNotice, PresentationOutcome,
    },
    core::*,
    dpi::{PhysicalPosition, PhysicalSize},
    stage::NodeKind,
};
use std::sync::Arc;

const SOURCE: &str = include_str!("../../../examples/author_buttons.aside");
fn settle(driver: &mut AsideSessionUi) {
    for _ in 0..10_000 {
        match driver
            .tick(
                SliceBudget {
                    instructions: 3,
                    ..Default::default()
                },
                16,
                || 0,
            )
            .unwrap()
        {
            StoryProgress::Idle | StoryProgress::Vm(reel_engine::aside::RunOutcome::Waiting(_))
                if !driver.has_runnable_tasks() && !driver.activities_pending() =>
            {
                return;
            }
            StoryProgress::Finished => return,
            StoryProgress::Faulted(_) => panic!("{:?}", driver.fault().unwrap()),
            StoryProgress::HostFailed(_) => panic!("{:?}", driver.host_failure()),
            _ => {}
        }
    }
    panic!("scene did not suspend");
}
fn present(
    driver: &mut AsideSessionUi,
    bridge: &mut PresentationBridge,
    root: WidgetId,
    frame: u64,
) {
    driver.sync(bridge, root).unwrap();
    let recording = bridge.record(FrameId(frame)).unwrap();
    let stamp = driver.projection_stamp();
    assert_eq!(
        bridge.resolve(PresentationNotice::new(
            recording.key(),
            SurfaceEpoch(1),
            PresentationOutcome::Accepted(FrameId(frame))
        )),
        NoticeResult::Accepted
    );
    driver.presented(stamp).unwrap();
}
fn click_at(driver: &mut AsideSessionUi, bridge: &mut PresentationBridge, x: f64, y: f64) {
    click_with_pointer(driver, bridge, x, y, PointerType::Mouse);
}
fn click_with_pointer(
    driver: &mut AsideSessionUi,
    bridge: &mut PresentationBridge,
    x: f64,
    y: f64,
    pointer_type: PointerType,
) {
    let pointer = PointerInfo {
        pointer_id: Some(PointerId::PRIMARY),
        persistent_device_id: None,
        pointer_type,
    };
    let state = PointerState {
        position: PhysicalPosition::new(x, y),
        ..Default::default()
    };
    for event in [
        PointerEvent::Move(PointerUpdate {
            pointer,
            current: state.clone(),
            coalesced: vec![],
            predicted: vec![],
        }),
        PointerEvent::Down(PointerButtonEvent {
            pointer,
            state: state.clone(),
            button: Some(PointerButton::Primary),
        }),
        PointerEvent::Up(PointerButtonEvent {
            pointer,
            state,
            button: Some(PointerButton::Primary),
        }),
    ] {
        driver.handle_pointer(bridge, event).unwrap();
    }
    settle(driver);
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
            reel_cli::decode_story(&artifact.bytes).unwrap(),
            pointer_type,
        );
        run(
            reel_engine::aside::source::compile_story(SOURCE, "main", 1, "main").unwrap(),
            pointer_type,
        );
    }
}

fn bridge(driver: &AsideSessionUi) -> (PresentationBridge, WidgetId) {
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
    let bridge = PresentationBridge::new(
        root,
        SurfaceEpoch(1),
        BridgeLimits {
            geometry: reel_ui::app::GeometryLimits { max_bytes: 1 << 20 },
            max_candidates: 4,
            max_bridge_bytes: 64 << 10,
            max_snapshot_bytes: 8 << 20,
        },
    )
    .unwrap();
    (bridge, root_id)
}

#[test]
fn source_and_asb_modal_menu_isolates_dialogue_and_restores_keyboard_focus() {
    const MODAL: &str = include_str!("../../../examples/author_modal.aside");
    let checked = reel_cli::check(MODAL, "author_modal.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for story in [
        reel_cli::decode_story(&artifact.bytes).unwrap(),
        reel_engine::aside::source::compile_story(MODAL, "main", 1, "main").unwrap(),
    ] {
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut driver);
        driver.reveal_more(usize::MAX).unwrap();
        let (mut bridge, root) = bridge(&driver);
        present(&mut driver, &mut bridge, root, 1);
        let press = |driver: &mut AsideSessionUi, bridge: &mut PresentationBridge, key| {
            driver
                .handle_text(bridge, TextEvent::key_down(keyboard::Key::Named(key)))
                .unwrap();
            driver
                .handle_text(bridge, TextEvent::key_up(keyboard::Key::Named(key)))
                .unwrap();
        };
        press(&mut driver, &mut bridge, keyboard::NamedKey::Tab);
        let opener = bridge.root().focused_widget().unwrap();
        let caption = driver
            .adapter()
            .stage()
            .nodes()
            .find(|node| node.kind == NodeKind::Text("Ready".into()))
            .unwrap()
            .id;
        press(&mut driver, &mut bridge, keyboard::NamedKey::Enter);
        settle(&mut driver);
        present(&mut driver, &mut bridge, root, 2);
        assert_ne!(bridge.root().focused_widget(), Some(opener));
        // Both the dialogue region and the underlying opener are blocked.
        click_at(&mut driver, &mut bridge, 400., 350.);
        click_at(&mut driver, &mut bridge, 50., 50.);
        assert_eq!(
            driver.adapter().stage().node(caption).unwrap().kind,
            NodeKind::Text("Ready".into())
        );
        press(&mut driver, &mut bridge, keyboard::NamedKey::Enter);
        settle(&mut driver);
        present(&mut driver, &mut bridge, root, 3);
        assert_eq!(
            driver.adapter().stage().node(caption).unwrap().kind,
            NodeKind::Text("Confirmed".into())
        );
        press(&mut driver, &mut bridge, keyboard::NamedKey::Tab);
        press(&mut driver, &mut bridge, keyboard::NamedKey::Enter);
        settle(&mut driver);
        present(&mut driver, &mut bridge, root, 4);
        assert_eq!(bridge.root().focused_widget(), Some(opener));
        click_at(&mut driver, &mut bridge, 400., 350.);
        present(&mut driver, &mut bridge, root, 5);
        assert_eq!(
            driver.adapter().stage().node(caption).unwrap().kind,
            NodeKind::Text("Story continued".into())
        );
        driver.cancel().unwrap();
        while driver.cleanup_pending() {
            driver
                .cleanup_step(1, SliceBudget::default(), || 0)
                .unwrap();
        }
    }
}

fn gamepad_press(
    driver: &mut AsideSessionUi,
    bridge: &mut PresentationBridge,
    button: reel_ui::input::GamepadButton,
) {
    use reel_ui::input::*;
    let mut mapping = NavigationInput::default();
    for value in [1., 0.] {
        let mut events = Vec::new();
        mapping
            .update(
                &InputEvent::Value {
                    device: InputDeviceId::Gamepad(1),
                    control: Control::Button(button),
                    value,
                },
                std::time::Duration::ZERO,
                |event| events.push(event),
            )
            .unwrap();
        for event in events {
            driver
                .handle_text(bridge, TextEvent::Navigation(event))
                .unwrap();
        }
    }
    settle(driver);
}

#[test]
fn source_and_asb_dialogue_confirmation_uses_presented_pairing() {
    const STORY: &str = r#"fn main() { narrator("First"); narrator("Second"); }"#;
    let checked = reel_cli::check(STORY, "gamepad.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for story in [
        reel_cli::decode_story(&artifact.bytes).unwrap(),
        reel_engine::aside::source::compile_story(STORY, "main", 1, "main").unwrap(),
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
        let first = driver.ui().session().view().interaction.unwrap().id();
        gamepad_press(
            &mut driver,
            &mut bridge,
            reel_ui::input::GamepadButton::South,
        );
        driver.sync(&mut bridge, root).unwrap();
        // First confirmation finishes reveal. Only an accepted new projection
        // can admit the next confirmation which advances to another dialogue.
        gamepad_press(
            &mut driver,
            &mut bridge,
            reel_ui::input::GamepadButton::South,
        );
        assert_eq!(
            driver.ui().session().view().interaction.unwrap().id(),
            first
        );
        present(&mut driver, &mut bridge, root, 2);
        gamepad_press(
            &mut driver,
            &mut bridge,
            reel_ui::input::GamepadButton::South,
        );
        assert_ne!(
            driver.ui().session().view().interaction.unwrap().id(),
            first
        );
    }
}

#[test]
fn source_and_asb_author_actions_share_keyboard_gamepad_values_and_cancellation() {
    use reel_ui::input::*;
    const INPUT: &str = include_str!("../../../examples/author_input.aside");
    let checked = reel_cli::check(INPUT, "author_input.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for story in [
        reel_cli::decode_story(&artifact.bytes).unwrap(),
        reel_engine::aside::source::compile_story(INPUT, "main", 1, "main").unwrap(),
    ] {
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut driver);
        let marker = driver
            .adapter()
            .stage()
            .nodes()
            .find(|n| matches!(&n.kind, NodeKind::Text(s) if s.starts_with("Move with")))
            .unwrap()
            .id;
        let status = driver
            .adapter()
            .stage()
            .nodes()
            .find(|n| n.kind == NodeKind::Text("Ready".into()))
            .unwrap()
            .id;
        let (mut bridge, root) = bridge(&driver);
        let mut frame = 1;
        present(&mut driver, &mut bridge, root, frame);
        let focused = bridge.root().focused_widget();
        assert!(focused.is_some(), "modal action scope takes focus");
        let events = [
            (
                InputDeviceId::Keyboard,
                Control::Key(keyboard::Code::KeyD),
                1.,
                280.,
                "pressed",
            ),
            (
                InputDeviceId::Gamepad(2),
                Control::Axis(GamepadAxis::LeftX),
                0.6,
                280.,
                "pressed",
            ),
            (
                InputDeviceId::Keyboard,
                Control::Key(keyboard::Code::KeyD),
                0.,
                220.,
                "changed",
            ),
        ];
        for (device, control, value, x, phase) in events {
            assert_eq!(
                driver
                    .handle_text(
                        &mut bridge,
                        TextEvent::Input(InputEvent::Value {
                            device,
                            control,
                            value
                        })
                    )
                    .unwrap(),
                Handled::Yes
            );
            settle(&mut driver);
            frame += 1;
            present(&mut driver, &mut bridge, root, frame);
            assert!((driver.adapter().stage().node(marker).unwrap().props.x - x).abs() < 0.0001);
            assert_eq!(
                driver.adapter().stage().node(status).unwrap().kind,
                NodeKind::Text(phase.into())
            );
            assert_eq!(
                bridge.root().focused_widget(),
                focused,
                "claimed axis does not navigate"
            );
        }
        driver
            .handle_text(
                &mut bridge,
                TextEvent::Input(InputEvent::Disconnected(InputDeviceId::Gamepad(2))),
            )
            .unwrap();
        settle(&mut driver);
        frame += 1;
        present(&mut driver, &mut bridge, root, frame);
        assert_eq!(driver.adapter().stage().node(marker).unwrap().props.x, 160.);
        assert_eq!(
            driver.adapter().stage().node(status).unwrap().kind,
            NodeKind::Text("cancelled".into())
        );
        // Real keyboard conversion uses the physical code and does not repeat presses.
        let keyboard_event = |state| {
            TextEvent::Keyboard(keyboard::KeyboardEvent {
                code: keyboard::Code::KeyA,
                key: keyboard::Key::Character("a".into()),
                state,
                ..Default::default()
            })
        };
        driver
            .handle_text(&mut bridge, keyboard_event(keyboard::KeyState::Down))
            .unwrap();
        settle(&mut driver);
        frame += 1;
        present(&mut driver, &mut bridge, root, frame);
        assert_eq!(driver.adapter().stage().node(marker).unwrap().props.x, 40.);
        driver
            .handle_text(&mut bridge, TextEvent::WindowFocusChange(false))
            .unwrap();
        settle(&mut driver);
        frame += 1;
        present(&mut driver, &mut bridge, root, frame);
        assert_eq!(driver.adapter().stage().node(marker).unwrap().props.x, 160.);
        driver
            .handle_text(&mut bridge, TextEvent::WindowFocusChange(true))
            .unwrap();
        driver
            .handle_text(
                &mut bridge,
                TextEvent::Keyboard(keyboard::KeyboardEvent {
                    code: keyboard::Code::KeyA,
                    key: keyboard::Key::Character("a".into()),
                    state: keyboard::KeyState::Down,
                    repeat: true,
                    ..Default::default()
                }),
            )
            .unwrap();
        settle(&mut driver);
        frame += 1;
        present(&mut driver, &mut bridge, root, frame);
        assert_eq!(
            driver.adapter().stage().node(marker).unwrap().props.x,
            160.,
            "system repeat must not resurrect a cancelled physical key"
        );
        // Both physical bindings hold one logical jump, avoiding duplicate callbacks.
        for (device, control, value) in [
            (
                InputDeviceId::Keyboard,
                Control::Key(keyboard::Code::Space),
                1.,
            ),
            (
                InputDeviceId::Gamepad(2),
                Control::Button(GamepadButton::South),
                1.,
            ),
            (
                InputDeviceId::Keyboard,
                Control::Key(keyboard::Code::Space),
                0.,
            ),
        ] {
            driver
                .handle_text(
                    &mut bridge,
                    TextEvent::Input(InputEvent::Value {
                        device,
                        control,
                        value,
                    }),
                )
                .unwrap();
            settle(&mut driver);
        }
        assert!(driver.activities_need_time());
        driver.advance_time(0.15, 32).unwrap();
        settle(&mut driver);
        frame += 1;
        present(&mut driver, &mut bridge, root, frame);
        assert_eq!(driver.adapter().stage().node(marker).unwrap().props.y, 16.);
        driver.advance_time(0.3, 32).unwrap();
        settle(&mut driver);
        frame += 1;
        present(&mut driver, &mut bridge, root, frame);
        assert!(!driver.activities_need_time());
        for value in [1., 0.] {
            driver
                .handle_text(
                    &mut bridge,
                    TextEvent::Input(InputEvent::Value {
                        device: InputDeviceId::Gamepad(2),
                        control: Control::Button(GamepadButton::East),
                        value,
                    }),
                )
                .unwrap();
            settle(&mut driver);
        }
        assert_eq!(driver.state(), StoryState::Finished);
        assert_eq!(
            driver.adapter().stage().nodes().count(),
            0,
            "finishing retires VM nodes, callbacks and activities; the UI keeps its final projection"
        );
        driver.cancel().unwrap();
        while driver.cleanup_pending() {
            driver
                .cleanup_step(1, SliceBudget::default(), || 0)
                .unwrap();
        }
    }
}

#[test]
fn source_and_asb_input_scope_remaps_and_retires_multitouch_ownership() {
    use reel_ui::input::*;
    let input_source = r#"
fn main() {
    let marker = text("marker", y: 220, width: 100, height: 30);
    let status = text("ready", y: 260, width: 200, height: 30);
    let popup = stack(x: 200, width: 200, height: 100, modal: true, visible: false);
    popup.add(button("Close", width: 180, height: 80, on_click: |e| { popup.visible = false; }));
    let pad = input(modal: true, width: 640, height: 300,
        bindings: {
            move_x: [binding("KeyD")],
            fire: [binding("PointerPrimary")],
            menu: [binding("Start")],
            remap: [binding("North")],
        },
        on_action: |e| {
            if e.action == "move_x" { marker.x = e.target.value("move_x") * 100; }
            if e.action == "fire" { status.value = e.phase; }
            if e.action == "menu" && e.phase == "pressed" { popup.visible = true; }
            if e.action == "remap" && e.phase == "pressed" {
                e.target.bindings = {move_x: [binding("KeyJ")], fire: [binding("PointerPrimary"), binding("PointerSecondary"), binding("South"), binding("Space")]};
                marker.x = e.target.value("move_x") * 100;
            }
        }
    ) {
        marker; status; popup;
        button("UI", x: 420, width: 100, height: 80,
            on_click: |e| { marker.y += 1; });
    };
    let mut rejected = false;
    try { pad.bindings = {bad: [["no-such-control", 1, 0]]}; }
    catch error { rejected = true; }
    if !rejected { throw "invalid binding assignment must fail at the script boundary"; }
    stage.replace(pad);
    narrator("input must not advance this dialogue");
}
"#;
    let checked = reel_cli::check(input_source, "input-scope.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for story in [
        reel_cli::decode_story(&artifact.bytes).unwrap(),
        reel_engine::aside::source::compile_story(input_source, "main", 1, "main").unwrap(),
    ] {
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut driver);
        let marker = driver
            .adapter()
            .stage()
            .nodes()
            .find(|n| n.kind == NodeKind::Text("marker".into()))
            .unwrap()
            .id;
        let status = driver
            .adapter()
            .stage()
            .nodes()
            .find(|n| n.kind == NodeKind::Text("ready".into()))
            .unwrap()
            .id;
        let (mut bridge, root) = bridge(&driver);
        let mut frame = 0;
        let flush =
            |driver: &mut AsideSessionUi, bridge: &mut PresentationBridge, frame: &mut u64| {
                for _ in 0..2 {
                    settle(driver);
                    *frame += 1;
                    present(driver, bridge, root, *frame);
                }
            };
        flush(&mut driver, &mut bridge, &mut frame);
        let send =
            |driver: &mut AsideSessionUi, bridge: &mut PresentationBridge, control, value| {
                driver
                    .handle_text(
                        bridge,
                        TextEvent::Input(InputEvent::Value {
                            device: InputDeviceId::Gamepad(1),
                            control,
                            value,
                        }),
                    )
                    .unwrap()
            };
        send(
            &mut driver,
            &mut bridge,
            Control::Key(keyboard::Code::KeyD),
            1.,
        );
        flush(&mut driver, &mut bridge, &mut frame);
        assert_eq!(driver.adapter().stage().node(marker).unwrap().props.x, 100.);
        send(
            &mut driver,
            &mut bridge,
            Control::Button(GamepadButton::Start),
            1.,
        );
        flush(&mut driver, &mut bridge, &mut frame);
        assert_eq!(
            driver.adapter().stage().node(marker).unwrap().props.x,
            0.,
            "accepted modal change cancels old held action"
        );
        driver
            .handle_text(
                &mut bridge,
                TextEvent::key_down(keyboard::Key::Named(keyboard::NamedKey::Enter)),
            )
            .unwrap();
        driver
            .handle_text(
                &mut bridge,
                TextEvent::key_up(keyboard::Key::Named(keyboard::NamedKey::Enter)),
            )
            .unwrap();
        flush(&mut driver, &mut bridge, &mut frame);
        send(
            &mut driver,
            &mut bridge,
            Control::Key(keyboard::Code::KeyD),
            1.,
        );
        flush(&mut driver, &mut bridge, &mut frame);
        assert_eq!(driver.adapter().stage().node(marker).unwrap().props.x, 100.);
        send(
            &mut driver,
            &mut bridge,
            Control::Button(GamepadButton::North),
            1.,
        );
        flush(&mut driver, &mut bridge, &mut frame);
        assert_eq!(
            driver.adapter().stage().node(marker).unwrap().props.x,
            0.,
            "rebind is neutral even before the next frame"
        );
        assert_eq!(
            send(
                &mut driver,
                &mut bridge,
                Control::Key(keyboard::Code::KeyD),
                1.
            ),
            Handled::No
        );
        send(
            &mut driver,
            &mut bridge,
            Control::Key(keyboard::Code::KeyJ),
            1.,
        );
        flush(&mut driver, &mut bridge, &mut frame);
        assert_eq!(driver.adapter().stage().node(marker).unwrap().props.x, 100.);
        driver
            .handle_text(
                &mut bridge,
                TextEvent::key_down(keyboard::Key::Named(keyboard::NamedKey::Tab)),
            )
            .unwrap();
        flush(&mut driver, &mut bridge, &mut frame);
        assert_eq!(
            driver.adapter().stage().node(marker).unwrap().props.x,
            0.,
            "child focus cancels the parent game action"
        );
        assert_eq!(
            send(
                &mut driver,
                &mut bridge,
                Control::Button(GamepadButton::South),
                1.
            ),
            Handled::No,
            "parent mapping must not steal a focused child button's confirmation"
        );
        gamepad_press(&mut driver, &mut bridge, GamepadButton::South);
        flush(&mut driver, &mut bridge, &mut frame);
        assert_eq!(driver.adapter().stage().node(marker).unwrap().props.y, 221.);
        for state in [keyboard::KeyState::Down, keyboard::KeyState::Up] {
            driver
                .handle_text(
                    &mut bridge,
                    TextEvent::Keyboard(keyboard::KeyboardEvent {
                        code: keyboard::Code::Space,
                        key: keyboard::Key::Character(" ".into()),
                        state,
                        ..Default::default()
                    }),
                )
                .unwrap();
        }
        flush(&mut driver, &mut bridge, &mut frame);
        assert_eq!(
            driver.adapter().stage().node(marker).unwrap().props.y,
            222.,
            "keyboard and controller activate the same child control"
        );
        driver
            .handle_text(
                &mut bridge,
                TextEvent::key_down(keyboard::Key::Named(keyboard::NamedKey::Tab)),
            )
            .unwrap();
        flush(&mut driver, &mut bridge, &mut frame);
        send(
            &mut driver,
            &mut bridge,
            Control::Key(keyboard::Code::KeyJ),
            1.,
        );
        flush(&mut driver, &mut bridge, &mut frame);
        assert_eq!(driver.adapter().stage().node(marker).unwrap().props.x, 100.);
        for (id, down, phase) in [
            (10, true, "pressed"),
            (11, true, "pressed"),
            (10, false, "pressed"),
            (11, false, "released"),
        ] {
            let pointer = PointerInfo {
                pointer_id: PointerId::new(id),
                pointer_type: PointerType::Touch,
                persistent_device_id: None,
            };
            let event = PointerButtonEvent {
                pointer,
                state: PointerState {
                    position: PhysicalPosition::new(40., 40.),
                    ..Default::default()
                },
                button: None,
            };
            driver
                .handle_pointer(
                    &mut bridge,
                    if down {
                        PointerEvent::Down(event)
                    } else {
                        PointerEvent::Up(event)
                    },
                )
                .unwrap();
            flush(&mut driver, &mut bridge, &mut frame);
            assert_eq!(
                driver.adapter().stage().node(status).unwrap().kind,
                NodeKind::Text(phase.into()),
                "both contacts own the same logical action independently"
            );
        }
        let pointer = PointerInfo {
            pointer_id: Some(PointerId::PRIMARY),
            pointer_type: PointerType::Mouse,
            persistent_device_id: None,
        };
        let mut buttons = pointer::PointerButtons::default();
        for (button, down) in [
            (PointerButton::Primary, true),
            (PointerButton::Secondary, true),
            (PointerButton::Primary, false),
        ] {
            if down {
                buttons.insert(button);
            } else {
                buttons.remove(button);
            }
            let event = PointerButtonEvent {
                pointer,
                state: PointerState {
                    position: if down {
                        PhysicalPosition::new(40., 40.)
                    } else {
                        PhysicalPosition::new(900., 900.)
                    },
                    buttons,
                    ..Default::default()
                },
                button: Some(button),
            };
            driver
                .handle_pointer(
                    &mut bridge,
                    if down {
                        PointerEvent::Down(event)
                    } else {
                        PointerEvent::Up(event)
                    },
                )
                .unwrap();
            flush(&mut driver, &mut bridge, &mut frame);
            assert_eq!(
                driver.adapter().stage().node(status).unwrap().kind,
                NodeKind::Text("pressed".into()),
                "remaining mouse button retains the shared action outside the scope bounds"
            );
        }
        assert_eq!(
            driver.ui().session().view().history.len(),
            1,
            "consumed action did not advance dialogue"
        );
        bridge.clear();
        driver.clear_presented().unwrap();
        settle(&mut driver);
        driver.sync(&mut bridge, root).unwrap();
        assert_eq!(driver.adapter().stage().node(marker).unwrap().props.x, 0.);
        assert_eq!(
            driver.adapter().stage().node(status).unwrap().kind,
            NodeKind::Text("cancelled".into())
        );
        driver.cancel().unwrap();
        while driver.cleanup_pending() {
            driver
                .cleanup_step(1, SliceBudget::default(), || 0)
                .unwrap();
        }
    }
}
