use crate::Value;
use std::collections::BTreeMap;

pub const VALUE_CHUNK_LEN: usize = 64;
pub const BYTE_CHUNK_LEN: usize = 1024;
// A deliberately conservative per-entry reserve for BTreeMap nodes (key + Box).
// Includes internal nodes; actual allocator totals are measured by the fixed load.
pub const CHUNK_METADATA_BYTES: u64 = 1024;
pub const VALUE_CHUNK_BYTES: u64 = VALUE_CHUNK_LEN as u64 * 16 + CHUNK_METADATA_BYTES;
pub const BYTE_CHUNK_BYTES: u64 = BYTE_CHUNK_LEN as u64 + CHUNK_METADATA_BYTES;

#[derive(Default)]
pub(crate) struct Values {
    chunks: BTreeMap<u32, Box<[Value; VALUE_CHUNK_LEN]>>,
    pub len: u32,
    pub release_cursor: u32,
}

impl Values {
    pub fn get(&self, index: u32) -> Option<Value> {
        if index >= self.len {
            return None;
        }
        self.chunks
            .get(&(index / VALUE_CHUNK_LEN as u32))
            .map(|c| c[index as usize % VALUE_CHUNK_LEN])
    }
    pub fn set(&mut self, index: u32, value: Value) {
        self.chunks
            .get_mut(&(index / VALUE_CHUNK_LEN as u32))
            .unwrap()[index as usize % VALUE_CHUNK_LEN] = value;
    }
    pub fn needs_chunk(&self) -> bool {
        (self.len as usize).is_multiple_of(VALUE_CHUNK_LEN)
    }
    pub fn push(&mut self, value: Value) {
        if self.needs_chunk() {
            self.chunks.insert(
                self.len / VALUE_CHUNK_LEN as u32,
                Box::new([Value::NULL; VALUE_CHUNK_LEN]),
            );
        }
        self.len += 1;
        self.set(self.len - 1, value);
    }
    pub fn pop(&mut self) -> Option<(Value, u64)> {
        let value = self.get(self.len.checked_sub(1)?)?;
        self.set(self.len - 1, Value::NULL);
        self.len -= 1;
        let freed = if self.needs_chunk() {
            self.chunks.remove(&(self.len / VALUE_CHUNK_LEN as u32));
            if self.chunks.is_empty() {
                self.chunks = BTreeMap::new();
            }
            VALUE_CHUNK_BYTES
        } else {
            0
        };
        Some((value, freed))
    }
    pub fn first_chunk_len(&self) -> u32 {
        let Some((&index, _)) = self.chunks.first_key_value() else {
            return 0;
        };
        (self.len - index * VALUE_CHUNK_LEN as u32).min(VALUE_CHUNK_LEN as u32)
    }
    pub fn clear_release_edge(&mut self) {
        self.chunks.first_entry().unwrap().get_mut()[self.release_cursor as usize] = Value::NULL;
        self.release_cursor += 1;
    }
    pub fn drop_first_chunk(&mut self) {
        self.chunks.pop_first();
        if self.chunks.is_empty() {
            self.chunks = BTreeMap::new();
        }
        self.release_cursor = 0;
    }
    pub fn empty(&self) -> bool {
        self.chunks.is_empty()
    }
}

#[derive(Default)]
pub(crate) struct Bytes {
    chunks: BTreeMap<u32, Box<[u8; BYTE_CHUNK_LEN]>>,
    pub len: u32,
}

impl Bytes {
    pub fn extra_chunks(&self, additional: usize) -> u64 {
        let before = (self.len as u64).div_ceil(BYTE_CHUNK_LEN as u64);
        let after = (self.len as u64 + additional as u64).div_ceil(BYTE_CHUNK_LEN as u64);
        after - before
    }
    pub fn append(&mut self, input: &[u8]) {
        let mut rest = input;
        while !rest.is_empty() {
            let within = self.len as usize % BYTE_CHUNK_LEN;
            if within == 0 {
                self.chunks.insert(
                    self.len / BYTE_CHUNK_LEN as u32,
                    Box::new([0; BYTE_CHUNK_LEN]),
                );
            }
            let count = rest.len().min(BYTE_CHUNK_LEN - within);
            self.chunks
                .get_mut(&(self.len / BYTE_CHUNK_LEN as u32))
                .unwrap()[within..within + count]
                .copy_from_slice(&rest[..count]);
            self.len += count as u32;
            rest = &rest[count..];
        }
    }
    pub fn read(&self, offset: u32, output: &mut [u8]) -> usize {
        let count = output.len().min(self.len.saturating_sub(offset) as usize);
        let mut n = 0;
        while n < count {
            let pos = offset as usize + n;
            let within = pos % BYTE_CHUNK_LEN;
            let part = (count - n).min(BYTE_CHUNK_LEN - within);
            output[n..n + part].copy_from_slice(
                &self.chunks[&((pos / BYTE_CHUNK_LEN) as u32)][within..within + part],
            );
            n += part;
        }
        count
    }
    pub fn drop_first_chunk(&mut self) {
        self.chunks.pop_first();
        if self.chunks.is_empty() {
            self.chunks = BTreeMap::new();
        }
    }
    pub fn empty(&self) -> bool {
        self.chunks.is_empty()
    }
}

pub(crate) enum Payload {
    Cell(crate::cell::CellState),
    Values(Values),
    Bytes(Bytes),
    Empty,
}
impl Payload {
    pub fn edge(&self, cursor: u32) -> Option<Value> {
        match self {
            Self::Cell(v) if cursor == 0 => v.value,
            Self::Values(v) => v.get(cursor),
            _ => None,
        }
    }
    pub fn len(&self) -> u32 {
        match self {
            Self::Cell(v) => u32::from(v.value.is_some()),
            Self::Values(v) => v.len,
            Self::Bytes(b) => b.len,
            Self::Empty => 0,
        }
    }
}
