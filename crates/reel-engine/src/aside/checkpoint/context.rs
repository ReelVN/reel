//! Explicit current-Root slots are indexed once at each frozen save/load boundary.
#[cfg(test)]
mod tests;
use super::CheckpointError;
use crate::aside::data::TypedDataError;
use aside_vm::{Heap, HeapReservation, ObjectKind, Value, ValueTag};
use std::collections::BTreeMap;

struct Slot {
    name: Value,
    value: Value,
}
pub(super) enum Source {
    Data,
    Root,
    Member(usize),
}
pub(super) struct ContextSlots {
    root: Value,
    entries: Vec<Slot>,
    identities: BTreeMap<u64, usize>,
    next: u32,
    count: u32,
    charge: HeapReservation,
}
impl ContextSlots {
    pub fn new(heap: &Heap, root: Value) -> Result<Self, CheckpointError> {
        let count = if root.tag() == ValueTag::Object {
            let info = heap.info(root)?;
            if info.kind == ObjectKind::Table {
                info.len
            } else {
                0
            }
        } else {
            0
        };
        Ok(Self {
            root,
            entries: Vec::new(),
            identities: BTreeMap::new(),
            next: 0,
            count,
            charge: heap.budget_usage().reserve(size_of::<Self>() as u64)?,
        })
    }
    pub fn retained_bytes(&self) -> u64 {
        self.charge.bytes()
    }
    /// Index one direct table entry. The active flow owns the table and its values.
    pub fn step(&mut self, heap: &Heap) -> Result<bool, CheckpointError> {
        if self.next == self.count {
            return Ok(true);
        }
        let (name, value) = heap.table_entry(self.root, self.next)?;
        self.next += 1;
        if name.tag() == ValueTag::Object
            && value.tag() == ValueTag::Object
            && heap.info(name)?.kind == ObjectKind::String
        {
            self.charge.resize(self.charge.bytes() + 256)?;
            let before = self.entries.capacity();
            self.entries
                .try_reserve(1)
                .map_err(|_| crate::aside::AdapterError::Allocation)?;
            self.charge.resize(
                self.charge.bytes()
                    + ((self.entries.capacity() - before) * size_of::<Slot>()) as u64,
            )?;
            let index = self.entries.len();
            self.entries.push(Slot { name, value });
            self.identities
                .entry(heap.handle(value)?.key)
                .or_insert(index);
        }
        Ok(self.next == self.count)
    }
    pub fn complete(&self) -> bool {
        self.next == self.count
    }
    pub fn source(&self, heap: &Heap, value: Value) -> Result<Source, CheckpointError> {
        if value.tag() != ValueTag::Object {
            return Ok(Source::Data);
        }
        if self.root.same_bits(value) {
            return Ok(Source::Root);
        }
        Ok(self
            .identities
            .get(&heap.handle(value)?.key)
            .map_or(Source::Data, |index| Source::Member(*index)))
    }
    pub fn copy_name(&self, heap: &Heap, index: usize) -> Result<MemberName, CheckpointError> {
        MemberName::new(heap, self.entries[index].name)
    }
    /// Compare one bounded byte segment against the explicitly named current slots.
    pub fn lookup_step(
        &self,
        heap: &Heap,
        name: &str,
        lookup: &mut Lookup,
        chunk: usize,
    ) -> Result<Option<Value>, CheckpointError> {
        if chunk == 0 {
            return Err(CheckpointError::Contract(
                "context member chunk must be nonzero",
            ));
        }
        let Some(slot) = self.entries.get(lookup.index) else {
            return Err(TypedDataError::Field {
                path: format!("run_flow.context[{name:?}]"),
                reason: "current Root member is missing or is not an object".into(),
            }
            .into());
        };
        let len = heap.info(slot.name)?.len as usize;
        if len != name.len() {
            lookup.index += 1;
            lookup.offset = 0;
            return Ok(None);
        }
        if lookup.offset == len {
            return Ok(Some(slot.value));
        }
        let bytes = heap.byte_segment(slot.name, lookup.offset as u32)?;
        let count = bytes.len().min(chunk).min(len - lookup.offset);
        if bytes[..count] != name.as_bytes()[lookup.offset..lookup.offset + count] {
            lookup.index += 1;
            lookup.offset = 0;
        } else {
            lookup.offset += count;
            if lookup.offset == len {
                return Ok(Some(slot.value));
            }
        }
        Ok(None)
    }
}
#[derive(Default)]
pub(super) struct Lookup {
    index: usize,
    offset: usize,
}

pub(super) struct MemberName {
    source: Value,
    offset: u32,
    len: u32,
    output: String,
    tail: [u8; 4],
    tail_len: usize,
    charge: HeapReservation,
}
impl MemberName {
    fn new(heap: &Heap, source: Value) -> Result<Self, CheckpointError> {
        let len = heap.info(source)?.len;
        let mut charge = heap
            .budget_usage()
            .reserve((size_of::<Self>() + len as usize) as u64)?;
        let mut output = String::new();
        output
            .try_reserve_exact(len as usize)
            .map_err(|_| crate::aside::AdapterError::Allocation)?;
        charge.resize((size_of::<Self>() + output.capacity()) as u64)?;
        Ok(Self {
            source,
            offset: 0,
            len,
            output,
            tail: [0; 4],
            tail_len: 0,
            charge,
        })
    }
    pub fn retained_bytes(&self) -> u64 {
        self.charge.bytes()
    }
    pub fn step(&mut self, heap: &Heap, chunk: usize) -> Result<bool, CheckpointError> {
        if chunk == 0 {
            return Err(CheckpointError::Contract(
                "context member chunk must be nonzero",
            ));
        }
        if self.offset == self.len {
            return Ok(true);
        }
        let bytes = heap.byte_segment(self.source, self.offset)?;
        let count = bytes
            .len()
            .min(chunk)
            .min((self.len - self.offset) as usize);
        for &byte in &bytes[..count] {
            self.tail[self.tail_len] = byte;
            self.tail_len += 1;
            if let Ok(text) = std::str::from_utf8(&self.tail[..self.tail_len]) {
                self.output.push_str(text);
                self.tail_len = 0;
            }
        }
        self.offset += count as u32;
        Ok(self.offset == self.len)
    }
    pub fn finish(self) -> String {
        self.output
    }
}
