//! Input-owned delta transactions. Preparation reserves before model publication.
use super::*;
use aside_vm::{HeapHandle, HeapReservation};
use reel_ui::widgets::{TextEdit, TextEditKind, TextSelection};
use std::collections::VecDeque;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::aside::stage) enum HistoryState {
    Active,
    // A saved script slice retains its existing readonly fields until maintenance.
    Invalidated,
    // A native edit already published the reset; projection only refreshes its cache.
    ResetPublished,
}

pub(in crate::aside::stage) struct History {
    pub expected: HeapHandle,
    pub revision: u64,
    pub position: usize,
    pub(super) initial_len: usize,
    pub(super) entries: Vec<Delta>,
    pub(super) merging: bool,
    pub(in crate::aside::stage) state: HistoryState,
    pub(super) restoring: Option<super::saved::RestoreState>,
    pub(in crate::aside::stage) queue: Option<Box<super::queue::Queue>>,
    storage: usize,
    charge: HeapReservation,
}

pub(super) struct Delta {
    pub start: usize,
    pub before: TextSelection,
    pub after: TextSelection,
    pub kind: TextEditKind,
    // Front growth keeps repeated backspace linear without copying the old delta.
    pub removed: VecDeque<u8>,
    pub inserted: Vec<u8>,
}

pub(super) enum Prepared {
    Merge,
    Append(Delta),
}

fn reserve<T>(values: &mut Vec<T>, additional: usize) -> Result<(), AdapterError> {
    let capacity = capacity(
        values.len(),
        values.capacity(),
        additional,
        if size_of::<T>() == 1 { 32 } else { 4 },
    );
    values
        .try_reserve_exact(capacity.saturating_sub(values.len()))
        .map_err(|_| AdapterError::Allocation)
}

fn capacity(len: usize, old: usize, additional: usize, minimum: usize) -> usize {
    let needed = len + additional;
    if needed <= old {
        old
    } else {
        needed.max(old.saturating_mul(2)).max(minimum)
    }
}

fn reserve_removed(values: &mut VecDeque<u8>, additional: usize) -> Result<(), AdapterError> {
    let capacity = capacity(values.len(), values.capacity(), additional, 32);
    values
        .try_reserve_exact(capacity.saturating_sub(values.len()))
        .map_err(|_| AdapterError::Allocation)
}

/// Borrow a committed fragment directly from an existing string or VM proposal.
/// The journal copies its own delta once; filtering needs no temporary full text.
#[derive(Clone, Copy)]
pub(super) enum EditText<'a> {
    Slice(&'a str),
    Vm {
        heap: &'a Heap,
        value: Value,
        start: usize,
        length: usize,
    },
}
impl<'a> From<&'a str> for EditText<'a> {
    fn from(value: &'a str) -> Self {
        Self::Slice(value)
    }
}
impl<'a> EditText<'a> {
    pub(super) fn len(self) -> usize {
        match self {
            Self::Slice(s) => s.len(),
            Self::Vm { length, .. } => length,
        }
    }
    fn is_empty(self) -> bool {
        self.len() == 0
    }
    fn bytes(self) -> EditBytes<'a> {
        let end = self.len();
        EditBytes {
            text: self,
            start: 0,
            end,
            chunk: &[],
        }
    }
}
struct EditBytes<'a> {
    text: EditText<'a>,
    start: usize,
    end: usize,
    chunk: &'a [u8],
}
impl Iterator for EditBytes<'_> {
    type Item = u8;
    fn next(&mut self) -> Option<u8> {
        if self.start == self.end {
            return None;
        }
        if self.chunk.is_empty() {
            self.chunk = match self.text {
                EditText::Slice(text) => &text.as_bytes()[self.start..self.end],
                EditText::Vm {
                    heap, value, start, ..
                } => heap
                    .byte_segment(value, (start + self.start) as u32)
                    .expect("validated edit fragment"),
            };
        }
        let value = self.chunk[0];
        self.chunk = &self.chunk[1..];
        self.start += 1;
        Some(value)
    }
}
impl DoubleEndedIterator for EditBytes<'_> {
    fn next_back(&mut self) -> Option<u8> {
        if self.start == self.end {
            return None;
        }
        self.end -= 1;
        Some(match self.text {
            EditText::Slice(text) => text.as_bytes()[self.end],
            EditText::Vm {
                heap, value, start, ..
            } => heap
                .byte_segment(value, (start + self.end) as u32)
                .expect("validated edit fragment")[0],
        })
    }
}

