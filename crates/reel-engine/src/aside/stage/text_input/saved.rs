//! Journal text is captured in bounded UTF-8 records, without cloning live histories.
use super::*;
use crate::aside::snapshot::{Part, Record, RecordStream};
use aside_vm::snapshot::SnapshotMap;
use reel_ui::{
    stage::NodeId,
    widgets::{TextEditKind, TextSelection},
};
use serde::{Deserialize, Serialize};

pub(super) struct RestoreState {
    count: usize,
    length: usize,
    current_length: usize,
    removed: usize,
    inserted: usize,
}

#[derive(Serialize, Deserialize)]
pub(in crate::aside) enum SavedHistory {
    Reset {
        node: NodeId,
        published: bool,
    },
    Begin {
        node: NodeId,
        expected: HeapHandle,
        revision: u64,
        position: usize,
        initial_len: usize,
        count: usize,
        merging: bool,
    },
    Delta {
        node: NodeId,
        start: usize,
        before: TextSelection,
        after: TextSelection,
        kind: TextEditKind,
        removed: usize,
        inserted: usize,
    },
    Text {
        node: NodeId,
        removed: bool,
        offset: usize,
        text: String,
    },
    End {
        node: NodeId,
    },
    Queue {
        node: NodeId,
        task: Option<aside_vm::executor::TaskId>,
        active_replay: Option<reel_ui::widgets::TextInputReplayId>,
        original_revision: u64,
        next_sequence: u64,
        limits: reel_ui::widgets::TextInputEditingLimits,
        count: usize,
    },
}

/// Immutable intent payloads are retained by Arc during bounded capture, then
/// encoded/decoded with the save on its storage worker, like retained pictures.
#[derive(Serialize, Deserialize)]
pub(in crate::aside) struct SavedTextInputIntent {
    node: NodeId,
    replay: Arc<reel_ui::widgets::TextInputReplay>,
}

fn current_source(heap: &Heap, binding: &NodeBinding) -> Option<HeapHandle> {
    let host = heap.import_handle(binding.handle).ok()?;
    heap.handle(heap.host_property(PropertyId { host, field: 0 }).ok()?)
        .ok()
}

pub(in crate::aside) fn snapshot_intents<'a>(
    stage: &'a StageBinding,
    heap: &'a Heap,
) -> impl Iterator<Item = SavedTextInputIntent> + 'a {
    stage.handles.iter().flat_map(move |(node, binding)| {
        binding
            .text_history
            .iter()
            .filter(move |h| {
                h.state == HistoryState::Active && Some(h.expected) == current_source(heap, binding)
            })
            .flat_map(|h| h.queue.iter())
            .flat_map(move |q| {
                q.entries.iter().map(move |entry| SavedTextInputIntent {
                    node: *node,
                    replay: entry.clone(),
                })
            })
    })
}

