//! Session reading policy and portable read-content progress.
use crate::{Completion, Interaction, SessionError};
use std::{collections::HashSet, time::Duration};
pub(crate) mod checkpoint;
mod context;
mod history;
pub(crate) use context::{ParkedReading, ReadingId};
pub use history::HistoryLog;

/// Live history retention for every reading context in a session. Zero lines
/// disables history retention. The displayed dialogue remains independent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryLimits {
    /// Maximum retained dialogue lines; the default is 200.
    pub max_lines: usize,
    /// Maximum logical history storage; the default is 1 MiB. A single newest
    /// line larger than this allowance remains readable until replaced.
    pub max_bytes: usize,
}
impl Default for HistoryLimits {
    fn default() -> Self {
        Self {
            max_lines: 200,
            max_bytes: 1024 * 1024,
        }
    }
}

impl crate::Session {
    /// Retain at most the final `keep_last` recorded lines, preserving original
    /// group/line IDs even when the boundary cuts a group. Zero clears all.
    /// Display, input, read progress and previously saved snapshots are independent.
    /// This explicit operation synchronously releases discarded storage.
    pub fn clear_history(&mut self, keep_last: usize) {
        if keep_last == 0 {
            self.reading_data.history = HistoryLog::default();
            return;
        }
        let count: usize = self
            .reading_data
            .history
            .iter()
            .map(|group| group.lines.len())
            .sum();
        if keep_last >= count {
            return;
        }
        let mut remaining = keep_last;
        let mut first = self.reading_data.history.len();
        while first != 0 {
            first -= 1;
            let count = self.reading_data.history[first].lines.len();
            if count >= remaining {
                if count > remaining {
                    self.reading_data.history.edit(first, |group| {
                        group.lines.drain(..count - remaining);
                        group.lines.shrink_to_fit();
                    });
                }
                break;
            }
            remaining -= count;
        }
        self.reading_data.history.retain_from(first);
    }

    /// Current retention policy, shared by foreground and suspended readings.
    pub fn history_limits(&self) -> HistoryLimits {
        self.history_limits
    }

    /// Apply the current application's history policy and trim existing readings.
    /// Checkpoints restore within this policy after current Root configuration.
    pub fn set_history_limits(&mut self, limits: HistoryLimits) {
        self.history_limits = limits;
        self.reading_data.history.enforce_limits(limits);
        for reading in &mut self.parked_readings {
            reading.data.history.enforce_limits(limits);
        }
    }
}

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct ReadingData {
    pub active: Option<Interaction>,
    pub completion: Option<Completion>,
    pub history: HistoryLog,
    pub control: ReadingControl,
}

