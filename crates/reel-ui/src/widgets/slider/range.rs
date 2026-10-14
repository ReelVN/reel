// Copyright 2026 the Reel Authors
// SPDX-License-Identifier: Apache-2.0
//! Shared numeric contract for Rust and Aside controls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SliderRange {
    min: f64,
    max: f64,
    step: Option<f64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidSliderRange;
impl std::fmt::Display for InvalidSliderRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "slider requires a finite increasing range and an optional finite positive step",
        )
    }
}
impl std::error::Error for InvalidSliderRange {}
impl SliderRange {
    pub fn new(min: f64, max: f64, step: Option<f64>) -> Result<Self, InvalidSliderRange> {
        if !min.is_finite()
            || !max.is_finite()
            || min >= max
            || !(max - min).is_finite()
            || step.is_some_and(|s| !s.is_finite() || s <= 0.)
        {
            return Err(InvalidSliderRange);
        }
        Ok(Self { min, max, step })
    }
    pub fn min(self) -> f64 {
        self.min
    }
    pub fn max(self) -> f64 {
        self.max
    }
    pub fn step(self) -> Option<f64> {
        self.step
    }
    pub fn contains(self, value: f64) -> bool {
        value.is_finite() && (self.min..=self.max).contains(&value)
    }
    /// Normalize user input. Programmatic values need only satisfy `contains`.
    pub fn snap(self, value: f64) -> f64 {
        let value = value.clamp(self.min, self.max);
        if value == self.min || value == self.max {
            return value;
        }
        self.step.map_or(value, |step| {
            let offset = value - self.min;
            // Remainder avoids overflowing offset / step for very small steps.
            let remainder = offset % step;
            let snapped = if remainder < step * 0.5 {
                value - remainder
            } else {
                value + (step - remainder)
            };
            snapped.clamp(self.min, self.max)
        })
    }
    pub fn increment(self, value: f64, steps: f64) -> f64 {
        let step = self.step.unwrap_or((self.max - self.min) / 100.);
        self.snap(value + step * steps)
    }
    pub fn progress(self, value: f64) -> f64 {
        (value - self.min) / (self.max - self.min)
    }
    pub fn at_progress(self, progress: f64) -> f64 {
        self.snap(self.min + progress.clamp(0., 1.) * (self.max - self.min))
    }
}

/// Thumb centers used for both drawing and inverse pointer mapping.
pub(super) fn endpoints(width: f64, radius: f64) -> (f64, f64) {
    let inset = radius.min(width * 0.5).max(0.);
    (inset, (width - inset).max(inset))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn negative_and_offset_ranges_share_anchored_steps_and_reachable_endpoints() {
        let range = SliderRange::new(-3., 8., Some(2.)).unwrap();
        assert_eq!(range.snap(0.), 1.);
        assert_eq!(range.snap(-2.5), -3.);
        assert_eq!(range.increment(7., 1.), 8.);
        assert_eq!(range.increment(-3., -1.), -3.);
        assert_eq!(range.at_progress(1.), 8.);
        let tiny = SliderRange::new(0., 1., Some(f64::MIN_POSITIVE)).unwrap();
        assert_eq!(tiny.snap(0.5), 0.5);
        for range in [
            (0., 0., None),
            (1., 0., None),
            (-f64::MAX, f64::MAX, None),
            (0., 1., Some(f64::NAN)),
        ] {
            assert!(SliderRange::new(range.0, range.1, range.2).is_err());
        }
        assert_eq!(endpoints(100., 10.), (10., 90.));
        assert_eq!(endpoints(10., 10.), (5., 5.));
        assert_eq!(endpoints(0., 10.), (0., 0.));
    }
}
