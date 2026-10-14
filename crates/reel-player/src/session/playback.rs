//! Presentation-aware text playback shared by all session hosts.
use super::*;
use std::time::Duration;

#[derive(Default)]
pub(super) struct Playback {
    interaction: Option<InteractionId>,
    last_ns: Option<u64>,
    fraction: f64,
    reading_revision: u64,
    auto_last: Option<u64>,
    remaining: Duration,
}

pub(super) fn dialogue_complete(session: &Session) -> bool {
    matches!(session.view().interaction, Some(InteractionView::Dialogue(d)) if d.lines().iter().all(|line| {
        let r = line.reveal(); r.revealed_count == r.total_count
    }))
}

impl SessionUi {
    /// A pending presentation, menu, pause or paired gesture freezes reading.
    pub fn dialogue_needs_time(&self) -> bool {
        let Some(InteractionView::Dialogue(dialogue)) = self.session.view().interaction else {
            return false;
        };
        let accepted = self.binding.borrow().accepted;
        accepted.is_some_and(|stamp| stamp.interaction == Some(dialogue.id()))
            && !self.images_pending()
            && !self.gesture_active()
            && self.reading_focused
            && !self.reading_modal
            && !self.session.reading().paused()
            && (self.session.reading().skipping()
                || !dialogue_complete(&self.session)
                || (self.session.reading().automatic()
                    && accepted.is_some_and(|stamp| stamp.dialogue_complete)))
    }

    pub fn pause_dialogue(&mut self) {
        self.playback.last_ns = None;
        self.playback.fraction = 0.;
        self.playback.auto_last = None;
        self.playback.remaining = self.session.reading().delay();
    }

    /// Supply monotonic logical nanoseconds and finite nonnegative graphemes/sec.
    /// Zero reveals the full group. Auto advances only after a fully presented group.
    /// Returns whether the interaction or its projection changed.
    pub fn advance_dialogue(&mut self, now_ns: u64, speed: f64) -> Result<bool, Error> {
        if !speed.is_finite() || speed < 0. {
            return Err("text speed must be finite and nonnegative".into());
        }
        let revision = self.session.reading().revision();
        if self.playback.reading_revision != revision {
            self.pause_dialogue();
            self.playback.reading_revision = revision;
        }
        if !self.dialogue_needs_time() {
            self.pause_dialogue();
            return Ok(false);
        }
        let id = self.session.view().interaction.unwrap().id();
        if self.playback.interaction != Some(id) {
            self.pause_dialogue();
            self.playback.interaction = Some(id);
        }
        if self.session.reading().skipping() {
            let owner = self.session.root_owner();
            if self
                .session
                .handle_local_input(owner, InputAction::Advance(id))?
                == reel_engine::InputOutcome::Revealed
            {
                self.session
                    .handle_local_input(owner, InputAction::Advance(id))?;
            }
            self.pause_dialogue();
            return Ok(true);
        }
        if dialogue_complete(&self.session) {
            let last = self.playback.auto_last.replace(now_ns).unwrap_or(now_ns);
            self.playback.remaining = self
                .playback
                .remaining
                .saturating_sub(Duration::from_nanos(now_ns.saturating_sub(last)));
            if self.playback.remaining.is_zero() {
                self.session
                    .handle_local_input(self.session.root_owner(), InputAction::Advance(id))?;
                self.pause_dialogue();
                return Ok(true);
            }
            return Ok(false);
        }
        let last = self.playback.last_ns.replace(now_ns).unwrap_or(now_ns);
        let units = if speed == 0. {
            usize::MAX
        } else {
            self.playback.fraction += now_ns.saturating_sub(last) as f64 * speed / 1_000_000_000.;
            let count = self.playback.fraction.floor() as usize;
            self.playback.fraction -= count as f64;
            count
        };
        self.session
            .reveal_more(self.session.root_owner(), id, units)
            .map_err(Into::into)
    }

    pub(crate) fn dialogue_interval(&self, speed: f64) -> Duration {
        if !self.dialogue_needs_time() {
            return Duration::MAX;
        }
        if self.session.reading().skipping() {
            return Duration::ZERO;
        }
        if dialogue_complete(&self.session) {
            // First tick establishes the deadline after actual presentation.
            return if self.playback.auto_last.is_none() {
                Duration::ZERO
            } else {
                self.playback.remaining
            };
        }
        Duration::from_nanos(if speed == 0. {
            0
        } else {
            ((1. - self.playback.fraction) * 1_000_000_000. / speed).ceil() as u64
        })
    }
}
