//! Rebuild current observable rows without republishing a dialogue operation.
use super::*;

/// A detached cursor; temporary VM values are owned by the current model root.
pub(in crate::aside) struct ReadingProjection {
    scratch: Option<Value>,
    phase: u8,
    row: usize,
    group: usize,
    builder: Option<RowBuilder>,
    locale: Option<StringBuilder>,
    complete: bool,
    ready: bool,
    last_interaction: Option<InteractionId>,
    last_group_first: u32,
}
impl Default for ReadingProjection {
    fn default() -> Self {
        Self {
            scratch: None,
            phase: 0,
            row: 0,
            group: 0,
            builder: None,
            locale: None,
            complete: false,
            ready: true,
            last_interaction: None,
            last_group_first: 0,
        }
    }
}
impl ReadingProjection {
    pub fn step(
        &mut self,
        model: &mut Narrative,
        heap: &mut Heap,
        session: &Session,
        work: u32,
        chunk: usize,
    ) -> Result<bool, AdapterError> {
        if chunk == 0 {
            return Err(AdapterError::Input(
                "reading projection chunk must be nonzero",
            ));
        }
        for _ in 0..work {
            if self.complete {
                return Ok(true);
            }
            self.unit(model, heap, session, chunk)?;
        }
        Ok(self.complete)
    }
    fn unit(
        &mut self,
        model: &mut Narrative,
        heap: &mut Heap,
        session: &Session,
        chunk: usize,
    ) -> Result<(), AdapterError> {
        if self.phase == 0 {
            let rows = heap.alloc_array(&[])?;
            let ids = array(heap)?;
            let scratch = heap.alloc_array(&[rows, ids])?;
            heap.array_push(model.roots, scratch)?;
            self.scratch = Some(scratch);
            for (host, field) in [
                (model.dialogue, DialogueField::Lines as u32),
                (model.dialogue, DialogueField::CurrentLineIds as u32),
                (model.choice, ChoiceField::Options as u32),
            ] {
                let previous = heap.host_property(PropertyId { host, field })?;
                let empty = heap.alloc_array(&[])?;
                heap.commit_collection(previous, empty)?;
            }
            if let Some(history) = &mut model.history {
                history.begin_projection(heap)?;
            }
            model.option_ids.clear();
            model.interaction = session.view().interaction.map(|i| i.id());
            model.display_cleared = model.page.cleared;
            self.phase = 1;
            return Ok(());
        }
        let scratch = self.scratch.unwrap();
        let rows = heap.sequence_get(scratch, 0)?;
        let ids = heap.sequence_get(scratch, 1)?;
        match self.phase {
            1 => {
                if let Some(line) = model.page.lines.get(self.row) {
                    if let Some(row) = project_row(
                        &mut self.builder,
                        heap,
                        scratch,
                        RowInput::Dialogue(line),
                        chunk,
                    )? {
                        if self.last_interaction != Some(line.id.interaction_id) {
                            self.last_interaction = Some(line.id.interaction_id);
                            self.last_group_first = self.row as u32;
                        }
                        let lines = heap.host_property(PropertyId {
                            host: model.dialogue,
                            field: DialogueField::Lines as u32,
                        })?;
                        heap.array_push(lines, row)?;
                        if matches!(session.view().interaction, Some(InteractionView::Dialogue(d)) if d.id() == line.id.interaction_id)
                        {
                            heap.array_push(rows, row)?;
                        }
                        self.row += 1;
                    }
                } else {
                    self.phase = 2;
                    self.row = 0;
                }
            }
            2 => {
                if let Some(InteractionView::Dialogue(d)) = session.view().interaction
                    && let Some(line) = d.lines().get(self.row)
                {
                    if self.row < heap.sequence_len(rows)? as usize {
                        self.ready &= line.playback_complete();
                        self.row += 1;
                    } else if let Some(row) = project_row(
                        &mut self.builder,
                        heap,
                        scratch,
                        RowInput::Dialogue(line),
                        chunk,
                    )? {
                        heap.array_push(rows, row)?;
                        self.ready &= line.playback_complete();
                        self.row += 1;
                    }
                } else {
                    self.phase = 3;
                    self.row = 0;
                }
            }
            3 => {
                if !model.page.cleared && self.row < heap.sequence_len(rows)? as usize {
                    let row = heap.sequence_get(rows, self.row as u32)?;
                    let id = heap.host_property(PropertyId {
                        host: row,
                        field: LineField::Id as u32,
                    })?;
                    heap.array_push(ids, id)?;
                    self.row += 1;
                } else {
                    heap.set_host_payload(model.dialogue, 0, rows)?;
                    let current = heap.host_property(PropertyId {
                        host: model.dialogue,
                        field: DialogueField::CurrentLineIds as u32,
                    })?;
                    heap.commit_collection(current, ids)?;
                    self.phase = 4;
                    self.row = 0;
                }
            }
            4 => {
                if let Some(InteractionView::Choice(c)) = session.view().interaction
                    && let Some(option) = c.options().get(self.row)
                {
                    if self.builder.is_none() {
                        self.builder = Some(RowBuilder::new(heap, scratch)?);
                    }
                    let builder = self.builder.as_mut().unwrap();
                    if builder.choice(heap, option, chunk)? {
                        let key = heap.sequence_get(builder.values, 0)?;
                        let label = heap.sequence_get(builder.values, 1)?;
                        let id = record(heap, &[], &[])?;
                        let row = record(heap, &[("id", id), ("key", key), ("label", label)], &[])?;
                        let options = heap.host_property(PropertyId {
                            host: model.choice,
                            field: ChoiceField::Options as u32,
                        })?;
                        heap.array_push(options, row)?;
                        model
                            .option_ids
                            .push((heap.handle(id)?, option.id().clone()));
                        heap.array_pop(scratch)?;
                        self.builder = None;
                        self.row += 1;
                    }
                } else {
                    self.phase = 5;
                    self.row = 0;
                }
            }
            5 => {
                if model.history.is_none() {
                    self.phase = 6;
                    return Ok(());
                }
                if let Some(group) = session.view().history.get(self.group) {
                    if heap.sequence_len(scratch)? == 2 {
                        let lines = array(heap)?;
                        heap.array_push(scratch, lines)?;
                    }
                    let lines = heap.sequence_get(scratch, 2)?;
                    if let Some(line) = group.lines().get(self.row) {
                        if let Some(row) = project_row(
                            &mut self.builder,
                            heap,
                            scratch,
                            RowInput::History(line),
                            chunk,
                        )? {
                            heap.array_push(lines, row)?;
                            self.row += 1;
                        }
                    } else {
                        if self.locale.is_none() {
                            self.locale = Some(StringBuilder::new(heap, scratch)?);
                        }
                        let locale = self.locale.as_mut().unwrap();
                        if locale.step(heap, group.page_locale(), chunk)? {
                            model.history.as_mut().unwrap().project_group(
                                heap,
                                group,
                                lines,
                                locale.value,
                            )?;
                            heap.array_pop(scratch)?;
                            self.locale = None;
                            heap.array_pop(scratch)?;
                            self.group += 1;
                            self.row = 0;
                        }
                    }
                } else {
                    self.phase = 6;
                }
            }
            6 => {
                let active = session.view().interaction;
                publish(
                    heap,
                    model.dialogue,
                    DialogueField::Active as u32,
                    Value::bool(matches!(active, Some(InteractionView::Dialogue(_)))),
                )?;
                publish(
                    heap,
                    model.choice,
                    ChoiceField::Active as u32,
                    Value::bool(matches!(active, Some(InteractionView::Choice(_)))),
                )?;
                let first = if model.page.cleared {
                    None
                } else {
                    let lines = heap.host_property(PropertyId {
                        host: model.dialogue,
                        field: DialogueField::Lines as u32,
                    })?;
                    let len = heap.sequence_len(lines)?;
                    if len == 0 {
                        None
                    } else if heap.sequence_len(rows)? > 0 {
                        Some(heap.sequence_get(rows, 0)?)
                    } else {
                        Some(heap.sequence_get(lines, self.last_group_first)?)
                    }
                };
                let empty = heap.alloc_string("")?;
                for (target, source) in [
                    (DialogueField::SpeakerName, LineField::SpeakerName),
                    (DialogueField::Text, LineField::Text),
                    (DialogueField::SpeakerId, LineField::SpeakerId),
                    (DialogueField::ContentId, LineField::ContentId),
                ] {
                    let value = if let Some(row) = first {
                        heap.host_property(PropertyId {
                            host: row,
                            field: source as u32,
                        })?
                    } else if matches!(target, DialogueField::SpeakerId | DialogueField::ContentId)
                    {
                        Value::NULL
                    } else {
                        empty
                    };
                    publish(heap, model.dialogue, target as u32, value)?;
                }
                let dialogue = match active {
                    Some(InteractionView::Dialogue(d)) => Some(d),
                    _ => None,
                };
                publish(
                    heap,
                    model.dialogue,
                    DialogueField::Reveal as u32,
                    Value::int(if model.page.cleared {
                        0
                    } else {
                        match dialogue {
                            Some(dialogue) => dialogue.reveal().visible_bytes.end as i64,
                            None => match first {
                                Some(row) => heap
                                    .host_property(PropertyId {
                                        host: row,
                                        field: LineField::Reveal as u32,
                                    })?
                                    .as_int()
                                    .unwrap(),
                                None => 0,
                            },
                        }
                    }),
                )?;
                publish(
                    heap,
                    model.dialogue,
                    DialogueField::Ready as u32,
                    Value::bool(!model.page.cleared && dialogue.is_some() && self.ready),
                )?;
                publish(
                    heap,
                    model.dialogue,
                    DialogueField::Read as u32,
                    Value::bool(
                        !model.page.cleared && dialogue.is_some_and(|d| d.previously_read()),
                    ),
                )?;
                publish(
                    heap,
                    model.dialogue,
                    DialogueField::AdvanceRequested as u32,
                    Value::bool(
                        !model.page.cleared && dialogue.is_some_and(|d| d.advance_requested()),
                    ),
                )?;
                model.text_revision = dialogue.map_or(0, |d| d.text_revision);
                model.sync_reading(heap, session)?;
                heap.array_pop(model.roots)?;
                self.scratch = None;
                self.complete = true;
            }
            _ => unreachable!(),
        }
        Ok(())
    }
}
enum RowInput<'a> {
    Dialogue(&'a crate::DialogueLine),
    History(&'a crate::HistoryLine),
}
impl RowInput<'_> {
    fn id(&self) -> crate::DialogueLineId {
        match self {
            Self::Dialogue(l) => l.id,
            Self::History(l) => l.id,
        }
    }
    fn content(&self) -> &crate::TextContent {
        match self {
            Self::Dialogue(l) => &l.content,
            Self::History(l) => &l.content,
        }
    }
    fn name(&self) -> &str {
        match self {
            Self::Dialogue(l) => &l.speaker_name,
            Self::History(l) => &l.speaker_name,
        }
    }
    fn speaker(&self) -> Option<&str> {
        match self {
            Self::Dialogue(l) => l.speaker_id.as_deref(),
            Self::History(l) => l.speaker_id.as_deref(),
        }
    }

    fn voice(&self) -> Option<&str> {
        match self {
            Self::Dialogue(_) => None,
            Self::History(line) => line.voice(),
        }
    }
}
struct StringBuilder {
    value: Value,
    offset: usize,
    sealed: bool,
}
impl StringBuilder {
    fn new(heap: &mut Heap, roots: Value) -> Result<Self, AdapterError> {
        let value = heap.alloc_byte_buffer(&[])?;
        heap.array_push(roots, value)?;
        Ok(Self {
            value,
            offset: 0,
            sealed: false,
        })
    }
    fn step(&mut self, heap: &mut Heap, source: &str, chunk: usize) -> Result<bool, AdapterError> {
        if self.sealed {
            return Ok(true);
        }
        let end = source.len().min(self.offset.saturating_add(chunk));
        heap.bytes_append(self.value, &source.as_bytes()[self.offset..end])?;
        self.offset = end;
        if end == source.len() {
            heap.seal_string(self.value)?;
            self.sealed = true;
        }
        Ok(self.sealed)
    }
}
struct RowBuilder {
    values: Value,
    field: u8,
    string: Option<StringBuilder>,
}
impl RowBuilder {
    fn new(heap: &mut Heap, roots: Value) -> Result<Self, AdapterError> {
        let values = heap.alloc_array(&[])?;
        heap.array_push(roots, values)?;
        Ok(Self {
            values,
            field: 0,
            string: None,
        })
    }
    fn field(&mut self, heap: &mut Heap, text: &str, chunk: usize) -> Result<bool, AdapterError> {
        if self.string.is_none() {
            self.string = Some(StringBuilder::new(heap, self.values)?);
        }
        if self.string.as_mut().unwrap().step(heap, text, chunk)? {
            self.string = None;
            self.field += 1;
            Ok(true)
        } else {
            Ok(false)
        }
    }
    fn choice(
        &mut self,
        heap: &mut Heap,
        option: &crate::ChoiceOption,
        chunk: usize,
    ) -> Result<bool, AdapterError> {
        let value = if self.field == 0 {
            option.key()
        } else {
            option.label()
        };
        self.field(heap, value, chunk)?;
        Ok(self.field == 2)
    }
    fn row(
        &mut self,
        heap: &mut Heap,
        input: &RowInput<'_>,
        chunk: usize,
    ) -> Result<Option<Value>, AdapterError> {
        match self.field {
            0 => {
                self.field(
                    heap,
                    &format!(
                        "{}:{}",
                        input.id().interaction_id().generation(),
                        input.id().index()
                    ),
                    chunk,
                )?;
            }
            1 => {
                self.field(heap, input.name(), chunk)?;
            }
            2 => {
                if let Some(speaker) = input.speaker() {
                    self.field(heap, speaker, chunk)?;
                } else {
                    heap.array_push(self.values, Value::NULL)?;
                    self.field += 1;
                }
            }
            3 => {
                self.field(heap, input.content().id(), chunk)?;
            }
            4 => {
                self.field(
                    heap,
                    input
                        .content()
                        .styled()
                        .markup()
                        .map_or(input.content().styled().as_ref(), |s| s.as_ref()),
                    chunk,
                )?;
                if self.field == 5 && input.content().styled().markup().is_none() {
                    self.field = 6;
                }
            }
            5 => {
                self.field(heap, input.content().styled(), chunk)?;
            }
            6 if let Some(voice) = input.voice() => {
                self.field(heap, voice, chunk)?;
            }
            _ => {
                let id = heap.sequence_get(self.values, 0)?;
                let name = heap.sequence_get(self.values, 1)?;
                let speaker = heap.sequence_get(self.values, 2)?;
                let content = heap.sequence_get(self.values, 3)?;
                let source = heap.sequence_get(self.values, 4)?;
                let text = if input.content().styled().markup().is_some() {
                    {
                        let plain = heap.sequence_get(self.values, 5)?;
                        super::super::rich_text::assemble(heap, source, plain)?
                    }
                } else {
                    source
                };
                let row = match input {
                    RowInput::Dialogue(line) => record(
                        heap,
                        &[
                            ("id", id),
                            ("speaker_name", name),
                            ("text", text),
                            ("reveal", Value::int(line.reveal().visible_bytes.end as i64)),
                            ("speaker_id", speaker),
                            ("content_id", content),
                            ("ready", Value::bool(line.playback_complete())),
                            ("read", Value::bool(line.previously_read())),
                            ("advance_requested", Value::bool(line.advance_requested())),
                        ],
                        &[],
                    )?,
                    RowInput::History(line) => {
                        let voice = if line.voice().is_some() {
                            heap.sequence_get(self.values, heap.sequence_len(self.values)? - 1)?
                        } else {
                            Value::NULL
                        };
                        record(
                            heap,
                            &[
                                ("id", id),
                                ("speaker_id", speaker),
                                ("speaker_name", name),
                                ("content_id", content),
                                ("text", text),
                                ("voice", voice),
                            ],
                            &[],
                        )?
                    }
                };
                return Ok(Some(row));
            }
        }
        Ok(None)
    }
}
fn project_row(
    builder: &mut Option<RowBuilder>,
    heap: &mut Heap,
    roots: Value,
    input: RowInput<'_>,
    chunk: usize,
) -> Result<Option<Value>, AdapterError> {
    if builder.is_none() {
        *builder = Some(RowBuilder::new(heap, roots)?);
    }
    let row = builder.as_mut().unwrap().row(heap, &input, chunk)?;
    if row.is_some() {
        heap.array_pop(roots)?;
        *builder = None;
    }
    Ok(row)
}
