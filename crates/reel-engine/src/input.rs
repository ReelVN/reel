//! Session input sequencing and interaction completion.
use super::{
    CompletionOutcome, InputAction, InputEvent, InputOutcome, InputRejection, InputSequence,
    Interaction, OwnerHandle, Session, SessionStatus,
};

impl Session {
    /// Process each live same-session sequence at most once, even if its action is
    /// stale or invalid. Foreign/dead sources do not change the high-water mark.
    pub fn handle_input(&mut self, event: InputEvent) -> Result<InputOutcome, InputRejection> {
        self.validate_input_source(event.source_owner)?;
        self.apply_sequenced_input(event, true)
    }

    /// Dispatch one new local action, assigning its sequence from this Session's
    /// shared watermark under the same exclusive borrow as processing the event.
    /// This follows every prior live-source event, including stale/invalid actions
    /// submitted through `handle_input`; UI attachment or replacement never resets
    /// the sequence. Exhaustion rejects explicitly without wrapping or mutation.
    ///
    /// Use `handle_input` for events that already carry a sequence, retaining that
    /// original sequence on replay. Replaying through this entry would create a
    /// new event instead of checking whether the original was already consumed.
    pub fn handle_local_input(
        &mut self,
        source_owner: OwnerHandle,
        action: InputAction,
    ) -> Result<InputOutcome, InputRejection> {
        self.handle_local_input_origin(source_owner, action, true)
    }

    /// Dispatch playback-driven input without recording a manual advance request.
    pub fn handle_playback_input(
        &mut self,
        source_owner: OwnerHandle,
        action: InputAction,
    ) -> Result<InputOutcome, InputRejection> {
        self.handle_local_input_origin(source_owner, action, false)
    }

    fn handle_local_input_origin(
        &mut self,
        source_owner: OwnerHandle,
        action: InputAction,
        manual: bool,
    ) -> Result<InputOutcome, InputRejection> {
        self.validate_input_source(source_owner)?;
        let sequence = match self.last_input_sequence {
            Some(last) => InputSequence(
                last.0
                    .checked_add(1)
                    .ok_or(InputRejection::SequenceExhausted)?,
            ),
            None => InputSequence(1),
        };
        self.apply_sequenced_input(
            InputEvent {
                source_owner,
                sequence,
                action,
            },
            manual,
        )
    }

    fn validate_input_source(&self, source_owner: OwnerHandle) -> Result<(), InputRejection> {
        if self.status != SessionStatus::Running {
            return Err(InputRejection::SessionNotRunning);
        }
        if source_owner.identity != self.identity {
            return Err(InputRejection::ForeignOwner);
        }
        if !self.is_owner_live(source_owner) {
            return Err(InputRejection::OwnerNotLive);
        }
        Ok(())
    }

    fn apply_sequenced_input(
        &mut self,
        event: InputEvent,
        manual: bool,
    ) -> Result<InputOutcome, InputRejection> {
        if self
            .last_input_sequence
            .is_some_and(|last| event.sequence <= last)
        {
            return Err(InputRejection::DuplicateOrOutOfOrder);
        }
        self.last_input_sequence = Some(event.sequence);
        if self.reading_data.control.paused() {
            return Err(InputRejection::ReadingPaused);
        }
        let active = self
            .reading_data
            .active
            .as_mut()
            .ok_or(InputRejection::NoActiveInteraction)?;
        let target = match &event.action {
            InputAction::Advance(id) => *id,
            InputAction::Select(id) => id.interaction_id,
        };
        if target != active.id() {
            return Err(InputRejection::StaleInteraction);
        }
        match (active, event.action) {
            (Interaction::Dialogue(dialogue), InputAction::Advance(_)) => {
                if !dialogue.reveal_skippable
                    && dialogue.lines.iter().any(|line| !line.playback_complete())
                {
                    return Ok(InputOutcome::Pending);
                }
                let mut revealed = false;
                for line in &mut dialogue.lines {
                    if manual {
                        line.advance_requested = true;
                    }
                    let total_count = line.grapheme_boundaries.len() - 1;
                    if !line.playback_complete() {
                        line.revealed_count = total_count;
                        line.skip_timing();
                        revealed = true;
                    }
                }
                if revealed {
                    return Ok(InputOutcome::Revealed);
                }
                if !dialogue.awaiting_advance {
                    return Ok(InputOutcome::Pending);
                }
                self.complete(CompletionOutcome::Advanced);
            }
            (Interaction::Choice(choice), InputAction::Select(id)) => {
                let selected = choice
                    .options
                    .iter_mut()
                    .find(|option| option.id == id)
                    .ok_or(InputRejection::UnknownChoice)?;
                // Move the reserved key; no terminal allocation or duplicate copy.
                let key = std::mem::take(&mut selected.id.key);
                self.complete(CompletionOutcome::Selected(key));
            }
            _ => return Err(InputRejection::WrongInteractionKind),
        }
        Ok(InputOutcome::Completed)
    }
}
