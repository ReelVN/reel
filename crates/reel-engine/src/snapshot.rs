//! Session capture moves through each context and copies one retained row per step.
use crate::*;
use aside_vm::snapshot::SnapshotError;

pub(crate) struct SessionCapture<'a> {
    source: &'a Session,
    saved: Session,
    owner: usize,
    context: usize,
    reading: Option<ReadingCapture<'a>>,
}
struct ReadingCapture<'a> {
    source: &'a reading::ReadingData,
    saved: reading::ReadingData,
    line: usize,
    current: usize,
    pending_read: usize,
    group: usize,
}
impl ReadingCapture<'_> {
    fn new(source: &reading::ReadingData) -> ReadingCapture<'_> {
        let active = source.active.as_ref().map(|active| match active {
            Interaction::Dialogue(d) => Interaction::Dialogue(DialogueInteraction {
                previously_read: d.previously_read,
                reveal_skippable: d.reveal_skippable,
                id: d.id,
                mode: d.mode,
                current_line_ids: Vec::new(),
                lines: Vec::new(),
                page_locale: d.page_locale.clone(),
                open: d.open,
                awaiting_advance: d.awaiting_advance,
                revision: d.revision,
                text_revision: d.text_revision,
                pending_read_ids: Vec::new(),
            }),
            Interaction::Choice(c) => Interaction::Choice(ChoiceInteraction {
                id: c.id,
                options: Vec::new(),
            }),
        });
        ReadingCapture {
            source,
            saved: reading::ReadingData {
                active,
                completion: source.completion.clone(),
                history: Default::default(),
                control: source.control,
            },
            line: 0,
            current: 0,
            pending_read: 0,
            group: 0,
        }
    }
    fn step(&mut self) -> bool {
        match (&self.source.active, &mut self.saved.active) {
            (Some(Interaction::Dialogue(source)), Some(Interaction::Dialogue(saved))) => {
                if let Some(id) = source.pending_read_ids.get(self.pending_read) {
                    saved.pending_read_ids.push(id.clone());
                    self.pending_read += 1;
                    return false;
                }
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
        if self
            .saved
            .history
            .share_page(&self.source.history, self.group)
            .is_some()
        {
            self.group += 1;
            return false;
        }
        true
    }
}
impl Session {
    pub(crate) fn snapshot_begin(&self) -> SessionCapture<'_> {
        SessionCapture {
            source: self,
            saved: Session {
                identity: self.identity,
                status: self.status,
                owners: Vec::new(),
                owner_revocation_revision: 0,
                next_owner: self.next_owner,
                next_interaction: self.next_interaction,
                last_input_sequence: None,
                reading_data: reading::ReadingData::default(),
                reading_id: self.reading_id,
                next_reading: self.next_reading,
                parked_readings: Vec::new(),
                auto_delay: self.auto_delay,
                read_progress: ReadProgress::default(),
                history_limits: self.history_limits,
            },
            owner: 0,
            context: 0,
            reading: None,
        }
    }
    pub(crate) fn restore_identity(
        &mut self,
        identity: SessionIdentity,
        progress: ReadProgress,
    ) -> Result<(), SnapshotError> {
        let mut previous = None;
        for id in self
            .parked_readings
            .iter()
            .map(|p| p.id)
            .chain(std::iter::once(self.reading_id))
        {
            if id.0 > i64::MAX as u64
                || previous.is_some_and(|p| p >= id.0)
                || (previous.is_none() && id.0 != 0)
                || self
                    .next_reading
                    .is_some_and(|next| next <= id.0 || next > i64::MAX as u64)
            {
                return Err(SnapshotError::Invalid("reading context identity".into()));
            }
            previous = Some(id.0);
        }
        if self.identity == identity || self.identity.epoch == identity.epoch {
            return Err(SnapshotError::Invalid(
                "session restoration needs a fresh epoch".into(),
            ));
        }
        self.identity = identity;
        self.read_progress = progress;
        self.last_input_sequence = None;
        for owner in &mut self.owners {
            owner.handle.identity = identity;
        }
        for data in std::iter::once(&mut self.reading_data)
            .chain(self.parked_readings.iter_mut().map(|p| &mut p.data))
        {
            restore_reading(data, identity, &mut self.read_progress)?;
        }
        Ok(())
    }
}
fn restore_reading(
    data: &mut reading::ReadingData,
    identity: SessionIdentity,
    progress: &mut ReadProgress,
) -> Result<(), SnapshotError> {
    data.control.set_skip(false, false, false);
    if let Some(completion) = &mut data.completion {
        completion.rebase(identity);
    }
    match &mut data.active {
        Some(Interaction::Dialogue(d)) => {
            progress
                .ids
                .try_reserve(d.lines.len().max(d.pending_read_ids.len()))
                .map_err(|_| SnapshotError::Heap(aside_vm::HeapError::AllocationFailure))?;
            d.id.identity = identity;
            for id in &mut d.current_line_ids {
                id.interaction_id.identity = identity;
            }
            for line in &mut d.lines {
                line.id.interaction_id.identity = identity;
                line.previously_read = progress.contains(&line.content.id);
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
            if d.open && d.lines.len() == 1 && !d.pending_read_ids.is_empty() {
                d.lines[0].previously_read =
                    d.pending_read_ids.iter().all(|id| progress.contains(id));
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
    for group in data.history.iter_mut() {
        group.group_id.0.identity = identity;
        group.interaction_id.identity = identity;
        for line in &mut group.lines {
            line.id.interaction_id.identity = identity;
        }
    }
    Ok(())
}
impl SessionCapture<'_> {
    pub(crate) fn step(&mut self) -> bool {
        if let Some(owner) = self.source.owners.get(self.owner) {
            self.saved.owners.push(owner.clone());
            self.owner += 1;
            return false;
        }
        if self.reading.is_none() {
            let data = if self.context == 0 {
                &self.source.reading_data
            } else if let Some(p) = self.source.parked_readings.get(self.context - 1) {
                &p.data
            } else {
                return true;
            };
            self.reading = Some(ReadingCapture::new(data));
            return false;
        }
        if self.reading.as_mut().unwrap().step() {
            let saved = self.reading.take().unwrap().saved;
            if self.context == 0 {
                self.saved.reading_data = saved;
            } else {
                self.saved.parked_readings.push(reading::ParkedReading {
                    id: self.source.parked_readings[self.context - 1].id,
                    data: saved,
                });
            }
            self.context += 1;
        }
        false
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