impl History {
    pub fn new(heap: &Heap, expected: HeapHandle) -> Result<Box<Self>, AdapterError> {
        let charge = heap.budget_usage().reserve(size_of::<Self>() as u64)?;
        let initial_len = heap.info(heap.import_handle(expected)?)?.len as usize;
        Ok(Box::new(Self {
            expected,
            revision: 0,
            position: 0,
            initial_len,
            entries: Vec::new(),
            merging: false,
            state: HistoryState::Active,
            restoring: None,
            queue: None,
            storage: 0,
            charge,
        }))
    }

    pub(super) fn reset_published(&mut self, expected: HeapHandle, length: usize) {
        self.expected = expected;
        self.position = 0;
        self.initial_len = length;
        self.entries = Vec::new();
        self.merging = false;
        self.restoring = None;
        self.queue = None;
        self.storage = 0;
        self.state = HistoryState::ResetPublished;
        self.charge
            .resize(self.bytes())
            .expect("release invalidated input history");
    }

    pub fn split(&mut self) {
        self.merging = false;
    }
    pub(super) fn abort(&mut self, prepared: Prepared) {
        drop(prepared);
        self.charge
            .resize(self.bytes())
            .expect("release prepared history storage");
    }
    pub fn can_undo(&self) -> bool {
        self.position != 0
    }
    pub fn can_redo(&self) -> bool {
        self.position < self.entries.len()
    }

    fn bytes(&self) -> u64 {
        (size_of::<Self>() + self.entries.capacity() * size_of::<Delta>() + self.storage) as u64
    }

    pub(super) fn prepare<'a>(
        &mut self,
        edit: &TextEdit,
        removed: impl Into<EditText<'a>>,
        inserted: impl Into<EditText<'a>>,
    ) -> Result<Prepared, AdapterError> {
        let removed = removed.into();
        let inserted = inserted.into();
        let merge = self.merging
            && self.position == self.entries.len()
            && edit.before.anchor == edit.before.focus
            && self.entries.last().is_some_and(|last| {
                last.kind == edit.kind
                    && last.after == edit.before
                    && match edit.kind {
                        TextEditKind::Insert => {
                            removed.is_empty()
                                && edit.range.start == last.start + last.inserted.len()
                        }
                        TextEditKind::Backspace => {
                            inserted.is_empty() && edit.range.end == last.start
                        }
                        TextEditKind::Delete => {
                            inserted.is_empty() && edit.range.start == last.start
                        }
                        TextEditKind::Transaction => false,
                    }
            });
        // Reserve growth conservatively before touching any live transaction.
        let growth = if merge {
            let last = self.entries.last().unwrap();
            capacity(
                last.removed.len(),
                last.removed.capacity(),
                removed.len(),
                32,
            ) - last.removed.capacity()
                + capacity(
                    last.inserted.len(),
                    last.inserted.capacity(),
                    inserted.len(),
                    32,
                )
                - last.inserted.capacity()
        } else {
            capacity(0, 0, removed.len(), 32)
                + capacity(0, 0, inserted.len(), 32)
                + (capacity(
                    self.entries.len(),
                    self.entries.capacity(),
                    usize::from(self.position == self.entries.len()),
                    4,
                ) - self.entries.capacity())
                    * size_of::<Delta>()
        };
        self.charge.resize(self.bytes() + growth as u64)?;
        let old_capacity = if merge {
            let last = self.entries.last().unwrap();
            last.removed.capacity() + last.inserted.capacity()
        } else {
            0
        };
        let preparation = (|| {
            if merge {
                let last = self.entries.last_mut().unwrap();
                reserve_removed(&mut last.removed, removed.len())?;
                reserve(&mut last.inserted, inserted.len())?;
                Ok(Prepared::Merge)
            } else {
                let additional = usize::from(self.position == self.entries.len());
                reserve(&mut self.entries, additional)?;
                let mut removed_bytes = VecDeque::new();
                let mut inserted_bytes = Vec::new();
                reserve_removed(&mut removed_bytes, removed.len())?;
                reserve(&mut inserted_bytes, inserted.len())?;
                removed_bytes.extend(removed.bytes());
                inserted_bytes.extend(inserted.bytes());
                Ok(Prepared::Append(Delta {
                    start: edit.range.start,
                    before: edit.before,
                    after: edit.after,
                    kind: edit.kind,
                    removed: removed_bytes,
                    inserted: inserted_bytes,
                }))
            }
        })();
        if merge {
            let last = self.entries.last().unwrap();
            self.storage += last.removed.capacity() + last.inserted.capacity() - old_capacity;
        }
        let prepared_bytes = preparation
            .as_ref()
            .ok()
            .map_or(0, |prepared| match prepared {
                Prepared::Merge => 0,
                Prepared::Append(delta) => delta.removed.capacity() + delta.inserted.capacity(),
            });
        self.charge.resize(self.bytes() + prepared_bytes as u64)?;
        preparation
    }

    /// Capacity and byte buffers were prepared before the authoritative model write.
    pub(super) fn commit<'a>(
        &mut self,
        prepared: Prepared,
        edit: &TextEdit,
        removed: impl Into<EditText<'a>>,
        inserted: impl Into<EditText<'a>>,
        expected: HeapHandle,
    ) {
        let removed = removed.into();
        let inserted = inserted.into();
        match prepared {
            Prepared::Merge => {
                let last = self.entries.last_mut().unwrap();
                if edit.kind == TextEditKind::Backspace {
                    for byte in removed.bytes().rev() {
                        last.removed.push_front(byte);
                    }
                    last.start = edit.range.start;
                } else {
                    last.removed.extend(removed.bytes());
                }
                last.inserted.extend(inserted.bytes());
                last.after = edit.after;
            }
            Prepared::Append(delta) => {
                self.storage -= self.entries[self.position..]
                    .iter()
                    .map(|d| d.removed.capacity() + d.inserted.capacity())
                    .sum::<usize>();
                self.entries.truncate(self.position);
                self.storage += delta.removed.capacity() + delta.inserted.capacity();
                self.entries.push(delta);
                self.position += 1;
            }
        }
        self.merging =
            edit.kind != TextEditKind::Transaction && edit.before.anchor == edit.before.focus;
        self.revision = self.revision.wrapping_add(1);
        self.expected = expected;
        self.state = HistoryState::Active;
        // Shrinking after discarding redo cannot fail and releases its retained byte budget.
        self.charge
            .resize(self.bytes())
            .expect("prepared history capacity");
    }

    pub(super) fn delta(&self, redo: bool) -> Option<&Delta> {
        if redo {
            self.entries.get(self.position)
        } else {
            self.position
                .checked_sub(1)
                .and_then(|i| self.entries.get(i))
        }
    }

    pub fn moved(&mut self, redo: bool, expected: HeapHandle) {
        if redo {
            self.position += 1;
        } else {
            self.position -= 1;
        }
        self.expected = expected;
        self.state = HistoryState::Active;
        self.revision = self.revision.wrapping_add(1);
        self.merging = false;
    }

    pub(super) fn charge_restored(&mut self) -> Result<(), AdapterError> {
        self.charge.resize(self.bytes())?;
        Ok(())
    }

    pub(super) fn reserve_restore(
        &mut self,
        entries: usize,
        removed: usize,
        inserted: usize,
    ) -> Result<(), AdapterError> {
        self.charge
            .resize(self.bytes() + (entries * size_of::<Delta>() + removed + inserted) as u64)?;
        self.entries
            .try_reserve_exact(entries)
            .map_err(|_| AdapterError::Allocation)?;
        if let Some(delta) = self.entries.last_mut() {
            let old = delta.removed.capacity() + delta.inserted.capacity();
            let result = (|| {
                delta
                    .removed
                    .try_reserve_exact(removed)
                    .map_err(|_| AdapterError::Allocation)?;
                delta
                    .inserted
                    .try_reserve_exact(inserted)
                    .map_err(|_| AdapterError::Allocation)
            })();
            self.storage += delta.removed.capacity() + delta.inserted.capacity() - old;
            result?;
        }
        self.charge_restored()
    }
}

