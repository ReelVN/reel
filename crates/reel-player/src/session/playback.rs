//! Presentation-aware text playback shared by all session hosts.
use super::*;
use std::time::Duration;

#[derive(Default)]
pub(super) struct Playback {
    interaction: Option<InteractionId>,
    dialogue_revision: u64,
    last_ns: Option<u64>,
    reading_revision: u64,
    auto_revision: u64,
    auto_last: Option<u64>,
    remaining: Duration,
}

pub(super) fn dialogue_complete(session: &Session) -> bool {
    matches!(session.view().interaction, Some(InteractionView::Dialogue(d)) if d.lines().iter().all(|line| line.playback_complete()))
}

pub(super) fn dialogue_revision(session: &Session) -> u64 {
    match session.view().interaction {
        Some(InteractionView::Dialogue(d)) => d.revision(),
        _ => 0,
    }
}

fn awaiting_advance(session: &Session) -> bool {
    matches!(session.view().interaction, Some(InteractionView::Dialogue(d)) if d.awaiting_advance())
}

fn continues(session: &Session) -> bool {
    matches!(session.view().interaction, Some(InteractionView::Dialogue(d)) if !d.lines().is_empty()
        && d.lines().iter().all(|line| line.content().styled().continue_after()))
}

fn skip_can_advance(session: &Session) -> bool {
    session.reading().skipping()
        && (!dialogue_complete(session) || awaiting_advance(session))
        && matches!(session.view().interaction, Some(InteractionView::Dialogue(d))
            if d.reveal_skippable() || dialogue_complete(session))
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
            && (skip_can_advance(&self.session)
                || !dialogue_complete(&self.session)
                || (awaiting_advance(&self.session)
                    && ((self.session.reading().automatic()
                        && self.session.reading().auto_ready())
                        || continues(&self.session))
                    && accepted.is_some_and(|stamp| stamp.dialogue_complete)))
    }

    pub fn pause_dialogue(&mut self) {
        self.playback.last_ns = None;
        self.playback.auto_last = None;
        self.playback.remaining = self.session.reading().delay();
    }

    /// Supply monotonic logical nanoseconds and finite nonnegative graphemes/sec.
    /// Zero reveals skippable groups immediately; protected groups retain authored
    /// speeds and pauses. Auto advances only after a fully presented group.
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
        let auto_revision = self.session.reading().auto_revision();
        if self.playback.auto_revision != auto_revision {
            self.playback.auto_last = None;
            self.playback.remaining = self.session.reading().delay();
            self.playback.auto_revision = auto_revision;
        }
        if !self.dialogue_needs_time() {
            self.pause_dialogue();
            return Ok(false);
        }
        let id = self.session.view().interaction.unwrap().id();
        let revision = dialogue_revision(&self.session);
        if self.playback.interaction != Some(id) || self.playback.dialogue_revision != revision {
            self.pause_dialogue();
            self.playback.interaction = Some(id);
            self.playback.dialogue_revision = revision;
        }
        if skip_can_advance(&self.session) {
            // A protected reveal must reach an accepted complete frame first.
            let Some(InteractionView::Dialogue(dialogue)) = self.session.view().interaction else {
                unreachable!()
            };
            if !dialogue.reveal_skippable()
                && !self
                    .binding
                    .borrow()
                    .accepted
                    .is_some_and(|stamp| stamp.dialogue_complete)
            {
                return Ok(false);
            }
            let owner = self.session.root_owner();
            if self
                .session
                .handle_playback_input(owner, InputAction::Advance(id))?
                == reel_engine::InputOutcome::Revealed
            {
                self.session
                    .handle_playback_input(owner, InputAction::Advance(id))?;
            }
            self.pause_dialogue();
            return Ok(true);
        }
        if dialogue_complete(&self.session) {
            if continues(&self.session) {
                self.session
                    .handle_playback_input(self.session.root_owner(), InputAction::Advance(id))?;
                self.pause_dialogue();
                return Ok(true);
            }
            let last = self.playback.auto_last.replace(now_ns).unwrap_or(now_ns);
            self.playback.remaining = self
                .playback
                .remaining
                .saturating_sub(Duration::from_nanos(now_ns.saturating_sub(last)));
            if self.playback.remaining.is_zero() {
                self.session
                    .handle_playback_input(self.session.root_owner(), InputAction::Advance(id))?;
                self.pause_dialogue();
                return Ok(true);
            }
            return Ok(false);
        }
        let last = self.playback.last_ns.replace(now_ns).unwrap_or(now_ns);
        self.session
            .advance_reveal(
                self.session.root_owner(),
                id,
                Duration::from_nanos(now_ns.saturating_sub(last)),
                speed,
            )
            .map_err(Into::into)
    }

    pub(crate) fn dialogue_interval(&self, speed: f64) -> Duration {
        if !self.dialogue_needs_time() {
            return Duration::MAX;
        }
        if skip_can_advance(&self.session) {
            return Duration::ZERO;
        }
        if dialogue_complete(&self.session) {
            // First tick establishes the deadline after actual presentation.
            return if continues(&self.session)
                || self.playback.auto_last.is_none()
                || self.playback.auto_revision != self.session.reading().auto_revision()
            {
                Duration::ZERO
            } else {
                self.playback.remaining
            };
        }
        let Some(InteractionView::Dialogue(dialogue)) = self.session.view().interaction else {
            return Duration::MAX;
        };
        dialogue
            .lines()
            .iter()
            .map(|line| line.playback_interval(speed, dialogue.reveal_skippable()))
            .min()
            .unwrap_or(Duration::MAX)
    }
}
