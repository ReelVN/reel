use super::*;

#[repr(u32)]
enum HistoryLineField {
    SpeakerName = 2,
    Text = 4,
}

#[repr(u32)]
enum HistoryGroupField {
    Id = 0,
    Lines = 3,
    SpeakerName = 4,
    Text = 5,
}

/// Prepare a private tail before changing the observable array. Retained row
/// values are reused; one payload swap detaches the prefix and notifies once.
fn retain_tail(
    heap: &mut Heap,
    array: Value,
    remove: u32,
    roots: Value,
) -> Result<(), AdapterError> {
    if remove == 0 {
        return Ok(());
    }
    let len = heap.sequence_len(array)?;
    let prepared = heap.alloc_array(&[])?;
    heap.array_push(roots, prepared)?;
    let result = (|| {
        for index in remove..len {
            let value = heap.sequence_get(array, index)?;
            heap.array_push(prepared, value)?;
        }
        heap.commit_collection(array, prepared)?;
        heap.request_collection();
        Ok(())
    })();
    heap.array_pop(roots)?;
    result
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct History {
    pub value: Value,
    groups: usize,
    first_group: Option<u64>,
    #[serde(skip)]
    keys: super::super::observable::SchemaKeys,
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
            first_group: None,
            keys: Default::default(),
        })
    }

    pub fn sync(
        &mut self,
        heap: &mut Heap,
        session: &Session,
        current: Option<InteractionId>,
        dialogue: Value,
        roots: Value,
        chunk: usize,
    ) -> Result<(), AdapterError> {
        let first_group = session
            .view()
            .history
            .first()
            .map(|g| g.interaction_id().generation());
        // Retain existing projected groups by their stable generation. Hosts
        // can trim and append before synchronization, including equal lengths.
        if self.groups != 0 && self.first_group != first_group {
            let mut removed = 0;
            while removed < self.groups {
                let row = heap.sequence_get(self.value, removed as u32)?;
                let id = heap
                    .host_property(PropertyId {
                        host: row,
                        field: HistoryGroupField::Id as u32,
                    })?
                    .as_int()
                    .unwrap() as u64;
                if Some(id) == first_group {
                    break;
                }
                removed += 1;
            }
            retain_tail(heap, self.value, removed as u32, roots)?;
            self.groups -= removed;
        }
        self.first_group = first_group;
        if self.groups != 0 {
            let row = heap.sequence_get(self.value, 0)?;
            let lines = heap.host_property(PropertyId {
                host: row,
                field: HistoryGroupField::Lines as u32,
            })?;
            let keep = session.view().history[0].lines().len() as u32;
            let previous = heap.sequence_len(lines)?;
            if previous > keep {
                retain_tail(heap, lines, previous - keep, roots)?;
                let first = heap.sequence_get(lines, 0)?;
                for (group_field, line_field) in [
                    (
                        HistoryGroupField::SpeakerName,
                        HistoryLineField::SpeakerName,
                    ),
                    (HistoryGroupField::Text, HistoryLineField::Text),
                ] {
                    let value = heap.host_property(PropertyId {
                        host: first,
                        field: line_field as u32,
                    })?;
                    publish(heap, row, group_field as u32, value)?;
                }
            }
        }
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
                let row = self.keys.record(
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
            let row = self.keys.record(
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
