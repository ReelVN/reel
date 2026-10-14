use super::*;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavigationAction {
    Left,
    Right,
    Up,
    Down,
    Confirm,
    Back,
}
impl NavigationAction {
    pub fn is_direction(self) -> bool {
        !matches!(self, Self::Confirm | Self::Back)
    }
}
pub type NavigationEvent = ActionEvent<NavigationAction>;

#[derive(Clone, Copy, Debug)]
pub struct NavigationConfig {
    pub press_threshold: f32,
    pub release_threshold: f32,
    pub repeat_delay: Duration,
    pub repeat_interval: Duration,
}
impl Default for NavigationConfig {
    fn default() -> Self {
        Self {
            press_threshold: 0.55,
            release_threshold: 0.35,
            repeat_delay: Duration::from_millis(350),
            repeat_interval: Duration::from_millis(90),
        }
    }
}
struct State {
    action: NavigationAction,
    held: bool,
    repeat: Option<Duration>,
}
/// Converts action values to navigation edges and scheduled direction repeats.
/// `now` is a host-owned monotonic time. Empty input has no wake deadline.
pub struct NavigationInput {
    map: ActionMap<NavigationAction>,
    config: NavigationConfig,
    states: [State; 6],
}
impl Default for NavigationInput {
    fn default() -> Self {
        use NavigationAction::*;
        let button = |button| Binding::new(Control::Button(button));
        let axis = |axis, scale| Binding {
            control: Control::Axis(axis),
            scale,
            dead_zone: 0.2,
            positive_only: true,
        };
        let map = ActionMap::new([
            (
                Left,
                vec![
                    button(GamepadButton::DpadLeft),
                    axis(GamepadAxis::LeftX, -1.),
                ],
            ),
            (
                Right,
                vec![
                    button(GamepadButton::DpadRight),
                    axis(GamepadAxis::LeftX, 1.),
                ],
            ),
            (
                Up,
                vec![button(GamepadButton::DpadUp), axis(GamepadAxis::LeftY, -1.)],
            ),
            (
                Down,
                vec![
                    button(GamepadButton::DpadDown),
                    axis(GamepadAxis::LeftY, 1.),
                ],
            ),
            (Confirm, vec![button(GamepadButton::South)]),
            (Back, vec![button(GamepadButton::East)]),
        ])
        .expect("default navigation bindings");
        Self::new(map, NavigationConfig::default()).expect("default navigation thresholds")
    }
}
impl NavigationInput {
    pub fn new(
        map: ActionMap<NavigationAction>,
        config: NavigationConfig,
    ) -> Result<Self, InvalidInput> {
        if !config.press_threshold.is_finite()
            || !config.release_threshold.is_finite()
            || !(0. ..=1.).contains(&config.press_threshold)
            || !(0. ..config.press_threshold).contains(&config.release_threshold)
            || config.repeat_interval.is_zero()
        {
            return Err(InvalidInput);
        }
        use NavigationAction::*;
        Ok(Self {
            map,
            config,
            states: [Left, Right, Up, Down, Confirm, Back].map(|action| State {
                action,
                held: false,
                repeat: None,
            }),
        })
    }
    pub fn update(
        &mut self,
        event: &InputEvent,
        now: Duration,
        mut emit: impl FnMut(NavigationEvent),
    ) -> Result<(), InvalidInput> {
        let disconnecting = matches!(event, InputEvent::Disconnected(_));
        let Self {
            map,
            states,
            config,
        } = self;
        map.update(event, |event| {
            let state = states
                .iter_mut()
                .find(|s| s.action == event.action)
                .expect("navigation action");
            let phase = if !state.held && event.value >= config.press_threshold {
                state.held = true;
                state.repeat = state
                    .action
                    .is_direction()
                    .then(|| now.saturating_add(config.repeat_delay));
                Some(ActionPhase::Pressed)
            } else if state.held && event.value <= config.release_threshold {
                state.held = false;
                state.repeat = None;
                Some(if disconnecting || event.phase == ActionPhase::Cancelled {
                    ActionPhase::Cancelled
                } else {
                    ActionPhase::Released
                })
            } else {
                None
            };
            if let Some(phase) = phase {
                emit(ActionEvent { phase, ..event });
            }
        })
    }
    pub fn next_deadline(&self) -> Option<Duration> {
        self.states.iter().filter_map(|s| s.repeat).min()
    }
    /// At most one repeat per held direction per turn; delayed hosts do not burst
    /// through old menu positions to catch up with missed deadlines.
    pub fn tick(&mut self, now: Duration, mut emit: impl FnMut(NavigationEvent)) {
        for state in &mut self.states {
            if state.repeat.is_some_and(|when| now >= when) {
                state.repeat = Some(now.saturating_add(self.config.repeat_interval));
                emit(ActionEvent {
                    action: state.action,
                    value: 1.,
                    phase: ActionPhase::Repeated,
                });
            }
        }
    }
    pub fn cancel(&mut self, mut emit: impl FnMut(NavigationEvent)) {
        self.map.cancel(|_| {});
        for state in &mut self.states {
            if state.held {
                emit(ActionEvent {
                    action: state.action,
                    value: 0.,
                    phase: ActionPhase::Cancelled,
                });
            }
            state.held = false;
            state.repeat = None;
        }
    }
    pub fn replace_bindings(
        &mut self,
        map: ActionMap<NavigationAction>,
        emit: impl FnMut(NavigationEvent),
    ) {
        self.cancel(emit);
        self.map = map;
    }
}
