use std::time::Duration;

/// Source-content speed and a linear transition measured in active playback time.
/// Pitch changes with speed. Bounds limit source-frame work per output sample.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AudioRate {
    /// Current source seconds consumed per output second.
    pub value: f64,
    /// Destination of the current linear transition.
    pub target: f64,
    /// Active playback time left in the transition.
    pub remaining: Duration,
}
impl AudioRate {
    /// Minimum supported source rate.
    pub const MIN: f64 = 0.25;
    /// Maximum supported source rate, bounding resampling work.
    pub const MAX: f64 = 4.0;

    /// Describe a constant rate. Validate author or restored input at admission.
    pub const fn constant(value: f64) -> Self {
        Self {
            value,
            target: value,
            remaining: Duration::ZERO,
        }
    }

    /// Whether both rates and the terminal transition state are valid.
    pub fn is_valid(self) -> bool {
        (Self::MIN..=Self::MAX).contains(&self.value)
            && (Self::MIN..=Self::MAX).contains(&self.target)
            && (!self.remaining.is_zero() || self.value == self.target)
    }

    /// Replace the transition, preserving the current value for a timed change.
    pub fn change(self, target: f64, duration: Duration) -> Self {
        Self {
            value: if duration.is_zero() {
                target
            } else {
                self.value
            },
            target,
            remaining: duration,
        }
    }
}
impl Default for AudioRate {
    fn default() -> Self {
        Self::constant(1.)
    }
}
