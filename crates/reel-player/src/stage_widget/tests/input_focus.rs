use super::*;
use reel_ui::{
    core::{PointerButton, PointerButtonEvent, PointerInfo, PointerState, PointerType},
    dpi::PhysicalPosition,
    input::{
        ActionPhase, Binding, Control, GamepadButton, InputDeviceId, InputEvent, NavigationInput,
    },
    stage::{ActionDefinition, InputBindings, PointerPhase},
};

fn input(keyboard: bool, controls: &[Control], pointer: bool) -> NodeKind {
    NodeKind::Input {
        bindings: InputBindings::new(
            (!controls.is_empty())
                .then(|| ActionDefinition {
                    name: "action".into(),
                    bindings: controls.iter().copied().map(Binding::new).collect(),
                })
                .into_iter()
                .collect(),
        )
        .unwrap(),
        listening: true,
        keyboard,
        pointer,
        wheel: false,
    }
}

fn setup() -> (Fixture, NodeId) {
    let mut f = Fixture::new(None);
    f.stage.clear_children(f.root).unwrap();
    f.stage
        .update(
            f.root,
            input(
                false,
                &[
                    Control::Key(keyboard::Code::Space),
                    Control::Button(GamepadButton::South),
                    Control::Button(GamepadButton::East),
                ],
                false,
            ),
            NodeProps {
                width: Some(1280.),
                height: Some(720.),
                ..Default::default()
            },
        )
        .unwrap();
    let blocker = f
        .stage
        .create(
            input(false, &[], true),
            NodeProps {
                width: Some(1280.),
                height: Some(720.),
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    f.stage.add(f.root, blocker, 0).unwrap();
    let mut buttons = Vec::new();
    for y in [142., 281.] {
        let button = f
            .stage
            .create(
                NodeKind::Button {
                    label: None,
                    listening: true,
                },
                NodeProps {
                    y,
                    width: Some(1280.),
                    height: Some(55.),
                    ..Default::default()
                },
                vec![],
            )
            .unwrap();
        f.stage.add(f.root, button, buttons.len() + 1).unwrap();
        buttons.push(button);
    }
    f.red = buttons[0];
    f.blue = buttons[1];
    f.bridge
        .handle_window_event(WindowEvent::Resize(PhysicalSize::new(1280, 720)));
    let frame = f.record();
    f.accept(&frame);
    (f, blocker)
}

pub(super) fn press_pointer(f: &mut Fixture, pressed: bool, x: f64, y: f64) -> Vec<NodeEvent> {
    let event = PointerButtonEvent {
        pointer: PointerInfo {
            pointer_id: Some(PointerId::PRIMARY),
            persistent_device_id: None,
            pointer_type: PointerType::Mouse,
        },
        state: PointerState {
            position: PhysicalPosition::new(x, y),
            ..Default::default()
        },
        button: Some(PointerButton::Primary),
    };
    f.ui.handle_pointer(
        &mut f.bridge,
        if pressed {
            PointerEvent::Down(event)
        } else {
            PointerEvent::Up(event)
        },
    )
    .unwrap()
    .events
}

fn gamepad(f: &mut Fixture, buttons: &[GamepadButton]) -> Vec<NodeEvent> {
    let mut navigation = NavigationInput::default();
    let mut events = Vec::new();
    for &button in buttons {
        for value in [1., 0.] {
            let event = InputEvent::Value {
                device: InputDeviceId::Gamepad(1),
                control: Control::Button(button),
                value,
            };
            let result =
                f.ui.handle_text(&mut f.bridge, TextEvent::Input(event.clone()))
                    .unwrap();
            events.extend(result.events);
            if !result.handled.is_handled() {
                let mut semantic = Vec::new();
                navigation
                    .update(&event, std::time::Duration::ZERO, |event| {
                        semantic.push(event)
                    })
                    .unwrap();
                for event in semantic {
                    events.extend(
                        f.ui.handle_text(&mut f.bridge, TextEvent::Navigation(event))
                            .unwrap()
                            .events,
                    );
                }
            }
        }
    }
    events
}

#[test]
fn input_focus_gamepad_enters_buttons_above_container_center_and_confirms_child() {
    let (mut f, blocker) = setup();
    let ids = f.layouts();
    assert!(
        !f.bridge
            .root()
            .get_widget(ids[&blocker].0)
            .unwrap()
            .accepts_focus()
    );
    f.ui.handle_text(
        &mut f.bridge,
        TextEvent::key_down(keyboard::Key::Named(keyboard::NamedKey::Tab)),
    )
    .unwrap();
    assert_eq!(f.bridge.root().focused_widget(), Some(ids[&f.root].0));
    let events = gamepad(
        &mut f,
        &[
            GamepadButton::DpadDown,
            GamepadButton::DpadDown,
            GamepadButton::South,
        ],
    );
    assert_eq!(f.bridge.root().focused_widget(), Some(ids[&f.blue].0));
    assert_eq!(
        events
            .iter()
            .filter_map(|event| match event {
                NodeEvent::Click(event) => Some(event.node),
                _ => None,
            })
            .collect::<Vec<_>>(),
        [f.blue]
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, NodeEvent::Action(_))),
        "child confirmation owns South before the enclosing action binding"
    );
}

