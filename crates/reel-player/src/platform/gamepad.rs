//! Gilrs desktop adapter. Portable input and UI never depend on this module.
use reel_ui::input::{Control, GamepadAxis, GamepadButton, InputDeviceId, InputEvent};
use std::{
    collections::HashMap,
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    time::Duration,
};

/// Owns the receiving end of a bounded device queue. Dropping it cancels the
/// worker without waiting for native input or joining on the event-loop thread.
pub struct GamepadSource {
    events: Receiver<InputEvent>,
    stop: Arc<AtomicBool>,
}
impl GamepadSource {
    pub fn new(
        capacity: NonZeroUsize,
        wake: impl Fn() + Send + 'static,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut gilrs = gilrs::Gilrs::new()?;
        let (sender, events) = mpsc::sync_channel(capacity.get());
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        std::thread::Builder::new()
            .name("reel-gamepad".into())
            .spawn(move || {
                let mut devices = HashMap::new();
                let mut next_id = 1u64;
                let send = |event| {
                    if sender.send(event).is_err() {
                        false
                    } else {
                        wake();
                        true
                    }
                };
                for (native, gamepad) in gilrs.gamepads() {
                    let device = InputDeviceId::Gamepad(next_id);
                    next_id += 1;
                    devices.insert(native, device);
                    if !send(InputEvent::Connected {
                        device,
                        name: gamepad.name().into(),
                        layout: layout(&gamepad),
                    }) {
                        return;
                    }
                }
                while !stopping.load(Ordering::Acquire) {
                    let Some(event) = gilrs.next_event_blocking(Some(Duration::from_millis(100)))
                    else {
                        continue;
                    };
                    use gilrs::EventType as E;
                    let output = match event.event {
                        E::Connected => {
                            if devices.contains_key(&event.id) {
                                continue;
                            }
                            let device = InputDeviceId::Gamepad(next_id);
                            next_id += 1;
                            devices.insert(event.id, device);
                            Some(InputEvent::Connected {
                                device,
                                name: gilrs.gamepad(event.id).name().into(),
                                layout: layout(&gilrs.gamepad(event.id)),
                            })
                        }
                        E::Disconnected => devices.remove(&event.id).map(InputEvent::Disconnected),
                        kind => devices
                            .get(&event.id)
                            .copied()
                            .and_then(|device| convert(device, kind)),
                    };
                    if let Some(event) = output
                        && !send(event)
                    {
                        return;
                    }
                }
            })?;
        Ok(Self { events, stop })
    }
    /// `Disconnected` here means the worker stopped, distinct from a gamepad
    /// hotplug event returned as `InputEvent::Disconnected`.
    pub fn try_event(&self) -> Result<Option<InputEvent>, mpsc::TryRecvError> {
        match self.events.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(error) => Err(error),
        }
    }
}
impl Drop for GamepadSource {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

fn layout(gamepad: &gilrs::Gamepad<'_>) -> reel_ui::input::GamepadLayout {
    recognized_layout(gamepad.vendor_id(), gamepad.product_id())
}

fn recognized_layout(vendor: Option<u16>, product: Option<u16>) -> reel_ui::input::GamepadLayout {
    use reel_ui::input::GamepadLayout as L;
    // USB product identities, also used by SDL's controller classification.
    // Mapping to positional controls alone does not establish the printed labels.
    match (vendor, product) {
        (Some(0x054c), Some(0x0268 | 0x05c4 | 0x09cc | 0x0ba0 | 0x0ce6 | 0x0df2)) => L::PlayStation,
        (
            Some(0x045e),
            Some(
                0x028e | 0x028f | 0x02d1 | 0x02dd | 0x02e0 | 0x02e3 | 0x02ea | 0x02fd | 0x0b00
                | 0x0b05 | 0x0b12 | 0x0b13,
            ),
        ) => L::Xbox,
        (Some(0x057e), Some(0x2006 | 0x2007 | 0x2009 | 0x200e)) => L::Nintendo,
        _ => L::Generic,
    }
}

fn convert(device: InputDeviceId, event: gilrs::EventType) -> Option<InputEvent> {
    use gilrs::EventType as E;
    let (control, value) = match event {
        E::ButtonChanged(button, value, _) => (Control::Button(button_kind(button)?), value),
        E::AxisChanged(axis, value, _) => {
            let (axis, sign) = axis_kind(axis)?;
            (Control::Axis(axis), value * sign)
        }
        // Gilrs sends ButtonChanged for digital and analog controls, including
        // mapped D-pads. Pressed/Released are redundant threshold notifications;
        // treating Pressed as 1.0 would corrupt analog trigger values.
        // Repeats belong to the portable clocked action layer.
        _ => return None,
    };
    Some(InputEvent::Value {
        device,
        control,
        value,
    })
}
fn button_kind(button: gilrs::Button) -> Option<GamepadButton> {
    use GamepadButton as G;
    use gilrs::Button as B;
    Some(match button {
        B::South => G::South,
        B::East => G::East,
        B::North => G::North,
        B::West => G::West,
        B::C => G::C,
        B::Z => G::Z,
        B::LeftTrigger => G::LeftShoulder,
        B::RightTrigger => G::RightShoulder,
        B::LeftTrigger2 => G::LeftTrigger,
        B::RightTrigger2 => G::RightTrigger,
        B::Select => G::Select,
        B::Start => G::Start,
        B::Mode => G::Mode,
        B::LeftThumb => G::LeftThumb,
        B::RightThumb => G::RightThumb,
        B::DPadUp => G::DpadUp,
        B::DPadDown => G::DpadDown,
        B::DPadLeft => G::DpadLeft,
        B::DPadRight => G::DpadRight,
        B::Unknown => return None,
    })
}
fn axis_kind(axis: gilrs::Axis) -> Option<(GamepadAxis, f32)> {
    use GamepadAxis as G;
    use gilrs::Axis as A;
    Some(match axis {
        A::LeftStickX => (G::LeftX, 1.),
        A::LeftStickY => (G::LeftY, -1.),
        A::RightStickX => (G::RightX, 1.),
        A::RightStickY => (G::RightY, -1.),
        A::LeftZ => (G::LeftZ, 1.),
        A::RightZ => (G::RightZ, 1.),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gilrs_standard_controls_preserve_analog_values_and_screen_axis_direction() {
        let device = InputDeviceId::Gamepad(7);
        let code = gilrs::Button::South.to_nec().unwrap();
        assert_eq!(
            convert(
                device,
                gilrs::EventType::ButtonChanged(gilrs::Button::LeftTrigger2, 0.42, code)
            ),
            Some(InputEvent::Value {
                device,
                control: Control::Button(GamepadButton::LeftTrigger),
                value: 0.42
            })
        );
        assert!(
            convert(
                device,
                gilrs::EventType::ButtonPressed(gilrs::Button::LeftTrigger2, code)
            )
            .is_none()
        );
        assert_eq!(
            convert(
                device,
                gilrs::EventType::AxisChanged(gilrs::Axis::LeftStickY, 0.75, code)
            ),
            Some(InputEvent::Value {
                device,
                control: Control::Axis(GamepadAxis::LeftY),
                value: -0.75
            })
        );
        assert_eq!(
            convert(
                device,
                gilrs::EventType::ButtonChanged(gilrs::Button::DPadRight, 0., code)
            ),
            Some(InputEvent::Value {
                device,
                control: Control::Button(GamepadButton::DpadRight),
                value: 0.
            })
        );
    }
    #[test]
    #[ignore = "requires a native gamepad backend; does not verify physical button input"]
    fn native_gamepad_backend_initializes_and_drops_without_waiting() {
        let source = GamepadSource::new(NonZeroUsize::new(16).unwrap(), || {}).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let mut connected = 0;
        while let Some(event) = source.try_event().unwrap() {
            if matches!(event, InputEvent::Connected { .. }) {
                connected += 1;
            }
        }
        let start = std::time::Instant::now();
        drop(source);
        println!(
            "Gilrs backend initialized; connected={connected}; receiving-end drop={:?}; physical buttons not exercised",
            start.elapsed()
        );
    }
}
