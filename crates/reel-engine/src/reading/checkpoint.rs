//! Portable reading contents; executor and observable identities are rebuilt.
#[cfg(test)]
mod tests;
use super::*;
use crate::{
    ChoiceInteraction, ChoiceOption, DialogueInteraction, DialogueLine, Session, SessionIdentity,
    TextContent,
};

/// Submitted display rows retain their original text independently of history.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct DisplayPage {
    pub lines: Vec<DialogueLine>,
    pub cleared: bool,
}

/// One reading at a logical flow boundary, without executor or GC handles.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct SavedReading {
    pub data: ReadingData,
    pub display: DisplayPage,
    next_interaction: Option<u64>,
    #[serde(skip)]
    retained: u64,
}
impl SavedReading {
    pub fn retained_bytes(&self) -> u64 {
        if self.retained != 0 {
            return self.retained;
        }
        let mut bytes = size_of::<Self>() + self.data.history.retained_bytes();
        if let Some(active) = &self.data.active {
            match active {
                Interaction::Dialogue(d) => {
                    bytes += d.page_locale.capacity()
                        + d.lines.capacity() * size_of::<DialogueLine>()
                        + d.current_line_ids.capacity() * size_of::<crate::DialogueLineId>()
                        + d.pending_read_ids.capacity() * size_of::<String>();
                    bytes += d.lines.iter().map(line_bytes).sum::<usize>();
                    bytes += d
                        .pending_read_ids
                        .iter()
                        .map(String::capacity)
                        .sum::<usize>();
                }
                Interaction::Choice(c) => {
                    bytes += c.options.capacity() * size_of::<ChoiceOption>();
                    bytes += c
                        .options
                        .iter()
                        .map(|o| o.id.key.capacity() + o.label.capacity())
                        .sum::<usize>();
                }
            }
        }
        if let Some(crate::Completion {
            outcome: crate::CompletionOutcome::Selected(key),
            ..
        }) = &self.data.completion
        {
            bytes += key.capacity();
        }
        bytes += self.display.lines.capacity() * size_of::<DialogueLine>();
        bytes += self.display.lines.iter().map(line_bytes).sum::<usize>();
        bytes as u64
    }
    pub fn install_begin(self, identity: SessionIdentity) -> ReadingInstall {
        ReadingInstall {
            saved: self,
            identity,
            phase: 0,
            row: 0,
            group: 0,
            boundary: 0,
            complete: false,
        }
    }

    pub(crate) fn attach_history(&mut self, history: super::HistoryLog) {
        self.data.history = history;
        self.retained = 0;
    }
}
fn line_bytes(line: &DialogueLine) -> usize {
    line.content.fallback.storage_bytes()
        + line.speaker_id.as_ref().map_or(0, String::capacity)
        + line.speaker_name.capacity()
        + line.content.id.capacity()
        + size_of_val(line.grapheme_boundaries.as_ref())
}

/// Each unit copies one metadata chunk, row, or identity. Immutable text is shared.
pub(crate) struct ReadingCapture {
    identity: SessionIdentity,
    reading: super::ReadingId,
    saved: SavedReading,
    phase: u8,
    row: usize,
    group: usize,
    field: usize,
    offset: usize,
    complete: bool,
    bytes: u64,
    include_history: bool,
}
impl ReadingCapture {
    #[cfg(test)]
    pub fn new(session: &Session, display: Option<&DisplayPage>) -> Self {
        Self::with_history(session, display, true)
    }

