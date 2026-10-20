//! Session data capture shares immutable text and copies one row per step.
use crate::*;
use aside_vm::snapshot::SnapshotError;

pub(crate) struct SessionCapture<'a> {
    source: &'a Session,
    saved: Session,
    owner: usize,
    line: usize,
    current: usize,
    group: usize,
    history_line: usize,
}
impl Session {
    pub(crate) fn snapshot_begin(&self) -> SessionCapture<'_> {
        let active = self.active.as_ref().map(|active| match active {
            Interaction::Dialogue(d) => Interaction::Dialogue(DialogueInteraction {
                previously_read: d.previously_read,
                reveal_skippable: d.reveal_skippable,
                id: d.id,
                mode: d.mode,
                current_line_ids: Vec::new(),
                lines: Vec::new(),
                page_locale: d.page_locale.clone(),
            }),
            Interaction::Choice(c) => Interaction::Choice(ChoiceInteraction {
                id: c.id,
                options: Vec::new(),
            }),
        });
        SessionCapture {
            source: self,
            saved: Session {
                identity: self.identity,
                status: self.status,
                owners: Vec::new(),
                next_owner: self.next_owner,
                next_interaction: self.next_interaction,
                last_input_sequence: None,
                active,
                completion: self.completion.clone(),
                history: Vec::new(),
                reading: self.reading,
                read_progress: ReadProgress::default(),
            },
            owner: 0,
            line: 0,
            current: 0,
            group: 0,
            history_line: 0,
        }
    }
    pub(crate) fn restore_identity(
        &mut self,
        identity: SessionIdentity,
        progress: ReadProgress,
    ) -> Result<(), SnapshotError> {
        if self.identity == identity || self.identity.epoch == identity.epoch {
            return Err(SnapshotError::Invalid(
                "session restoration needs a fresh epoch".into(),
            ));
        }
        self.identity = identity;
        self.read_progress = progress;
        self.last_input_sequence = None;
        self.reading.set_skip(false, false, false);
        for owner in &mut self.owners {
            owner.handle.identity = identity;
        }
        if let Some(completion) = &mut self.completion {
            completion.rebase(identity);
        }
        match &mut self.active {
            Some(Interaction::Dialogue(d)) => {
                self.read_progress
                    .ids
                    .try_reserve(d.lines.len())
                    .map_err(|_| SnapshotError::Heap(aside_vm::HeapError::AllocationFailure))?;
                d.id.identity = identity;
                for id in &mut d.current_line_ids {
                    id.interaction_id.identity = identity;
                }
                for line in &mut d.lines {
                    line.id.interaction_id.identity = identity;
                    line.previously_read = self.read_progress.contains(&line.content.id);
                    let text = line.resolved_text();
                    if line.grapheme_boundaries.first() != Some(&0)
                        || line.grapheme_boundaries.last() != Some(&text.len())
                        || line.revealed_count >= line.grapheme_boundaries.len()
                        || !line
                            .grapheme_boundaries
                            .iter()
                            .all(|&b| text.is_char_boundary(b))
                    {
                        return Err(SnapshotError::Invalid("dialogue reveal boundaries".into()));
                    }
                }
                d.previously_read = d.lines.iter().all(DialogueLine::previously_read);
            }
            Some(Interaction::Choice(c)) => {
                c.id.identity = identity;
                for option in &mut c.options {
                    option.id.interaction_id.identity = identity;
                }
            }
            None => (),
        }
        for group in &mut self.history {
            group.group_id.0.identity = identity;
            group.interaction_id.identity = identity;
            for line in &mut group.lines {
                line.id.interaction_id.identity = identity;
            }
        }
        Ok(())
    }
}
impl SessionCapture<'_> {
    pub(crate) fn step(&mut self) -> bool {
        if let Some(owner) = self.source.owners.get(self.owner) {
            self.saved.owners.push(owner.clone());
            self.owner += 1;
            return false;
        }
        match (&self.source.active, &mut self.saved.active) {
            (Some(Interaction::Dialogue(source)), Some(Interaction::Dialogue(saved))) => {
                if let Some(line) = source.lines.get(self.line) {
                    saved.lines.push(line.clone());
                    self.line += 1;
                    return false;
                }
                if let Some(id) = source.current_line_ids.get(self.current) {
                    saved.current_line_ids.push(*id);
                    self.current += 1;
                    return false;
                }
            }
            (Some(Interaction::Choice(source)), Some(Interaction::Choice(saved))) => {
                if let Some(option) = source.options.get(self.line) {
                    saved.options.push(option.clone());
                    self.line += 1;
                    return false;
                }
            }
            _ => (),
        }
        if let Some(group) = self.source.history.get(self.group) {
            if self.saved.history.len() == self.group {
                self.saved.history.push(HistoryGroup {
                    group_id: group.group_id,
                    interaction_id: group.interaction_id,
                    lines: Vec::new(),
                    page_locale: group.page_locale.clone(),
                });
                return false;
            }
            if let Some(line) = group.lines.get(self.history_line) {
                self.saved.history[self.group].lines.push(line.clone());
                self.history_line += 1;
            } else {
                self.group += 1;
                self.history_line = 0;
            }
            return false;
        }
        true
    }
    pub(crate) fn finish(self) -> Session {
        self.saved
    }
}
impl OwnerHandle {
    pub(crate) fn rebase(&mut self, identity: SessionIdentity) {
        self.identity = identity;
    }
}
impl InteractionId {
    pub(crate) fn rebase(&mut self, identity: SessionIdentity) {
        self.identity = identity;
    }
}
impl ChoiceId {
    pub(crate) fn rebase(&mut self, identity: SessionIdentity) {
        self.interaction_id.identity = identity;
    }
}
impl Completion {
    pub(crate) fn rebase(&mut self, identity: SessionIdentity) {
        self.interaction_id.identity = identity;
        self.owner.identity = identity;
    }
}
