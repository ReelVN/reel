// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0

//! Portable controls and action state. No platform handles or event loop dependencies.
use crate::core::{PointerButton, keyboard::Code};
mod navigation;
pub use navigation::*;
mod hints;
pub use hints::{GamepadLayout, InputHints, InputSource, InputTracker};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InputDeviceId {
    Keyboard,
    Pointer(crate::core::PointerInfo),
    Gamepad(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GamepadButton {
    South,
    East,
    North,
    West,
    C,
    Z,
    LeftShoulder,
    RightShoulder,
    LeftTrigger,
    RightTrigger,
    Select,
    Start,
    Mode,
    LeftThumb,
    RightThumb,
    DpadUp,
    DpadDown,
    DpadLeft,
    DpadRight,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GamepadAxis {
    LeftX,
    LeftY,
    RightX,
    RightY,
    LeftZ,
    RightZ,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Control {
    Key(Code),
    Pointer(PointerButton),
    Button(GamepadButton),
    Axis(GamepadAxis),
}
#[derive(Clone, Debug, PartialEq)]
pub enum InputEvent {
    Connected {
        device: InputDeviceId,
        name: String,
        layout: GamepadLayout,
    },
    Disconnected(InputDeviceId),
    /// Revoke all held controls at an input ownership boundary.
    Cancel,
    Value {
        device: InputDeviceId,
        control: Control,
        value: f32,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionPhase {
    Pressed,
    Changed,
    Repeated,
    Released,
    Cancelled,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ActionEvent<A> {
    pub action: A,
    pub value: f32,
    pub phase: ActionPhase,
}

/// Scale is applied before clipping to the positive half, for directional bindings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Binding {
    pub control: Control,
    pub scale: f32,
    pub dead_zone: f32,
    pub positive_only: bool,
}
impl Binding {
    pub fn new(control: Control) -> Self {
        Self {
            control,
            scale: 1.,
            dead_zone: 0.,
            positive_only: false,
        }
    }
    /// Whether scale and dead zone define a finite, normalized control mapping.
    pub fn is_valid(&self) -> bool {
        self.scale.is_finite() && self.dead_zone.is_finite() && (0. ..1.).contains(&self.dead_zone)
    }
    fn value(self, value: f32) -> f32 {
        let value = if value.abs() <= self.dead_zone {
            0.
        } else {
            value.signum() * (value.abs() - self.dead_zone) / (1. - self.dead_zone)
        } * self.scale;
        if self.positive_only {
            value.max(0.)
        } else {
            value
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidInput;
impl std::fmt::Display for InvalidInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("input values must be normalized and binding scales/dead zones must be finite")
    }
}
impl std::error::Error for InvalidInput {}

struct Action<A> {
    id: A,
    bindings: Vec<Binding>,
    value: f32,
}
/// Values remain held until their source releases or disconnects. Storage follows
/// active controls; repeated updates reuse it and never allocate an event queue.
pub struct ActionMap<A> {
    actions: Vec<Action<A>>,
    held: Vec<(InputDeviceId, Control, f32)>,
}
impl<A: Copy + Eq> ActionMap<A> {
    pub fn new(actions: impl IntoIterator<Item = (A, Vec<Binding>)>) -> Result<Self, InvalidInput> {
        let mut result = Self {
            actions: Vec::new(),
            held: Vec::new(),
        };
        for (id, bindings) in actions {
            if result.actions.iter().any(|a| a.id == id)
                || bindings.iter().any(|binding| !binding.is_valid())
            {
                return Err(InvalidInput);
            }
            result.actions.push(Action {
                id,
                bindings,
                value: 0.,
            });
        }
        Ok(result)
    }
    pub fn binds(&self, control: Control) -> bool {
        self.actions
            .iter()
            .any(|a| a.bindings.iter().any(|b| b.control == control))
    }
    pub fn value(&self, action: A) -> f32 {
        self.actions
            .iter()
            .find(|a| a.id == action)
            .map_or(0., |a| a.value)
    }
    pub fn update(
        &mut self,
        event: &InputEvent,
        mut emit: impl FnMut(ActionEvent<A>),
    ) -> Result<(), InvalidInput> {
        let cancelled = match event {
            InputEvent::Cancel => {
                self.cancel(emit);
                return Ok(());
            }
            InputEvent::Connected { .. } => return Ok(()),
            InputEvent::Disconnected(device) => {
                self.held.retain(|(id, _, _)| id != device);
                true
            }
            InputEvent::Value {
                device,
                control,
                value,
            } => {
                if !value.is_finite() || !(-1. ..=1.).contains(value) {
                    return Err(InvalidInput);
                }
                if !self
                    .actions
                    .iter()
                    .any(|a| a.bindings.iter().any(|b| b.control == *control))
                {
                    return Ok(());
                }
                let existing = self
                    .held
                    .iter()
                    .position(|(d, c, _)| d == device && c == control);
                match (existing, *value == 0.) {
                    (Some(i), true) => {
                        self.held.swap_remove(i);
                    }
                    (Some(i), false) => self.held[i].2 = *value,
                    (None, false) => self.held.push((*device, *control, *value)),
                    (None, true) => return Ok(()),
                }
                false
            }
        };
        for action in &mut self.actions {
            let value = action
                .bindings
                .iter()
                .flat_map(|b| {
                    self.held
                        .iter()
                        .filter(move |(_, c, _)| *c == b.control)
                        .map(move |(_, _, v)| b.value(*v))
                })
                .sum::<f32>()
                .clamp(-1., 1.);
            if value == action.value {
                continue;
            }
            let phase = if value == 0. {
                if cancelled {
                    ActionPhase::Cancelled
                } else {
                    ActionPhase::Released
                }
            } else if action.value == 0. {
                ActionPhase::Pressed
            } else {
                ActionPhase::Changed
            };
            action.value = value;
            emit(ActionEvent {
                action: action.id,
                value,
                phase,
            });
        }
        Ok(())
    }
    pub fn cancel(&mut self, mut emit: impl FnMut(ActionEvent<A>)) {
        self.held.clear();
        for action in &mut self.actions {
            if action.value != 0. {
                action.value = 0.;
                emit(ActionEvent {
                    action: action.id,
                    value: 0.,
                    phase: ActionPhase::Cancelled,
                });
            }
        }
    }
}

impl std::str::FromStr for Control {
    type Err = InvalidInput;
    fn from_str(name: &str) -> Result<Self, Self::Err> {
        use GamepadAxis as A;
        use GamepadButton as B;
        Ok(match name {
            "GamepadConfirm" => DEFAULT_CONFIRM_CONTROL,
            "GamepadBack" => DEFAULT_BACK_CONTROL,
            "South" => Self::Button(B::South),
            "East" => Self::Button(B::East),
            "North" => Self::Button(B::North),
            "West" => Self::Button(B::West),
            "C" => Self::Button(B::C),
            "Z" => Self::Button(B::Z),
            "LeftShoulder" => Self::Button(B::LeftShoulder),
            "RightShoulder" => Self::Button(B::RightShoulder),
            "LeftTrigger" => Self::Button(B::LeftTrigger),
            "RightTrigger" => Self::Button(B::RightTrigger),
            "Select" => Self::Button(B::Select),
            "Start" => Self::Button(B::Start),
            "Mode" => Self::Button(B::Mode),
            "LeftThumb" => Self::Button(B::LeftThumb),
            "RightThumb" => Self::Button(B::RightThumb),
            "DpadUp" => Self::Button(B::DpadUp),
            "DpadDown" => Self::Button(B::DpadDown),
            "DpadLeft" => Self::Button(B::DpadLeft),
            "DpadRight" => Self::Button(B::DpadRight),
            "LeftX" => Self::Axis(A::LeftX),
            "LeftY" => Self::Axis(A::LeftY),
            "RightX" => Self::Axis(A::RightX),
            "RightY" => Self::Axis(A::RightY),
            "LeftZ" => Self::Axis(A::LeftZ),
            "RightZ" => Self::Axis(A::RightZ),
            "PointerPrimary" => Self::Pointer(PointerButton::Primary),
            "PointerSecondary" => Self::Pointer(PointerButton::Secondary),
            "PointerAuxiliary" => Self::Pointer(PointerButton::Auxiliary),
            "PointerX1" => Self::Pointer(PointerButton::X1),
            "PointerX2" => Self::Pointer(PointerButton::X2),
            _ => Self::Key(name.parse().map_err(|_| InvalidInput)?),
        })
    }
}
