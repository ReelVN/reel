//! Event-driven input hints shared by native hosts. Icons belong to the game.
use super::{Control, InputDeviceId, InputEvent};
use crate::core::{PointerEvent, PointerType, TextEvent, keyboard::KeyState};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputSource {
    #[default]
    Unknown,
    Keyboard,
    Mouse,
    Touch,
    Pen,
    Gamepad,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{GamepadAxis, GamepadButton};
    #[test]
    fn hints_follow_active_input_and_keep_unknown_layout_generic() {
        let mut tracker = InputTracker::default();
        let device = InputDeviceId::Gamepad(1);
        tracker.text(&TextEvent::Input(InputEvent::Connected {
            device,
            name: "pad".into(),
            layout: GamepadLayout::PlayStation,
        }));
        assert_eq!(tracker.hints().source, InputSource::Unknown);
        for value in [0.0, 0.01, -0.19] {
            tracker.text(&TextEvent::Input(InputEvent::Value {
                device,
                control: Control::Axis(GamepadAxis::LeftX),
                value,
            }));
            assert_eq!(tracker.hints().source, InputSource::Unknown);
        }
        tracker.text(&TextEvent::Input(InputEvent::Value {
            device,
            control: Control::Button(GamepadButton::South),
            value: 1.,
        }));
        assert_eq!(tracker.hints().layout, GamepadLayout::PlayStation);
        tracker.text(&TextEvent::Input(InputEvent::Value {
            device: InputDeviceId::Keyboard,
            control: Control::Key(crate::core::keyboard::Code::Space),
            value: 1.,
        }));
        assert_eq!(tracker.hints().source, InputSource::Keyboard);
        tracker.text(&TextEvent::Input(InputEvent::Value {
            device,
            control: Control::Button(GamepadButton::South),
            value: 0.,
        }));
        assert_eq!(tracker.hints().source, InputSource::Keyboard);
        let unknown = InputDeviceId::Gamepad(2);
        tracker.text(&TextEvent::Input(InputEvent::Value {
            device: unknown,
            control: Control::Axis(GamepadAxis::LeftX),
            value: 0.8,
        }));
        assert_eq!(
            tracker.hints(),
            InputHints {
                source: InputSource::Gamepad,
                layout: GamepadLayout::Generic
            }
        );
        tracker.text(&TextEvent::Input(InputEvent::Disconnected(device)));
        assert_eq!(tracker.hints().source, InputSource::Gamepad);
        tracker.text(&TextEvent::Input(InputEvent::Disconnected(unknown)));
        assert_eq!(tracker.hints(), InputHints::default());
    }
}
impl InputSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Keyboard => "keyboard",
            Self::Mouse => "mouse",
            Self::Touch => "touch",
            Self::Pen => "pen",
            Self::Gamepad => "gamepad",
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GamepadLayout {
    #[default]
    Generic,
    Xbox,
    PlayStation,
    Nintendo,
}
impl GamepadLayout {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::Xbox => "xbox",
            Self::PlayStation => "playstation",
            Self::Nintendo => "nintendo",
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputHints {
    pub source: InputSource,
    pub layout: GamepadLayout,
}
/// Device storage changes only on connection events. Ordinary input allocates nothing.
#[derive(Default)]
pub struct InputTracker {
    hints: InputHints,
    gamepads: Vec<(InputDeviceId, GamepadLayout)>,
    active: Option<InputDeviceId>,
    pointer: Option<(crate::core::PointerInfo, crate::dpi::PhysicalPosition<f64>)>,
}
impl InputTracker {
    pub fn hints(&self) -> InputHints {
        self.hints
    }
    fn select(&mut self, source: InputSource, device: Option<InputDeviceId>) {
        self.active = device;
        self.hints = InputHints {
            source,
            layout: device
                .and_then(|id| self.gamepads.iter().find(|(d, _)| *d == id))
                .map_or(GamepadLayout::Generic, |(_, layout)| *layout),
        };
    }
    pub fn text(&mut self, event: &TextEvent) {
        match event {
            TextEvent::Input(InputEvent::Connected { device, layout, .. }) => {
                if let Some(entry) = self.gamepads.iter_mut().find(|(d, _)| d == device) {
                    entry.1 = *layout;
                } else {
                    self.gamepads.push((*device, *layout));
                }
                if self.active == Some(*device) {
                    self.hints.layout = *layout;
                }
            }
            TextEvent::Input(InputEvent::Disconnected(device)) => {
                self.gamepads.retain(|(d, _)| d != device);
                if self.active == Some(*device) {
                    self.select(InputSource::Unknown, None);
                }
            }
            TextEvent::Input(InputEvent::Value {
                device,
                control,
                value,
            }) => {
                let threshold = if matches!(control, Control::Axis(_) | Control::Button(_)) {
                    0.2
                } else {
                    0.0
                };
                if value.is_finite() && value.abs() > threshold && value.abs() <= 1.0 {
                    match device {
                        InputDeviceId::Keyboard => self.select(InputSource::Keyboard, None),
                        InputDeviceId::Gamepad(_) => {
                            self.select(InputSource::Gamepad, Some(*device))
                        }
                        InputDeviceId::Pointer(_) => {}
                    }
                }
            }
            TextEvent::Keyboard(key) if key.state == KeyState::Down && !key.repeat => {
                self.select(InputSource::Keyboard, None)
            }
            _ => {}
        }
    }
    pub fn pointer(&mut self, event: &PointerEvent) {
        let (pointer, position) = match event {
            PointerEvent::Down(event) => (event.pointer, event.state.position),
            PointerEvent::Scroll(event) => (event.pointer, event.state.position),
            PointerEvent::Move(event) => {
                let position = (event.pointer, event.current.position);
                if self.pointer == Some(position) {
                    return;
                }
                position
            }
            _ => return,
        };
        self.pointer = Some((pointer, position));
        let source = match pointer.pointer_type {
            PointerType::Mouse => InputSource::Mouse,
            PointerType::Touch => InputSource::Touch,
            PointerType::Pen => InputSource::Pen,
            _ => InputSource::Unknown,
        };
        self.select(source, None);
    }
}
