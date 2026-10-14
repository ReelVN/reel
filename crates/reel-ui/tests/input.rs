use reel_ui::core::keyboard::Code;
use reel_ui::input::*;
use std::time::Duration;

fn value(device: u64, control: Control, value: f32) -> InputEvent {
    InputEvent::Value {
        device: InputDeviceId::Gamepad(device),
        control,
        value,
    }
}

#[test]
fn actions_aggregate_devices_bindings_analog_values_and_terminal_causes() {
    let key = Control::Key(Code::Space);
    let button = Control::Button(GamepadButton::South);
    let axis = Control::Axis(GamepadAxis::LeftX);
    let mut map = ActionMap::new([
        ("jump", vec![Binding::new(key), Binding::new(button)]),
        ("move", vec![Binding::new(axis)]),
    ])
    .unwrap();
    let mut events = Vec::new();
    for event in [
        value(0, key, 1.),
        value(1, button, 1.),
        value(2, button, 1.),
        value(0, key, 0.),
        InputEvent::Disconnected(InputDeviceId::Gamepad(1)),
    ] {
        map.update(&event, |e| events.push(e)).unwrap();
    }
    assert_eq!(
        events,
        vec![ActionEvent {
            action: "jump",
            phase: ActionPhase::Pressed,
            value: 1.
        }]
    );
    assert_eq!(map.value("jump"), 1.);
    map.update(&value(2, button, 0.), |e| events.push(e))
        .unwrap();
    assert_eq!(events.last().unwrap().phase, ActionPhase::Released);
    map.update(&value(1, axis, -0.75), |e| events.push(e))
        .unwrap();
    map.update(&value(1, axis, 0.5), |e| events.push(e))
        .unwrap();
    assert_eq!(events.last().unwrap().phase, ActionPhase::Changed);
    map.update(&InputEvent::Disconnected(InputDeviceId::Gamepad(1)), |e| {
        events.push(e)
    })
    .unwrap();
    assert_eq!(events.last().unwrap().phase, ActionPhase::Cancelled);
    assert_eq!(map.value("move"), 0.);
    assert!(map.update(&value(1, axis, f32::NAN), |_| {}).is_err());
    assert_eq!(map.value("move"), 0.);
}

#[test]
fn navigation_hysteresis_repeat_disconnect_and_remapping_share_one_clock() {
    let mut input = NavigationInput::default();
    let axis = Control::Axis(GamepadAxis::LeftX);
    let mut events = Vec::new();
    let mut update = |input: &mut NavigationInput, v, time| {
        input
            .update(&value(1, axis, v), Duration::from_millis(time), |e| {
                events.push(e)
            })
            .unwrap()
    };
    update(&mut input, 0.1, 0);
    assert_eq!(input.next_deadline(), None);
    update(&mut input, 0.9, 10);
    update(&mut input, 0.6, 20);
    assert_eq!(input.next_deadline(), Some(Duration::from_millis(360)));
    input.tick(Duration::from_millis(359), |e| events.push(e));
    assert_eq!(events.len(), 1);
    input.tick(Duration::from_millis(1000), |e| events.push(e));
    assert_eq!(
        events.len(),
        2,
        "a delayed turn emits one repeat, not a catch-up burst"
    );
    assert_eq!(events[1].phase, ActionPhase::Repeated);
    input
        .update(
            &InputEvent::Disconnected(InputDeviceId::Gamepad(1)),
            Duration::from_secs(1),
            |e| events.push(e),
        )
        .unwrap();
    assert_eq!(events.last().unwrap().phase, ActionPhase::Cancelled);
    assert_eq!(input.next_deadline(), None);
    input
        .update(
            &value(2, Control::Button(GamepadButton::South), 1.),
            Duration::ZERO,
            |e| events.push(e),
        )
        .unwrap();
    let remapped = ActionMap::new([(
        NavigationAction::Confirm,
        vec![Binding::new(Control::Button(GamepadButton::North))],
    )])
    .unwrap();
    input.replace_bindings(remapped, |e| events.push(e));
    assert_eq!(events.last().unwrap().phase, ActionPhase::Cancelled);
    let count = events.len();
    input
        .update(
            &value(2, Control::Button(GamepadButton::South), 0.),
            Duration::ZERO,
            |e| events.push(e),
        )
        .unwrap();
    assert_eq!(events.len(), count);
    input
        .update(
            &value(2, Control::Button(GamepadButton::North), 1.),
            Duration::ZERO,
            |e| events.push(e),
        )
        .unwrap();
    assert_eq!(events.last().unwrap().action, NavigationAction::Confirm);
    assert_eq!(events.last().unwrap().phase, ActionPhase::Pressed);
    assert_eq!(input.next_deadline(), None, "confirm never repeats");
    let mut confirm = NavigationInput::default();
    events.clear();
    for event in [
        value(1, Control::Button(GamepadButton::South), 0.9),
        value(2, Control::Button(GamepadButton::South), 0.2),
        InputEvent::Disconnected(InputDeviceId::Gamepad(1)),
    ] {
        confirm
            .update(&event, Duration::ZERO, |e| events.push(e))
            .unwrap();
    }
    assert_eq!(
        events.last().unwrap().phase,
        ActionPhase::Cancelled,
        "disconnect below the navigation threshold must not turn into a confirmation release"
    );
}