pub(in crate::aside) fn snapshot_records<'a>(
    stage: &'a StageBinding,
    heap: &'a Heap,
) -> RecordStream<'a> {
    Box::new(
        stage
            .handles
            .iter()
            .filter_map(move |(node, binding)| {
                if !matches!(
                    binding.shape,
                    snapshot::Shape::TextInput | snapshot::Shape::TextArea
                ) {
                    return None;
                }
                let source = current_source(heap, binding);
                let history = binding
                    .text_history
                    .as_deref()
                    .filter(|h| h.state == HistoryState::Active && Some(h.expected) == source);
                // A script write can invalidate the journal at an ordinary VM slice,
                // before stage projection runs. Capture the authoritative replacement.
                (history.is_some()
                    || binding.text_history.is_some()
                    || binding.text_source != source)
                    .then_some((
                        *node,
                        history,
                        binding.text_history.as_ref().is_some_and(|h| {
                            h.state == HistoryState::ResetPublished && Some(h.expected) == source
                        }),
                    ))
            })
            .flat_map(|(node, history, published)| {
                let header = std::iter::once_with(move || {
                    Record::new(
                        Part::TextInputHistory,
                        &history.map_or(SavedHistory::Reset { node, published }, |history| {
                            SavedHistory::Begin {
                                node,
                                expected: history.expected,
                                revision: history.revision,
                                position: history.position,
                                initial_len: history.initial_len,
                                count: history.entries.len(),
                                merging: history.merging,
                            }
                        }),
                    )
                });
                let entries =
                    history
                        .into_iter()
                        .flat_map(|h| h.entries.iter())
                        .flat_map(move |delta| {
                            let metadata = std::iter::once_with(move || {
                                Record::new(
                                    Part::TextInputHistory,
                                    &SavedHistory::Delta {
                                        node,
                                        start: delta.start,
                                        before: delta.before,
                                        after: delta.after,
                                        kind: delta.kind,
                                        removed: delta.removed.len(),
                                        inserted: delta.inserted.len(),
                                    },
                                )
                            });
                            let text = [true, false].into_iter().flat_map(move |removed| {
                                let mut offset = 0;
                                std::iter::from_fn(move || {
                                    let len = if removed {
                                        delta.removed.len()
                                    } else {
                                        delta.inserted.len()
                                    };
                                    if offset == len {
                                        return None;
                                    }
                                    let mut bytes = [0; 1024];
                                    let count = (len - offset).min(bytes.len());
                                    if removed {
                                        for (to, from) in bytes[..count]
                                            .iter_mut()
                                            .zip(delta.removed.range(offset..offset + count))
                                        {
                                            *to = *from;
                                        }
                                    } else {
                                        bytes[..count].copy_from_slice(
                                            &delta.inserted[offset..offset + count],
                                        );
                                    }
                                    let count = match std::str::from_utf8(&bytes[..count]) {
                                        Ok(_) => count,
                                        Err(error) => error.valid_up_to(),
                                    };
                                    let text = std::str::from_utf8(&bytes[..count])
                                        .expect("UTF-8 journal boundary")
                                        .to_owned();
                                    let record = Record::new(
                                        Part::TextInputHistory,
                                        &SavedHistory::Text {
                                            node,
                                            removed,
                                            offset,
                                            text,
                                        },
                                    );
                                    offset += count;
                                    Some(record)
                                })
                            });
                            metadata.chain(text)
                        });
                let queue = history
                    .into_iter()
                    .flat_map(|h| h.queue.iter())
                    .map(move |q| {
                        Record::new(
                            Part::TextInputHistory,
                            &SavedHistory::Queue {
                                node,
                                task: q.active,
                                active_replay: q.active_replay,
                                original_revision: q.original_revision,
                                next_sequence: q.next_sequence,
                                limits: q.limits,
                                count: q.entries.len(),
                            },
                        )
                    });
                header
                    .chain(entries)
                    .chain(history.into_iter().map(move |_| {
                        Record::new(Part::TextInputHistory, &SavedHistory::End { node })
                    }))
                    .chain(queue)
            }),
    )
}

fn invalid() -> AdapterError {
    AdapterError::Input("saved text input history")
}