impl Delta {
    pub fn replacement_len(&self, redo: bool) -> usize {
        if redo {
            self.inserted.len()
        } else {
            self.removed.len()
        }
    }

    pub fn replaced_len(&self, redo: bool) -> usize {
        if redo {
            self.removed.len()
        } else {
            self.inserted.len()
        }
    }

    pub fn selection(&self, redo: bool) -> TextSelection {
        if redo { self.after } else { self.before }
    }

    pub fn read_replacement(&self, redo: bool, offset: usize, output: &mut [u8]) -> usize {
        let count = output.len().min(self.replacement_len(redo) - offset);
        if redo {
            output[..count].copy_from_slice(&self.inserted[offset..offset + count]);
        } else {
            for (target, source) in output[..count]
                .iter_mut()
                .zip(self.removed.range(offset..offset + count))
            {
                *target = *source;
            }
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directional_transactions_coalesce_without_retaining_whole_values() {
        let mut heap =
            Heap::new(aside_vm::SessionEpoch(1), aside_vm::HeapLimits::default()).unwrap();
        let value = heap.alloc_string("").unwrap();
        let expected = heap.handle(value).unwrap();
        let mut history = History::new(&heap, expected).unwrap();
        let mut position = 0;
        for text in ["A", "é", "中"] {
            let edit = TextEdit {
                range: position..position,
                before: TextSelection {
                    anchor: position,
                    focus: position,
                },
                after: TextSelection {
                    anchor: position + text.len(),
                    focus: position + text.len(),
                },
                kind: TextEditKind::Insert,
                revision: 0,
                replay: None,
            };
            let prepared = history.prepare(&edit, "", text).unwrap();
            history.commit(prepared, &edit, "", text, expected);
            position += text.len();
        }
        assert_eq!(history.entries.len(), 1);
        assert_eq!(history.entries[0].inserted, "Aé中".as_bytes());
        history.split();
        for text in ["中", "é", "A"] {
            let next = position - text.len();
            let edit = TextEdit {
                range: next..position,
                before: TextSelection {
                    anchor: position,
                    focus: position,
                },
                after: TextSelection {
                    anchor: next,
                    focus: next,
                },
                kind: TextEditKind::Backspace,
                revision: 0,
                replay: None,
            };
            let prepared = history.prepare(&edit, text, "").unwrap();
            history.commit(prepared, &edit, text, "", expected);
            position = next;
        }
        assert_eq!(history.entries.len(), 2);
        let mut bytes = [0; 8];
        let delta = history.delta(false).unwrap();
        let count = delta.read_replacement(false, 0, &mut bytes);
        assert_eq!(&bytes[..count], "Aé中".as_bytes());
        history.moved(false, expected);
        assert!(history.can_undo() && history.can_redo());
        assert_eq!(
            history.delta(true).unwrap().replaced_len(true),
            "Aé中".len()
        );
    }

    #[test]
    fn merged_typing_retains_only_delta_capacity_and_releases_its_charge() {
        let mut heap =
            Heap::new(aside_vm::SessionEpoch(1), aside_vm::HeapLimits::default()).unwrap();
        let value = heap.alloc_string("").unwrap();
        let expected = heap.handle(value).unwrap();
        println!(
            "text_input_types,props={},node={},kind={},options={}",
            size_of::<reel_ui::stage::NodeProps>(),
            size_of::<reel_ui::stage::StageNode>(),
            size_of::<reel_ui::stage::NodeKind>(),
            size_of::<reel_ui::stage::TextInputOptions>()
        );
        let baseline = heap.stats().charged_bytes;
        let mut history = History::new(&heap, expected).unwrap();
        assert_eq!(history.entries.capacity(), 0);
        for index in 0..1000 {
            let edit = TextEdit {
                range: index..index,
                before: TextSelection {
                    anchor: index,
                    focus: index,
                },
                after: TextSelection {
                    anchor: index + 1,
                    focus: index + 1,
                },
                kind: TextEditKind::Insert,
                revision: 0,
                replay: None,
            };
            let prepared = history.prepare(&edit, "", "A").unwrap();
            history.commit(prepared, &edit, "", "A", expected);
        }
        assert_eq!(history.entries.len(), 1);
        assert_eq!(history.entries[0].inserted.len(), 1000);
        let bytes = history.charge.bytes();
        assert!(
            bytes < 4096,
            "one thousand characters retain one delta: {bytes}"
        );
        assert_eq!(heap.stats().charged_bytes - baseline, bytes);
        println!(
            "text_history,typed=1000,groups=1,retained_bytes={bytes},delta_capacity={},history_bytes={}",
            history.storage,
            size_of::<History>()
        );
        drop(history);
        assert_eq!(heap.stats().charged_bytes, baseline);
    }

    #[test]
    fn rejected_growth_preserves_transactions_and_redo_state() {
        let limits = aside_vm::HeapLimits {
            max_bytes: 1_000_000,
            ..Default::default()
        };
        let mut heap = Heap::new(aside_vm::SessionEpoch(1), limits).unwrap();
        let value = heap.alloc_string("").unwrap();
        let expected = heap.handle(value).unwrap();
        let mut history = History::new(&heap, expected).unwrap();
        let edit = TextEdit {
            range: 0..0,
            before: Default::default(),
            after: TextSelection {
                anchor: 1,
                focus: 1,
            },
            kind: TextEditKind::Insert,
            revision: 0,
            replay: None,
        };
        let prepared = history.prepare(&edit, "", "A").unwrap();
        history.commit(prepared, &edit, "", "A", expected);
        history.moved(false, expected);
        let bytes = history.charge.bytes();
        let guard = heap
            .budget_usage()
            .reserve(limits.max_bytes - heap.stats().charged_bytes)
            .unwrap();
        let big = "B".repeat(4096);
        assert!(history.prepare(&edit, "", big.as_str()).is_err());
        assert_eq!(history.position, 0);
        assert_eq!(history.entries.len(), 1);
        assert!(!history.can_undo() && history.can_redo());
        assert_eq!(history.entries[0].inserted, b"A");
        assert_eq!(history.charge.bytes(), bytes);
        drop(guard);
    }
}