/// Independent of session display IDs. Hosts may persist these content IDs.
#[derive(Clone, Debug, Default)]
pub struct ReadProgress {
    pub(crate) ids: HashSet<String>,
}
impl ReadProgress {
    pub fn from_ids(ids: impl IntoIterator<Item = String>) -> Result<Self, SessionError> {
        let mut progress = Self::default();
        for id in ids {
            progress
                .ids
                .try_reserve(1)
                .map_err(|_| SessionError::AllocationFailed)?;
            progress.ids.insert(id);
        }
        Ok(progress)
    }
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.ids.iter().map(String::as_str)
    }
    pub fn contains(&self, content_id: &str) -> bool {
        self.ids.contains(content_id)
    }
    pub(crate) fn storage_bytes(&self) -> usize {
        self.ids.capacity() * (std::mem::size_of::<String>() + 1)
            + self.ids.iter().map(String::capacity).sum::<usize>()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum Mode {
    #[default]
    Manual,
    Auto,
    Skip {
        unread: bool,
        after_choices: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ReadingControl {
    mode: Mode,
    paused: bool,
    revision: u64,
    auto_ready: bool,
    auto_revision: u64,
}
impl Default for ReadingControl {
    fn default() -> Self {
        Self {
            mode: Mode::Manual,
            paused: false,
            revision: 0,
            auto_ready: true,
            auto_revision: 0,
        }
    }
}
impl ReadingControl {
    /// Whether the author condition permits the automatic countdown.
    pub fn auto_ready(self) -> bool {
        self.auto_ready
    }
    /// Condition changes are independent of the printing clock.
    pub fn auto_revision(self) -> u64 {
        self.auto_revision
    }
    pub(crate) fn set_auto_condition(&mut self, ready: bool) {
        if self.auto_ready != ready {
            self.auto_ready = ready;
            self.auto_revision = self.auto_revision.wrapping_add(1);
        }
    }
    pub fn automatic(self) -> bool {
        self.mode == Mode::Auto
    }
    pub fn skipping(self) -> bool {
        matches!(self.mode, Mode::Skip { .. })
    }
    pub fn skip_unread(self) -> bool {
        matches!(self.mode, Mode::Skip { unread: true, .. })
    }
    pub fn skip_after_choices(self) -> bool {
        matches!(
            self.mode,
            Mode::Skip {
                after_choices: true,
                ..
            }
        )
    }
    pub fn paused(self) -> bool {
        self.paused
    }
    /// Changes even when a pause/resume pair occurs between host clock ticks.
    pub fn revision(self) -> u64 {
        self.revision
    }
    pub(crate) fn changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }
    pub(crate) fn set_auto(&mut self, enabled: bool) {
        let mode = if enabled {
            Mode::Auto
        } else if self.automatic() {
            Mode::Manual
        } else {
            self.mode
        };
        if self.mode != mode {
            self.mode = mode;
            self.changed();
        }
    }
    pub(crate) fn set_skip(&mut self, enabled: bool, unread: bool, after_choices: bool) {
        let mode = if enabled && !self.paused {
            Mode::Skip {
                unread,
                after_choices,
            }
        } else if self.skipping() {
            Mode::Manual
        } else {
            self.mode
        };
        if self.mode != mode {
            self.mode = mode;
            self.changed();
        }
    }
    pub(crate) fn pause(&mut self, paused: bool) {
        if self.paused != paused {
            self.paused = paused;
            self.changed();
        }
        if paused {
            self.set_skip(false, false, false);
        }
    }
    pub(crate) fn activate_after(&mut self, previous: Self) {
        self.revision = previous.revision.wrapping_add(1);
        self.auto_revision = previous.auto_revision.wrapping_add(1);
    }
}

/// The selected reading context's controls with the session's shared delay.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReadingState {
    control: ReadingControl,
    delay: Duration,
}
impl Default for ReadingState {
    fn default() -> Self {
        Self::new(ReadingControl::default(), Duration::from_secs(2))
    }
}
impl ReadingState {
    pub(crate) fn new(control: ReadingControl, delay: Duration) -> Self {
        Self { control, delay }
    }
    /// Whether the author condition permits automatic advancement.
    pub fn auto_ready(self) -> bool {
        self.control.auto_ready()
    }
    /// Revision of the automatic advancement condition.
    pub fn auto_revision(self) -> u64 {
        self.control.auto_revision()
    }
    /// Whether automatic advancement is enabled in this context.
    pub fn automatic(self) -> bool {
        self.control.automatic()
    }
    /// Whether skipping is enabled in this context.
    pub fn skipping(self) -> bool {
        self.control.skipping()
    }
    /// Whether skipping may advance unread dialogue.
    pub fn skip_unread(self) -> bool {
        self.control.skip_unread()
    }
    /// Whether skipping stays enabled across a choice.
    pub fn skip_after_choices(self) -> bool {
        self.control.skip_after_choices()
    }
    /// Whether authored reading controls pause this context.
    pub fn paused(self) -> bool {
        self.control.paused()
    }
    /// Shared automatic advancement delay after complete presentation.
    pub fn delay(self) -> Duration {
        self.delay
    }
    /// Changes on control updates and context activation.
    pub fn revision(self) -> u64 {
        self.control.revision()
    }
}