impl StageBinding {
    pub(in crate::aside) fn restore_text_history(
        &mut self,
        heap: &Heap,
        map: &SnapshotMap,
        record: SavedHistory,
    ) -> Result<(), AdapterError> {
        let node = match &record {
            SavedHistory::Reset { node, .. }
            | SavedHistory::Begin { node, .. }
            | SavedHistory::Delta { node, .. }
            | SavedHistory::Text { node, .. }
            | SavedHistory::End { node }
            | SavedHistory::Queue { node, .. } => *node,
        };
        // A detached node omitted by VM reachability has no journal owner to restore.
        let Some(binding) = self.handles.get_mut(&node) else {
            return Ok(());
        };
        if !matches!(
            binding.shape,
            snapshot::Shape::TextInput | snapshot::Shape::TextArea
        ) {
            return Err(invalid());
        }
        match record {
            SavedHistory::Reset { published, .. } => {
                // Retain the interrupted script's observable fields until normal
                // maintenance. The marker contains only the current weak identity,
                // not the retired text, deltas or queue. Native edits also recognize it.
                let host = heap.import_handle(binding.handle)?;
                let current = heap.host_property(PropertyId { host, field: 0 })?;
                let mut marker = History::new(heap, heap.handle(current)?)?;
                marker.state = if published {
                    HistoryState::ResetPublished
                } else {
                    HistoryState::Invalidated
                };
                binding.text_history = Some(marker);
                binding.text_source = None;
                binding.projection_dirty = true;
            }
            SavedHistory::Begin {
                expected,
                revision,
                position,
                initial_len,
                count,
                merging,
                ..
            } => {
                if binding.text_history.is_some() || position > count {
                    return Err(invalid());
                }
                let expected = map.handle(expected)?;
                let mut history = History::new(heap, expected)?;
                history.reserve_restore(count, 0, 0)?;
                let current_length = if position == 0 {
                    initial_len
                } else {
                    usize::MAX
                };
                history.position = position;
                history.initial_len = initial_len;
                history.revision = revision;
                history.merging = merging;
                history.restoring = Some(RestoreState {
                    count,
                    length: initial_len,
                    current_length,
                    removed: 0,
                    inserted: 0,
                });
                binding.text_history = Some(history);
            }
            SavedHistory::Delta {
                start,
                before,
                after,
                kind,
                removed,
                inserted,
                ..
            } => {
                let history = binding.text_history.as_mut().ok_or_else(invalid)?;
                let state = history.restoring.as_ref().ok_or_else(invalid)?;
                if history.entries.last().is_some_and(|d| {
                    d.removed.len() != state.removed || d.inserted.len() != state.inserted
                }) || history.entries.len() >= state.count
                    || start > state.length
                    || removed > state.length - start
                    || before.anchor > state.length
                    || before.focus > state.length
                {
                    return Err(invalid());
                }
                let length = state
                    .length
                    .checked_sub(removed)
                    .and_then(|n| n.checked_add(inserted))
                    .ok_or_else(invalid)?;
                if after.anchor > length || after.focus > length {
                    return Err(invalid());
                }
                history.entries.push(history::Delta {
                    start,
                    before,
                    after,
                    kind,
                    removed: Default::default(),
                    inserted: Vec::new(),
                });
                history.reserve_restore(0, removed, inserted)?;
                let state = history.restoring.as_mut().unwrap();
                state.length = length;
                state.removed = removed;
                state.inserted = inserted;
                if history.entries.len() == history.position {
                    state.current_length = length;
                }
            }
            SavedHistory::Text {
                removed,
                offset,
                text,
                ..
            } => {
                if text.len() > 1024 || text.is_empty() {
                    return Err(invalid());
                }
                let history = binding.text_history.as_mut().ok_or_else(invalid)?;
                let state = history.restoring.as_ref().ok_or_else(invalid)?;
                let delta = history.entries.last_mut().ok_or_else(invalid)?;
                if removed {
                    if !delta.inserted.is_empty()
                        || offset != delta.removed.len()
                        || text.len() > state.removed - offset
                    {
                        return Err(invalid());
                    }
                    delta.removed.extend(text.bytes());
                } else {
                    if delta.removed.len() != state.removed
                        || offset != delta.inserted.len()
                        || text.len() > state.inserted - offset
                    {
                        return Err(invalid());
                    }
                    delta.inserted.extend_from_slice(text.as_bytes());
                }
            }
            SavedHistory::End { .. } => {
                let history = binding.text_history.as_mut().ok_or_else(invalid)?;
                let state = history.restoring.take().ok_or_else(invalid)?;
                if history.entries.len() != state.count
                    || history.entries.last().is_some_and(|d| {
                        d.removed.len() != state.removed || d.inserted.len() != state.inserted
                    })
                    || heap.info(heap.import_handle(history.expected)?)?.len as usize
                        != state.current_length
                {
                    return Err(invalid());
                }
            }
            SavedHistory::Queue {
                task,
                active_replay,
                original_revision,
                next_sequence,
                limits,
                count,
                ..
            } => {
                let history = binding.text_history.as_mut().ok_or_else(invalid)?;
                if history.queue.is_some()
                    || next_sequence == 0
                    || count > limits.max_events
                    || active_replay.is_some() && task.is_none()
                {
                    return Err(invalid());
                }
                let mut queue = super::queue::Queue::new(heap, limits, original_revision)?;
                queue.active = task.map(|task| map.task(task)).transpose()?;
                queue.active_replay = active_replay;
                queue.next_sequence = next_sequence;
                queue.restore_expected = count;
                history.queue = Some(queue);
                binding.projection_dirty = true;
                self.project_restored_text_queue(node)?;
            }
        }
        Ok(())
    }

    pub(in crate::aside) fn text_history_restore_complete(&self) -> bool {
        self.handles.values().all(|b| {
            b.text_history.as_ref().is_none_or(|h| {
                h.restoring.is_none()
                    && h.queue.as_ref().is_none_or(|q| {
                        q.entries.len() == q.restore_expected
                            && q.active_replay.is_none_or(|id| {
                                q.active.is_some()
                                    && q.entries.front().is_some_and(|entry| entry.id == id)
                            })
                    })
            })
        })
    }

    pub(in crate::aside) fn restore_text_intent(
        &mut self,
        saved: &SavedTextInputIntent,
    ) -> Result<(), AdapterError> {
        let Some(binding) = self.handles.get_mut(&saved.node) else {
            return Ok(());
        };
        let queue = binding
            .text_history
            .as_mut()
            .and_then(|h| h.queue.as_mut())
            .ok_or_else(invalid)?;
        if matches!(
            saved.replay.intent.as_ref(),
            reel_ui::widgets::TextInputIntent::Preedit { .. }
        ) {
            queue.restore_expected = queue.restore_expected.checked_sub(1).ok_or_else(invalid)?;
            return Ok(());
        }
        if saved.replay.id.generation != queue.original_revision
            || saved.replay.id.sequence == 0
            || saved.replay.id.sequence >= queue.next_sequence
            || queue
                .entries
                .back()
                .is_some_and(|e| e.id.sequence >= saved.replay.id.sequence)
        {
            return Err(invalid());
        }
        let next = queue.next_sequence;
        queue.next_sequence = saved.replay.id.sequence;
        queue.push(saved.replay.intent.clone())?;
        queue.next_sequence = next;
        binding.projection_dirty = true;
        self.project_restored_text_queue(saved.node)?;
        Ok(())
    }
}
