//! Selection of an accepted frame for bounded, render-only diagnostics.

/// Cache state used when repeatedly submitting an immutable captured frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CachePolicy {
    /// Keep effect results and report the cost of their actual reuse.
    Reuse,
    /// Invalidate effect results before each repetition; keep resource/target pools.
    Replay,
}

impl CachePolicy {
    /// Stable report token for this diagnostic policy.
    pub fn label(self) -> &'static str {
        match self {
            Self::Reuse => "reuse",
            Self::Replay => "replay",
        }
    }
}

/// A fixed-frame probe runs through the ordinary game entry, retains one accepted
/// frame with its resource leases, then stops the session after the repetitions.
/// Selection uses the exact title and a one-based accepted-frame
/// index. Different runs must compare captured command/resource identities before
/// treating the selected frames as the same workload.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrameCaptureOptions {
    /// Exact title emitted by the workload.
    pub label: String,
    /// One-based distinct accepted frame within the matching title.
    pub accepted_frame: u32,
    /// Measured repetitions per batching mode, excluding the capture frame.
    /// Enabling `measure_passes` runs this many batched and serial repetitions.
    pub repetitions: u32,
    /// Whether immutable effect results survive between repetitions.
    pub cache: CachePolicy,
    /// Also repeat the same captured frame with per-pass completion drains.
    /// This changes batching and measures attribution rather than game FPS.
    #[serde(default)]
    pub measure_passes: bool,
    /// Save dimensions and a self-contained retained drawing after measurement.
    /// `frame.postcard` is a same-build developer artifact with shared pixel backing.
    #[serde(default)]
    pub save_frame: bool,
}

impl FrameCaptureOptions {
    /// Validate the bounded diagnostic at its configuration boundary.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.label.is_empty() || self.label.chars().any(char::is_control) {
            return Err("frame-label must be nonempty and contain no control characters");
        }
        if !(1..=10_000).contains(&self.accepted_frame) {
            return Err("frame-index must be in 1..10000");
        }
        if !(1..=16).contains(&self.repetitions) {
            return Err("frame-repeats must be in 1..16");
        }
        Ok(())
    }
}

/// Select once, counting accepted frame identities rather than maintenance loops.
#[cfg(any(target_os = "vita", test))]
pub(crate) struct Selection {
    options: FrameCaptureOptions,
    last_frame: Option<u64>,
    count: u32,
    selected: bool,
}

#[cfg(any(target_os = "vita", test))]
impl Selection {
    pub(crate) fn new(options: FrameCaptureOptions) -> Self {
        Self {
            options,
            last_frame: None,
            count: 0,
            selected: false,
        }
    }

    pub(crate) fn observe(&mut self, title: &str, frame: u64) -> bool {
        if self.selected || title != self.options.label {
            return false;
        }
        if self.last_frame == Some(frame) {
            return false;
        }
        self.last_frame = Some(frame);
        self.count += 1;
        self.selected = self.count == self.options.accepted_frame;
        self.selected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> FrameCaptureOptions {
        FrameCaptureOptions {
            label: "PERF / rune".into(),
            accepted_frame: 2,
            repetitions: 3,
            cache: CachePolicy::Replay,
            measure_passes: true,
            save_frame: false,
        }
    }

    #[test]
    fn repeated_presentations_and_other_scenes_do_not_advance_selection() {
        let mut selection = Selection::new(options());
        assert!(!selection.observe("PERF / other-rune", 1));
        assert!(!selection.observe("LOAD / rune", 1));
        assert!(!selection.observe("PERF / rune", 2));
        assert!(!selection.observe("PERF / rune", 2));
        assert!(!selection.observe("PERF / other", 3));
        assert!(selection.observe("PERF / rune", 4));
        assert!(!selection.observe("PERF / rune", 5));
    }

    #[test]
    fn limits_reject_empty_selection_and_unbounded_recording() {
        let mut config = options();
        assert!(config.validate().is_ok());
        config.accepted_frame = 0;
        assert!(config.validate().is_err());
        config.accepted_frame = 1;
        config.repetitions = 17;
        assert!(config.validate().is_err());
    }
}
