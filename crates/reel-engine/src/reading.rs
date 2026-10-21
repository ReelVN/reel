//! Session reading policy and portable read-content progress.
use crate::SessionError;
use std::{collections::HashSet, time::Duration};

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
pub struct ReadingState {
    mode: Mode,
    paused: bool,
    delay: Duration,
    revision: u64,
    auto_ready: bool,
    auto_revision: u64,
}
impl Default for ReadingState {
    fn default() -> Self {
        Self {
            mode: Mode::Manual,
            paused: false,
            delay: Duration::from_secs(2),
            revision: 0,
            auto_ready: true,
            auto_revision: 0,
        }
    }
}
impl ReadingState {
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
    pub fn delay(self) -> Duration {
        self.delay
    }
    /// Changes even when a pause/resume pair occurs between host clock ticks.
    pub fn revision(self) -> u64 {
        self.revision
    }
    fn changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }
    pub(crate) fn set_auto(&mut self, enabled: bool, delay: Option<Duration>) {
        let delay = delay.unwrap_or(self.delay);
        let mode = if enabled {
            Mode::Auto
        } else if self.automatic() {
            Mode::Manual
        } else {
            self.mode
        };
        if self.mode != mode || self.delay != delay {
            self.mode = mode;
            self.delay = delay;
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
}