#[test]
fn input_focus_first_direction_enters_controls_without_stopping_at_container() {
    for directions in [
        vec![GamepadButton::DpadUp],
        vec![GamepadButton::DpadDown, GamepadButton::DpadDown],
    ] {
        let (mut f, _) = setup();
        let ids = f.layouts();
        assert_eq!(f.bridge.root().focused_widget(), None);
        gamepad(&mut f, &directions);
        assert_eq!(f.bridge.root().focused_widget(), Some(ids[&f.blue].0));
        let events = gamepad(&mut f, &[GamepadButton::South]);
        assert_eq!(
            events
                .iter()
                .filter_map(|event| match event {
                    NodeEvent::Click(event) => Some(event.node),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            [f.blue]
        );
    }
}

#[test]
fn input_focus_pointer_only_preserves_focus_and_keyboard_fallback() {
    let (mut f, blocker) = setup();
    let ids = f.layouts();
    let event = InputEvent::Value {
        device: InputDeviceId::Keyboard,
        control: Control::Key(keyboard::Code::Space),
        value: 1.,
    };
    let result =
        f.ui.handle_text(&mut f.bridge, TextEvent::Input(event))
            .unwrap();
    assert!(result.events.iter().any(|event| matches!(event,
        NodeEvent::Action(event) if event.node == f.root && event.phase == ActionPhase::Pressed)));
    assert_eq!(f.bridge.root().focused_widget(), None);
    press_pointer(&mut f, true, 640., 300.);
    press_pointer(&mut f, false, 640., 300.);
    let events = press_pointer(&mut f, true, 640., 600.);
    assert!(events.iter().any(|event| matches!(event,
        NodeEvent::Pointer(event) if event.node == blocker && event.phase == PointerPhase::Pressed)));
    assert_eq!(f.bridge.root().focused_widget(), Some(ids[&f.blue].0));
    press_pointer(&mut f, false, 640., 600.);
    assert_eq!(f.bridge.root().focused_widget(), Some(ids[&f.blue].0));

    let props = f.stage.node(blocker).unwrap().props.clone();
    f.stage
        .update(
            blocker,
            input(false, &[Control::Pointer(PointerButton::Primary)], false),
            props,
        )
        .unwrap();
    let frame = f.record();
    f.accept(&frame);
    let events = press_pointer(&mut f, true, 640., 600.);
    assert!(events.iter().any(|event| matches!(event,
        NodeEvent::Action(event) if event.node == blocker && event.phase == ActionPhase::Pressed)));
    assert_eq!(f.bridge.root().focused_widget(), Some(ids[&f.blue].0));
    press_pointer(&mut f, false, 640., 600.);
}

#[test]
fn input_focus_dynamic_keyboard_capability_updates_tab_and_fallback() {
    let (mut f, blocker) = setup();
    let ids = f.layouts();
    f.ui.handle_text(
        &mut f.bridge,
        TextEvent::key_down(keyboard::Key::Named(keyboard::NamedKey::Tab)),
    )
    .unwrap();
    let props = f.stage.node(blocker).unwrap().props.clone();
    f.stage
        .update(blocker, input(true, &[], true), props.clone())
        .unwrap();
    let frame = f.record();
    f.accept(&frame);
    assert!(
        f.bridge
            .root()
            .get_widget(ids[&blocker].0)
            .unwrap()
            .accepts_focus()
    );
    f.ui.handle_text(
        &mut f.bridge,
        TextEvent::key_down(keyboard::Key::Named(keyboard::NamedKey::Tab)),
    )
    .unwrap();
    assert_eq!(f.bridge.root().focused_widget(), Some(ids[&blocker].0));
    let keys =
        f.ui.handle_text(
            &mut f.bridge,
            TextEvent::key_down(keyboard::Key::Character("a".into())),
        )
        .unwrap()
        .events;
    assert!(keys.iter().any(|event| matches!(event,
        NodeEvent::Key(event) if event.node == blocker && event.phase == ActionPhase::Pressed)));
    f.stage
        .update(blocker, input(false, &[], true), props)
        .unwrap();
    let frame = f.record();
    assert_eq!(f.bridge.root().focused_widget(), None);
    f.accept(&frame);
    assert!(
        !f.bridge
            .root()
            .get_widget(ids[&blocker].0)
            .unwrap()
            .accepts_focus()
    );
    let result =
        f.ui.handle_text(
            &mut f.bridge,
            TextEvent::Input(InputEvent::Value {
                device: InputDeviceId::Gamepad(1),
                control: Control::Button(GamepadButton::East),
                value: 1.,
            }),
        )
        .unwrap();
    assert!(result.events.iter().any(|event| matches!(event,
        NodeEvent::Action(event) if event.node == f.root && event.phase == ActionPhase::Pressed)));
}
