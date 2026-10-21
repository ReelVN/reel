//! Incremental authored dialogue shares the existing interaction and history.
use crate::*;
use reel_core::text::Text;

fn joined(prefix: &Text, suffix: &Text) -> Result<Text, SessionError> {
    let rich = prefix.markup().is_some() || suffix.markup().is_some();
    let mut body = String::new();
    for part in [prefix, suffix] {
        let source: &str = part.markup().map_or(part.as_ref(), |value| value.as_ref());
        let extra = if rich && part.markup().is_none() {
            source.bytes().filter(|b| *b == b'[').count()
        } else {
            0
        };
        body.try_reserve(
            source
                .len()
                .checked_add(extra)
                .ok_or(SessionError::AllocationFailed)?,
        )
        .map_err(|_| SessionError::AllocationFailed)?;
        if rich && part.markup().is_none() {
            for ch in source.chars() {
                body.push(ch);
                if ch == '[' {
                    body.push('[');
                }
            }
        } else {
            body.push_str(source);
        }
    }
    if rich {
        Text::parse(body).map_err(|_| SessionError::InvalidTextComposition)
    } else {
        Ok(body.into())
    }
}

impl Session {
    pub fn begin_open_say(
        &mut self,
        owner: OwnerHandle,
        lines: &[DialogueLineInput<'_>],
        locale: &str,
        reveal_skippable: bool,
    ) -> Result<InteractionId, SessionError> {
        let mut pending = reserved_vec(lines.len())?;
        for line in lines {
            pending.push(copy_text(line.content_id)?);
        }
        let id = self.begin_say(owner, lines, locale, reveal_skippable)?;
        let Some(Interaction::Dialogue(dialogue)) = &mut self.active else {
            unreachable!()
        };
        dialogue.open = true;
        dialogue.awaiting_advance = false;
        dialogue.pending_read_ids = pending;
        Ok(id)
    }

    pub(crate) fn open_dialogue(
        &self,
        id: InteractionId,
    ) -> Result<&DialogueInteraction, SessionError> {
        self.ensure_running()?;
        match &self.active {
            Some(Interaction::Dialogue(d)) if d.id == id && d.open && !d.awaiting_advance => Ok(d),
            _ => Err(SessionError::InvalidDialogueHandle),
        }
    }

    pub fn wait_dialogue(
        &mut self,
        owner: OwnerHandle,
        id: InteractionId,
    ) -> Result<(), SessionError> {
        if owner != self.root_owner() || !self.is_owner_live(owner) {
            return Err(SessionError::MainOwnerRequired);
        }
        self.open_dialogue(id)?;
        if self.completion.is_some() {
            return Err(SessionError::CompletionPending);
        }
        let Some(Interaction::Dialogue(d)) = &mut self.active else {
            unreachable!()
        };
        d.awaiting_advance = true;
        d.revision = d.revision.wrapping_add(1);
        Ok(())
    }

    pub fn append_dialogue(
        &mut self,
        owner: OwnerHandle,
        id: InteractionId,
        suffix: &Text,
        content_id: &str,
        history: HistoryInput<'_>,
    ) -> Result<(), SessionError> {
        if owner != self.root_owner() || !self.is_owner_live(owner) {
            return Err(SessionError::MainOwnerRequired);
        }
        let dialogue = self.open_dialogue(id)?;
        if dialogue.lines.len() != 1 {
            return Err(SessionError::SingleDialogueLineRequired);
        }
        if content_id.is_empty() {
            return Err(SessionError::EmptyContentId);
        }
        let line = &dialogue.lines[0];
        let joined = joined(line.content.styled(), suffix)?;
        let mut boundaries = reserved_vec(
            joined
                .graphemes(true)
                .count()
                .checked_add(1)
                .ok_or(SessionError::AllocationFailed)?,
        )?;
        boundaries.push(0);
        boundaries.extend(
            joined
                .grapheme_indices(true)
                .map(|(offset, g)| offset + g.len()),
        );
        // Appended combining marks may extend the last visible grapheme. Keep that
        // whole grapheme visible rather than hiding its already displayed base.
        let revealed = boundaries.partition_point(|&b| b < line.reveal().visible_bytes.end);
        let history_index = self
            .history
            .binary_search_by_key(&id.generation, |g| g.interaction_id.generation)
            .ok();
        let history_text = if let Some(index) = history_index {
            if let Some(previous) = self.history[index].lines.first() {
                match history {
                    HistoryInput::Omit => None,
                    HistoryInput::Display if previous.content.styled() == line.content.styled() => {
                        Some(joined.clone())
                    }
                    HistoryInput::Display => Some(self::joined(previous.content.styled(), suffix)?),
                    HistoryInput::Text(text) => {
                        let text = match text {
                            TextInput::Plain(s) => Text::from(s),
                            TextInput::Styled(t) => t.clone(),
                        };
                        Some(self::joined(previous.content.styled(), &text)?)
                    }
                }
            } else {
                None
            }
        } else {
            None
        };
        let pending_id = copy_text(content_id)?;
        let read = self.read_progress.contains(content_id);
        let pending_count = dialogue.pending_read_ids.len();
        self.read_progress
            .ids
            .try_reserve(
                pending_count
                    .checked_add(1)
                    .ok_or(SessionError::AllocationFailed)?,
            )
            .map_err(|_| SessionError::AllocationFailed)?;
        let Some(Interaction::Dialogue(d)) = &mut self.active else {
            unreachable!()
        };
        d.pending_read_ids
            .try_reserve(1)
            .map_err(|_| SessionError::AllocationFailed)?;
        // All fallible preparation is complete before replacing any visible data.
        d.pending_read_ids.push(pending_id);
        let line = &mut d.lines[0];
        line.previously_read = if pending_count == 0 {
            read
        } else {
            line.previously_read && read
        };
        d.previously_read = line.previously_read;
        line.content.fallback = joined;
        line.grapheme_boundaries = boundaries.into();
        line.revealed_count = revealed;
        d.revision = d.revision.wrapping_add(1);
        if let (Some(index), Some(text)) = (history_index, history_text) {
            self.history[index].lines[0].content.fallback = text;
        }
        d.text_revision = d.text_revision.wrapping_add(1);
        self.check_skip_boundary();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn append_preserves_pending_rich_pause_and_cleared_history() {
        let mut session = Session::new(SessionId(1), Epoch(1)).unwrap();
        let text = Text::parse("[speed=10]A[pause=1]B[/speed]").unwrap();
        let owner = session.root_owner();
        let id = session
            .begin_open_say(
                owner,
                &[DialogueLineInput {
                    speaker_id: None,
                    speaker_name: "",
                    content_id: "first",
                    text: (&text).into(),
                    history: HistoryInput::Display,
                }],
                "en",
                false,
            )
            .unwrap();
        session
            .advance_reveal(owner, id, Duration::from_millis(400), 10.)
            .unwrap();
        session
            .append_dialogue(owner, id, &Text::from("C"), "second", HistoryInput::Display)
            .unwrap();
        session
            .advance_reveal(owner, id, Duration::from_millis(500), 10.)
            .unwrap();
        assert_eq!(
            session.open_dialogue(id).unwrap().lines()[0].visible_text(),
            "A"
        );
        session
            .advance_reveal(owner, id, Duration::from_millis(300), 10.)
            .unwrap();
        assert_eq!(
            session.open_dialogue(id).unwrap().lines()[0].visible_text(),
            "AB"
        );
        session.clear_history(0);
        session
            .append_dialogue(owner, id, &Text::from("D"), "third", HistoryInput::Display)
            .unwrap();
        assert!(session.view().history.is_empty());
        session.wait_dialogue(owner, id).unwrap();
        assert_eq!(
            session
                .handle_local_input(owner, InputAction::Advance(id))
                .unwrap(),
            InputOutcome::Pending
        );
        session
            .advance_reveal(owner, id, Duration::from_secs(1), 10.)
            .unwrap();
        assert_eq!(
            session
                .handle_local_input(owner, InputAction::Advance(id))
                .unwrap(),
            InputOutcome::Completed
        );
        assert_eq!(session.read_progress.ids.len(), 3);
        assert!(session.take_completion().is_some());
        session.cancel();
        assert_eq!(
            session.append_dialogue(
                owner,
                id,
                &Text::from("stale"),
                "fourth",
                HistoryInput::Display
            ),
            Err(SessionError::MainOwnerRequired)
        );
    }
}