    pub fn with_history(
        session: &Session,
        display: Option<&DisplayPage>,
        include_history: bool,
    ) -> Self {
        let active = session.reading_data.active.as_ref().map(|a| match a {
            Interaction::Dialogue(d) => Interaction::Dialogue(DialogueInteraction {
                previously_read: d.previously_read,
                reveal_skippable: d.reveal_skippable,
                id: d.id,
                mode: d.mode,
                current_line_ids: Vec::new(),
                lines: Vec::new(),
                page_locale: String::new(),
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
        let completion = session
            .reading_data
            .completion
            .as_ref()
            .map(|c| crate::Completion {
                outcome: match &c.outcome {
                    crate::CompletionOutcome::Selected(_) => {
                        crate::CompletionOutcome::Selected(String::new())
                    }
                    other => other.clone(),
                },
                interaction_id: c.interaction_id,
                owner: c.owner,
                delivery: c.delivery,
            });
        Self {
            identity: session.identity,
            reading: session.reading_id,
            saved: SavedReading {
                data: ReadingData {
                    active,
                    completion,
                    history: Default::default(),
                    control: session.reading_data.control,
                },
                display: DisplayPage {
                    lines: Vec::new(),
                    cleared: display.is_some_and(|d| d.cleared),
                },
                next_interaction: session.next_interaction,
                retained: 0,
            },
            phase: 0,
            row: 0,
            group: 0,
            field: 0,
            offset: 0,
            complete: false,
            bytes: size_of::<SavedReading>() as u64,
            include_history,
        }
    }
    pub fn step(
        &mut self,
        session: &Session,
        display: Option<&DisplayPage>,
        work: u32,
        chunk: usize,
    ) -> Result<bool, SessionError> {
        if session.identity != self.identity || session.reading_id != self.reading {
            return Err(SessionError::InvalidReadingContext);
        }
        if chunk == 0 {
            return Err(SessionError::InvalidTextComposition);
        }
        for _ in 0..work {
            if self.complete {
                return Ok(true);
            }
            self.unit(&session.reading_data, display, chunk)?;
        }
        Ok(self.complete)
    }
    pub fn retained_bytes(&self) -> u64 {
        self.bytes
    }
    pub fn finish(mut self) -> Result<SavedReading, SessionError> {
        if !self.complete {
            return Err(SessionError::InteractionActive);
        }
        self.saved.retained = self.bytes;
        Ok(self.saved)
    }
    fn next(&mut self) {
        self.phase += 1;
        self.row = 0;
        self.group = 0;
        self.field = 0;
        self.offset = 0;
    }
    fn unit(
        &mut self,
        source: &ReadingData,
        display: Option<&DisplayPage>,
        chunk: usize,
    ) -> Result<(), SessionError> {
        match self.phase {
            0 => match (&source.completion, &mut self.saved.data.completion) {
                (
                    Some(crate::Completion {
                        outcome: crate::CompletionOutcome::Selected(source),
                        ..
                    }),
                    Some(crate::Completion {
                        outcome: crate::CompletionOutcome::Selected(target),
                        ..
                    }),
                ) => {
                    if copy(source, target, &mut self.offset, chunk, &mut self.bytes)? {
                        self.next();
                    }
                }
                _ => self.next(),
            },
            1 => match (&source.active, &mut self.saved.data.active) {
                (Some(Interaction::Dialogue(source)), Some(Interaction::Dialogue(target))) => {
                    if copy(
                        &source.page_locale,
                        &mut target.page_locale,
                        &mut self.offset,
                        chunk,
                        &mut self.bytes,
                    )? {
                        self.next();
                    }
                }
                _ => self.next(),
            },
            2 => match (&source.active, &mut self.saved.data.active) {
                (Some(Interaction::Dialogue(source)), Some(Interaction::Dialogue(target))) => {
                    if let Some(line) = source.lines.get(self.row) {
                        if target.lines.len() == self.row {
                            let shell = line_shell(line);
                            self.bytes += line_bytes(&shell) as u64;
                            push(&mut target.lines, shell, &mut self.bytes)?;
                        }
                        if copy_line(
                            line,
                            &mut target.lines[self.row],
                            &mut self.field,
                            &mut self.offset,
                            chunk,
                            &mut self.bytes,
                        )? {
                            self.row += 1;
                        }
                    } else {
                        self.next();
                    }
                }
                (Some(Interaction::Choice(source)), Some(Interaction::Choice(target))) => {
                    if let Some(option) = source.options.get(self.row) {
                        if target.options.len() == self.row {
                            push(
                                &mut target.options,
                                ChoiceOption {
                                    id: crate::ChoiceId {
                                        interaction_id: option.id.interaction_id,
                                        key: String::new(),
                                    },
                                    label: String::new(),
                                },
                                &mut self.bytes,
                            )?;
                        }
                        let result = if self.field == 0 {
                            copy(
                                &option.id.key,
                                &mut target.options[self.row].id.key,
                                &mut self.offset,
                                chunk,
                                &mut self.bytes,
                            )?
                        } else {
                            copy(
                                &option.label,
                                &mut target.options[self.row].label,
                                &mut self.offset,
                                chunk,
                                &mut self.bytes,
                            )?
                        };
                        if result {
                            self.offset = 0;
                            self.field += 1;
                            if self.field == 2 {
                                self.field = 0;
                                self.row += 1;
                            }
                        }
                    } else {
                        self.next();
                    }
                }
                _ => self.next(),
            },
            3 => match (&source.active, &mut self.saved.data.active) {
                (Some(Interaction::Dialogue(source)), Some(Interaction::Dialogue(target))) => {
                    if let Some(id) = source.current_line_ids.get(self.row) {
                        push(&mut target.current_line_ids, *id, &mut self.bytes)?;
                        self.row += 1;
                    } else {
                        self.next();
                    }
                }
                _ => self.next(),
            },
            4 => match (&source.active, &mut self.saved.data.active) {
                (Some(Interaction::Dialogue(source)), Some(Interaction::Dialogue(target))) => {
                    if let Some(id) = source.pending_read_ids.get(self.row) {
                        if target.pending_read_ids.len() == self.row {
                            push(&mut target.pending_read_ids, String::new(), &mut self.bytes)?;
                        }
                        if copy(
                            id,
                            &mut target.pending_read_ids[self.row],
                            &mut self.offset,
                            chunk,
                            &mut self.bytes,
                        )? {
                            self.offset = 0;
                            self.row += 1;
                        }
                    } else {
                        self.next();
                    }
                }
                _ => self.next(),
            },
            5 => {
                if self.include_history
                    && let Some(bytes) = self
                        .saved
                        .data
                        .history
                        .share_page(&source.history, self.group)
                {
                    self.bytes += bytes as u64;
                    self.group += 1;
                } else {
                    self.next();
                }
            }
            6 => {
                let active = match &source.active {
                    Some(Interaction::Dialogue(dialogue)) => Some(dialogue),
                    _ => None,
                };
                let visible = display
                    .map(|page| page.lines.as_slice())
                    .or_else(|| active.map(|dialogue| dialogue.lines.as_slice()));
                let line = visible.and_then(|lines| lines.get(self.row)).map(|line| {
                    active
                        .and_then(|dialogue| dialogue.lines.get(line.id.index))
                        .filter(|current| current.id == line.id)
                        .unwrap_or(line)
                });
                if let Some(line) = line {
                    if self.saved.display.lines.len() == self.row {
                        let mut shell = line_shell(line);
                        if source.completion.as_ref().is_some_and(|completion| {
                            completion.interaction_id == line.id.interaction_id
                                && matches!(completion.outcome, crate::CompletionOutcome::Advanced)
                        }) {
                            shell.complete_display();
                        }
                        self.bytes += line_bytes(&shell) as u64;
                        push(&mut self.saved.display.lines, shell, &mut self.bytes)?;
                    }
                    if copy_line(
                        line,
                        &mut self.saved.display.lines[self.row],
                        &mut self.field,
                        &mut self.offset,
                        chunk,
                        &mut self.bytes,
                    )? {
                        self.row += 1;
                    }
                } else {
                    self.complete = true;
                }
            }
            _ => unreachable!(),
        }
        Ok(())
    }
}
fn line_shell(line: &DialogueLine) -> DialogueLine {
    DialogueLine {
        id: line.id,
        previously_read: line.previously_read,
        speaker_id: line.speaker_id.as_ref().map(|_| String::new()),
        speaker_name: String::new(),
        content: TextContent {
            id: String::new(),
            fallback: line.content.fallback.clone(),
        },
        grapheme_boundaries: line.grapheme_boundaries.clone(),
        revealed_count: line.revealed_count,
        advance_requested: line.advance_requested,
        timing: line.timing.clone(),
    }
}
fn copy_line(
    source: &DialogueLine,
    target: &mut DialogueLine,
    field: &mut usize,
    offset: &mut usize,
    chunk: usize,
    bytes: &mut u64,
) -> Result<bool, SessionError> {
    let done = match *field {
        0 => copy(
            &source.content.id,
            &mut target.content.id,
            offset,
            chunk,
            bytes,
        )?,
        1 => copy(
            &source.speaker_name,
            &mut target.speaker_name,
            offset,
            chunk,
            bytes,
        )?,
        _ => match (&source.speaker_id, &mut target.speaker_id) {
            (Some(s), Some(t)) => copy(s, t, offset, chunk, bytes)?,
            _ => true,
        },
    };
    if done {
        *offset = 0;
        *field += 1;
    }
    if *field == 3 {
        *field = 0;
        Ok(true)
    } else {
        Ok(false)
    }
}
fn copy(
    source: &str,
    target: &mut String,
    offset: &mut usize,
    chunk: usize,
    bytes: &mut u64,
) -> Result<bool, SessionError> {
    let mut end = source.len().min(offset.saturating_add(chunk.max(4)));
    while !source.is_char_boundary(end) {
        end -= 1;
    }
    let before = target.capacity();
    target
        .try_reserve(end - *offset)
        .map_err(|_| SessionError::AllocationFailed)?;
    *bytes += (target.capacity() - before) as u64;
    target.push_str(&source[*offset..end]);
    *offset = end;
    Ok(end == source.len())
}
fn push<T>(target: &mut Vec<T>, value: T, bytes: &mut u64) -> Result<(), SessionError> {
    let before = target.capacity();
    target
        .try_reserve(1)
        .map_err(|_| SessionError::AllocationFailed)?;
    *bytes += ((target.capacity() - before) * size_of::<T>()) as u64;
    target.push(value);
    Ok(())
}

/// Rebase one row/boundary per unit before publishing it in the current Reading.
pub(crate) struct ReadingInstall {
    saved: SavedReading,
    identity: SessionIdentity,
    phase: u8,
    row: usize,
    group: usize,
    boundary: usize,
    complete: bool,
}
impl ReadingInstall {
    pub fn step(&mut self, work: u32) -> Result<bool, SessionError> {
        for _ in 0..work {
            if self.complete {
                return Ok(true);
            }
            match self.phase {
                0 => {
                    if let Some(c) = &mut self.saved.data.completion {
                        c.interaction_id.identity = self.identity;
                    }
                    match &mut self.saved.data.active {
                        Some(Interaction::Dialogue(d)) => d.id.identity = self.identity,
                        Some(Interaction::Choice(c)) => c.id.identity = self.identity,
                        None => (),
                    }
                    self.phase = 1;
                }
                1 => match &mut self.saved.data.active {
                    Some(Interaction::Dialogue(d)) => {
                        if let Some(line) = d.lines.get_mut(self.row) {
                            if rebase_line(line, self.identity, &mut self.boundary)? {
                                self.row += 1;
                            }
                        } else {
                            self.phase = 2;
                            self.row = 0;
                        }
                    }
                    Some(Interaction::Choice(c)) => {
                        if let Some(option) = c.options.get_mut(self.row) {
                            option.id.interaction_id.identity = self.identity;
                            self.row += 1;
                        } else {
                            self.phase = 2;
                            self.row = 0;
                        }
                    }
                    None => {
                        self.phase = 2;
                        self.row = 0;
                    }
                },
                2 => {
                    if let Some(Interaction::Dialogue(d)) = &mut self.saved.data.active
                        && let Some(id) = d.current_line_ids.get_mut(self.row)
                    {
                        id.interaction_id.identity = self.identity;
                        self.row += 1;
                        continue;
                    }
                    self.phase = 3;
                    self.row = 0;
                }
                3 => {
                    if let Some(group) = self.saved.data.history.get_mut(self.group) {
                        group.group_id.0.identity = self.identity;
                        group.interaction_id.identity = self.identity;
                        if let Some(line) = group.lines.get_mut(self.row) {
                            line.id.interaction_id.identity = self.identity;
                            self.row += 1;
                        } else {
                            self.group += 1;
                            self.row = 0;
                        }
                    } else {
                        self.phase = 4;
                        self.row = 0;
                    }
                }
                4 => {
                    if let Some(line) = self.saved.display.lines.get_mut(self.row) {
                        if rebase_line(line, self.identity, &mut self.boundary)? {
                            self.row += 1;
                        }
                    } else {
                        self.complete = true;
                    }
                }
                _ => unreachable!(),
            }
        }
        Ok(self.complete)
    }
    pub fn finish(mut self, session: &mut Session) -> Result<DisplayPage, SessionError> {
        if !self.complete || session.identity != self.identity {
            return Err(SessionError::InvalidReadingContext);
        }
        if let Some(Interaction::Dialogue(d)) = &self.saved.data.active {
            session
                .read_progress
                .ids
                .try_reserve(d.lines.len().max(d.pending_read_ids.len()))
                .map_err(|_| SessionError::AllocationFailed)?;
        }
        if let Some(c) = &mut self.saved.data.completion {
            c.owner = session.root_owner();
        }
        self.saved.data.control = session.reading_data.control;
        session.reading_data = self.saved.data;
        Ok(self.saved.display)
    }
}
fn rebase_line(
    line: &mut DialogueLine,
    identity: SessionIdentity,
    cursor: &mut usize,
) -> Result<bool, SessionError> {
    if *cursor == 0 {
        if line.grapheme_boundaries.first() != Some(&0)
            || line.grapheme_boundaries.last() != Some(&line.content.fallback.len())
            || line.revealed_count >= line.grapheme_boundaries.len()
        {
            return Err(SessionError::InvalidTextComposition);
        }
        line.id.interaction_id.identity = identity;
    }
    if let Some(&boundary) = line.grapheme_boundaries.get(*cursor) {
        if !line.content.fallback.is_char_boundary(boundary)
            || (*cursor > 0 && line.grapheme_boundaries[*cursor - 1] >= boundary)
        {
            return Err(SessionError::InvalidTextComposition);
        }
        *cursor += 1;
        return Ok(false);
    }
    *cursor = 0;
    Ok(true)
}
impl Session {
    /// Reserve archived generations before current bootstrap can submit new rows.
    pub(crate) fn reserve_interaction_generations(
        &mut self,
        saved: &SavedReading,
    ) -> Result<(), SessionError> {
        self.ensure_running()?;
        self.next_interaction = match (self.next_interaction, saved.next_interaction) {
            (Some(a), Some(b)) => Some(a.max(b)),
            _ => None,
        };
        Ok(())
    }
}
