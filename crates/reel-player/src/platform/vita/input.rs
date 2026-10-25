//! Controller actions and stable touch identities on the common input path.
use super::{Error, GlesBackend, Player};
use pvr_psp2_sys::platform::input as native;
use reel_core::clock::MonoTime;
use reel_ui::{
    core::*,
    dpi::PhysicalPosition,
    input::{self, Control, GamepadAxis, GamepadButton, InputDeviceId, InputEvent},
};
use std::time::Duration;

#[derive(Default)]
pub(super) struct Input {
    last: native::Sample,
    contacts: Vec<native::Touch>,
    blocked_touches: Vec<u8>,
    blocked_buttons: u32,
    blocked_axes: [bool; 4],
    connected: bool,
    navigation: input::NavigationInput,
    events: Vec<input::NavigationEvent>,
}
impl Input {
    pub fn return_to_library(&self, sample: &native::Sample) -> bool {
        let chord = native::SELECT | native::START;
        sample.buttons & chord == chord && self.blocked_buttons & chord == 0
    }
    pub fn new(sample: native::Sample) -> Self {
        Self {
            blocked_buttons: sample.buttons,
            blocked_axes: sample.axes.map(|value| value.abs() > 0.2),
            blocked_touches: sample.touches[..sample.touch_count]
                .iter()
                .map(|touch| touch.id)
                .collect(),
            last: sample,
            ..Default::default()
        }
    }
    pub fn deadline(&self) -> Option<MonoTime> {
        self.navigation
            .next_deadline()
            .map(|time| MonoTime(time.as_nanos().min(u128::from(u64::MAX)) as u64))
    }
    /// Retain releases belonging to the native keyboard without dispatching
    /// them to the story after the keyboard closes.
    pub fn suppress(&mut self, sample: native::Sample) {
        self.blocked_buttons = sample.buttons;
        self.blocked_axes = sample.axes.map(|value| value.abs() > 0.2);
        self.blocked_touches.clear();
        self.blocked_touches.extend(
            sample.touches[..sample.touch_count]
                .iter()
                .map(|touch| touch.id),
        );
        self.last = sample;
    }
    fn value(
        &mut self,
        control: Control,
        value: f32,
        now: MonoTime,
        player: &mut Player<&mut GlesBackend>,
    ) -> Result<(), Error> {
        let event = InputEvent::Value {
            device: InputDeviceId::Gamepad(1),
            control,
            value,
        };
        let claimed = player
            .text_event(TextEvent::Input(event.clone()))?
            .is_handled();
        self.events.clear();
        if claimed {
            self.navigation.cancel(|event| self.events.push(event));
        } else {
            self.navigation
                .update(&event, Duration::from_nanos(now.0), |event| {
                    self.events.push(event)
                })?;
        }
        for event in &self.events {
            player.text_event(TextEvent::Navigation(*event))?;
        }
        Ok(())
    }
    pub fn cancel(&mut self, player: &mut Player<&mut GlesBackend>) -> Result<(), Error> {
        self.blocked_buttons = self.last.buttons;
        for (blocked, axis) in self.blocked_axes.iter_mut().zip(self.last.axes) {
            *blocked = axis.abs() > 0.2;
        }
        self.blocked_touches.clear();
        for touch in self.contacts.drain(..) {
            self.blocked_touches.push(touch.id);
            player.dispatch_pointer(PointerEvent::Cancel(pointer(touch.id)))?;
        }
        player.text_event(TextEvent::Input(InputEvent::Cancel))?;
        self.events.clear();
        self.navigation.cancel(|event| self.events.push(event));
        for event in &self.events {
            player.text_event(TextEvent::Navigation(*event))?;
        }
        Ok(())
    }
    pub fn sample(
        &mut self,
        sample: native::Sample,
        now: MonoTime,
        size: [u32; 2],
        player: &mut Player<&mut GlesBackend>,
    ) -> Result<(), Error> {
        if !self.connected {
            player.text_event(TextEvent::Input(InputEvent::Connected {
                device: InputDeviceId::Gamepad(1),
                name: "PlayStation Vita".into(),
            }))?;
            self.connected = true;
        }
        self.blocked_buttons &= sample.buttons;
        let previous = self.last.buttons & !self.blocked_buttons;
        let current = sample.buttons & !self.blocked_buttons;
        for (mask, button) in [
            (native::CROSS, GamepadButton::South),
            (native::CIRCLE, GamepadButton::East),
            (native::SQUARE, GamepadButton::West),
            (native::TRIANGLE, GamepadButton::North),
            (native::L, GamepadButton::LeftShoulder),
            (native::R, GamepadButton::RightShoulder),
            (native::START, GamepadButton::Start),
            (native::SELECT, GamepadButton::Select),
            (native::UP, GamepadButton::DpadUp),
            (native::DOWN, GamepadButton::DpadDown),
            (native::LEFT, GamepadButton::DpadLeft),
            (native::RIGHT, GamepadButton::DpadRight),
        ] {
            if (previous ^ current) & mask != 0 {
                self.value(
                    Control::Button(button),
                    if current & mask != 0 { 1. } else { 0. },
                    now,
                    player,
                )?;
            }
        }
        for (index, axis) in [
            GamepadAxis::LeftX,
            GamepadAxis::LeftY,
            GamepadAxis::RightX,
            GamepadAxis::RightY,
        ]
        .into_iter()
        .enumerate()
        {
            if self.blocked_axes[index] && sample.axes[index].abs() <= 0.2 {
                self.blocked_axes[index] = false;
            }
            if !self.blocked_axes[index] && self.last.axes[index] != sample.axes[index] {
                self.value(Control::Axis(axis), sample.axes[index], now, player)?;
            }
        }
        let touches = &sample.touches[..sample.touch_count];
        self.blocked_touches
            .retain(|id| touches.iter().any(|touch| touch.id == *id));
        for old in &self.contacts {
            if !touches.iter().any(|touch| touch.id == old.id) {
                player.dispatch_pointer(PointerEvent::Up(PointerButtonEvent {
                    pointer: pointer(old.id),
                    state: state(*old, size, false),
                    button: None,
                }))?;
            }
        }
        for touch in touches {
            if self.blocked_touches.contains(&touch.id) {
                continue;
            }
            let pointer = pointer(touch.id);
            if let Some(old) = self.contacts.iter().find(|old| old.id == touch.id) {
                if old != touch {
                    player.dispatch_pointer(PointerEvent::Move(PointerUpdate {
                        pointer,
                        current: state(*touch, size, true),
                        coalesced: Vec::new(),
                        predicted: Vec::new(),
                    }))?;
                }
            } else {
                player.dispatch_pointer(PointerEvent::Down(PointerButtonEvent {
                    pointer,
                    state: state(*touch, size, true),
                    button: None,
                }))?;
            }
        }
        self.contacts.clear();
        self.contacts.extend(
            touches
                .iter()
                .filter(|touch| !self.blocked_touches.contains(&touch.id))
                .copied(),
        );
        self.last = sample;
        self.events.clear();
        self.navigation
            .tick(Duration::from_nanos(now.0), |event| self.events.push(event));
        for event in &self.events {
            player.text_event(TextEvent::Navigation(*event))?;
        }
        Ok(())
    }
}
fn pointer(id: u8) -> PointerInfo {
    PointerInfo {
        pointer_id: PointerId::new(u64::from(id) + 2),
        persistent_device_id: None,
        pointer_type: PointerType::Touch,
    }
}
fn state(touch: native::Touch, size: [u32; 2], active: bool) -> PointerState {
    PointerState {
        position: PhysicalPosition::new(
            f64::from(touch.position[0]) * f64::from(size[0].saturating_sub(1)),
            f64::from(touch.position[1]) * f64::from(size[1].saturating_sub(1)),
        ),
        pressure: if active { touch.force } else { 0. },
        scale_factor: 1.,
        ..Default::default()
    }
}
