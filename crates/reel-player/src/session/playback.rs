//! Presentation-aware text playback shared by all session hosts.
use super::*;

#[derive(Default)]
pub(super) struct Playback {
    interaction: Option<InteractionId>,
    last_ns: Option<u64>,
    fraction: f64,
}

impl SessionUi {
    /// A pending presentation or a paired gesture freezes the visible text.
    pub fn dialogue_needs_time(&self) -> bool {
        let Some(InteractionView::Dialogue(dialogue)) = self.session.view().interaction else {
            return false;
        };
        self.binding
            .borrow()
            .accepted
            .is_some_and(|stamp| stamp.interaction == Some(dialogue.id()))
            && !self.images_pending()
            && !self.gesture_active()
            && dialogue.lines().iter().any(|line| {
                let r = line.reveal();
                r.revealed_count < r.total_count
            })
    }

    pub fn pause_dialogue(&mut self) {
        self.playback.last_ns = None;
        self.playback.fraction = 0.;
    }

    /// Supply monotonic logical nanoseconds and finite nonnegative graphemes/sec.
    /// Zero reveals the full group. Returns whether the projection changed.
    pub fn advance_dialogue(&mut self, now_ns: u64, speed: f64) -> Result<bool, Error> {
        if !speed.is_finite() || speed < 0. {
            return Err("text speed must be finite and nonnegative".into());
        }
        if !self.dialogue_needs_time() {
            self.pause_dialogue();
            return Ok(false);
        }
        let Some(InteractionView::Dialogue(dialogue)) = self.session.view().interaction else {
            unreachable!()
        };
        let id = dialogue.id();
        if self.playback.interaction != Some(id) {
            self.playback = Playback {
                interaction: Some(id),
                ..Default::default()
            };
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

    pub(crate) fn dialogue_interval(&self, speed: f64) -> std::time::Duration {
        std::time::Duration::from_nanos(if speed == 0. {
            0
        } else {
            ((1. - self.playback.fraction) * 1_000_000_000. / speed).ceil() as u64
        })
    }
}
