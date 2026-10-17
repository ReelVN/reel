use super::*;

#[repr(u32)]
enum HistoryLineField {
    SpeakerName = 2,
    Text = 4,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct History {
    pub value: Value,
    groups: usize,
}

pub(super) fn line_id(heap: &mut Heap, id: crate::DialogueLineId) -> Result<Value, AdapterError> {
    Ok(heap.alloc_string(&format!(
        "{}:{}",
        id.interaction_id().generation(),
        id.index()
    ))?)
}

impl History {
    pub fn new(heap: &mut Heap) -> Result<Self, AdapterError> {
        Ok(Self {
            value: array(heap)?,
            groups: 0,
        })
    }

    pub fn sync(
        &mut self,
        heap: &mut Heap,
        session: &Session,
        current: Option<InteractionId>,
        dialogue: Value,
        chunk: usize,
    ) -> Result<(), AdapterError> {
        for group in &session.view().history[self.groups..] {
            let lines = array(heap)?;
            let active = if current == Some(group.interaction_id()) {
                Some(heap.host_payload(dialogue, 0)?)
            } else {
                None
            };
            for line in group.lines() {
                let (id, name, text, speaker, content) = if let Some(active) = active {
                    let row = heap.sequence_get(active, line.id().index() as u32)?;
                    let get = |field: LineField| {
                        heap.host_property(PropertyId {
                            host: row,
                            field: field as u32,
                        })
                    };
                    let values = (
                        get(LineField::Id)?,
                        get(LineField::SpeakerName)?,
                        get(LineField::Text)?,
                        get(LineField::SpeakerId)?,
                        get(LineField::ContentId)?,
                    );
                    let Some(InteractionView::Dialogue(display)) = session.view().interaction
                    else {
                        unreachable!("active history group belongs to the displayed dialogue")
                    };
                    let text = if display.lines()[line.id().index()].content() == line.content() {
                        values.2
                    } else {
                        super::super::rich_text::alloc(heap, line.content().styled(), chunk)?
                    };
                    (values.0, values.1, text, values.3, values.4)
                } else {
                    (
                        line_id(heap, line.id())?,
                        text::alloc_string(heap, line.speaker_name(), chunk)?,
                        super::super::rich_text::alloc(heap, line.content().styled(), chunk)?,
                        line.speaker_id().map_or(Ok(Value::NULL), |name| {
                            text::alloc_string(heap, name, chunk)
                        })?,
                        text::alloc_string(heap, line.content().id(), chunk)?,
                    )
                };
                let row = record(
                    heap,
                    &[
                        ("id", id),
                        ("speaker_id", speaker),
                        ("speaker_name", name),
                        ("content_id", content),
                        ("text", text),
                    ],
                    &[],
                )?;
                heap.array_push(lines, row)?;
            }
            let (name, text) = if heap.sequence_len(lines)? == 0 {
                let empty = heap.alloc_string("")?;
                (empty, empty)
            } else {
                let first = heap.sequence_get(lines, 0)?;
                (
                    heap.host_property(PropertyId {
                        host: first,
                        field: HistoryLineField::SpeakerName as u32,
                    })?,
                    heap.host_property(PropertyId {
                        host: first,
                        field: HistoryLineField::Text as u32,
                    })?,
                )
            };
            let locale = text::alloc_string(heap, group.page_locale(), chunk)?;
            let id = Value::int(group.interaction_id().generation() as i64);
            let row = record(
                heap,
                &[
                    ("group_id", id),
                    ("interaction_id", id),
                    ("locale", locale),
                    ("lines", lines),
                    ("speaker_name", name),
                    ("text", text),
                ],
                &[],
            )?;
            heap.array_push(self.value, row)?;
            self.groups += 1;
        }
        Ok(())
    }
}
