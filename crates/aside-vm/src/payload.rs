use crate::Value;
use std::collections::BTreeMap;

pub const VALUE_CHUNK_LEN: usize = 64;
pub const BYTE_CHUNK_LEN: usize = 1024;
// A deliberately conservative per-entry reserve for BTreeMap nodes (key + Box).
// Includes internal nodes; actual allocator totals are measured by the fixed load.
pub const CHUNK_METADATA_BYTES: u64 = 1024;
pub const VALUE_CHUNK_BYTES: u64 = VALUE_CHUNK_LEN as u64 * 16 + CHUNK_METADATA_BYTES;
pub const BYTE_CHUNK_BYTES: u64 = BYTE_CHUNK_LEN as u64 + CHUNK_METADATA_BYTES;

// These fit inside the existing Fault variant, so small payloads do not enlarge
// every object slot. Promotion copies at most 128 bytes; large payloads retain
// independently allocated chunks for bounded growth and GC release.
pub(crate) const INLINE_VALUES: usize = 8;
pub(crate) const INLINE_BYTES: usize = 128;

enum ValueStorage {
    Inline([Value; INLINE_VALUES]),
    Chunks(BTreeMap<u32, Box<[Value; VALUE_CHUNK_LEN]>>),
}
pub(crate) struct Values {
    storage: ValueStorage,
    pub len: u32,
    pub release_cursor: u32,
}
impl Default for Values {
    fn default() -> Self {
        Self {
            storage: ValueStorage::Inline([Value::NULL; INLINE_VALUES]),
            len: 0,
            release_cursor: 0,
        }
    }
}

impl Values {
    pub fn chunks_for_len(len: u64) -> u64 {
        if len <= INLINE_VALUES as u64 {
            0
        } else {
            len.div_ceil(VALUE_CHUNK_LEN as u64)
        }
    }
    pub fn allocation_bytes(len: u64) -> u64 {
        Self::chunks_for_len(len) * VALUE_CHUNK_BYTES
    }
    pub fn growth_bytes(len: u64, additional: u64) -> u64 {
        Self::allocation_bytes(len + additional) - Self::allocation_bytes(len)
    }
    pub fn owned_chunks(&self) -> u64 {
        match &self.storage {
            ValueStorage::Inline(_) => 0,
            ValueStorage::Chunks(chunks) => chunks.len() as u64,
        }
    }
    pub fn get(&self, index: u32) -> Option<Value> {
        if index >= self.len {
            return None;
        }
        match &self.storage {
            ValueStorage::Inline(values) => Some(values[index as usize]),
            ValueStorage::Chunks(chunks) => chunks
                .get(&(index / VALUE_CHUNK_LEN as u32))
                .map(|c| c[index as usize % VALUE_CHUNK_LEN]),
        }
    }
    /// The remaining values in one existing storage block. Internal walkers
    /// retain the owning payload and do not use this slice across mutation.
    pub fn segment(&self, offset: u32) -> &[Value] {
        if offset == self.len {
            return &[];
        }
        match &self.storage {
            ValueStorage::Inline(values) => &values[offset as usize..self.len as usize],
            ValueStorage::Chunks(chunks) => {
                let within = offset as usize % VALUE_CHUNK_LEN;
                let count = (self.len - offset).min((VALUE_CHUNK_LEN - within) as u32) as usize;
                &chunks[&(offset / VALUE_CHUNK_LEN as u32)][within..within + count]
            }
        }
    }
    pub fn set(&mut self, index: u32, value: Value) {
        match &mut self.storage {
            ValueStorage::Inline(values) => values[index as usize] = value,
            ValueStorage::Chunks(chunks) => {
                chunks.get_mut(&(index / VALUE_CHUNK_LEN as u32)).unwrap()
                    [index as usize % VALUE_CHUNK_LEN] = value
            }
        }
    }
    pub fn needs_chunk(&self) -> bool {
        Self::growth_bytes(self.len.into(), 1) != 0
    }
    pub fn push(&mut self, value: Value) {
        if let ValueStorage::Inline(values) = &mut self.storage {
            if (self.len as usize) < INLINE_VALUES {
                values[self.len as usize] = value;
                self.len += 1;
                return;
            }
            let mut first = Box::new([Value::NULL; VALUE_CHUNK_LEN]);
            first[..INLINE_VALUES].copy_from_slice(values);
            self.storage = ValueStorage::Chunks(BTreeMap::from([(0, first)]));
        }
        let ValueStorage::Chunks(chunks) = &mut self.storage else {
            unreachable!()
        };
        if (self.len as usize).is_multiple_of(VALUE_CHUNK_LEN) {
            chunks.insert(
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
        let freed = Self::growth_bytes(self.len.into(), 1);
        if freed != 0 {
            let ValueStorage::Chunks(chunks) = &mut self.storage else {
                unreachable!()
            };
            if self.len as usize == INLINE_VALUES {
                let mut inline = [Value::NULL; INLINE_VALUES];
                inline.copy_from_slice(&chunks[&0][..INLINE_VALUES]);
                self.storage = ValueStorage::Inline(inline);
            } else {
                chunks.remove(&(self.len / VALUE_CHUNK_LEN as u32));
            }
        }
        Some((value, freed))
    }
    pub fn first_chunk_len(&self) -> u32 {
        let ValueStorage::Chunks(chunks) = &self.storage else {
            return self.len;
        };
        let Some((&index, _)) = chunks.first_key_value() else {
            return 0;
        };
        (self.len - index * VALUE_CHUNK_LEN as u32).min(VALUE_CHUNK_LEN as u32)
    }
    pub fn clear_release_edge(&mut self) {
        match &mut self.storage {
            ValueStorage::Inline(values) => values[self.release_cursor as usize] = Value::NULL,
            ValueStorage::Chunks(chunks) => {
                chunks.first_entry().unwrap().get_mut()[self.release_cursor as usize] = Value::NULL
            }
        }
        self.release_cursor += 1;
    }
    pub fn drop_first_chunk(&mut self) {
        match &mut self.storage {
            ValueStorage::Inline(_) => self.len = 0,
            ValueStorage::Chunks(chunks) => {
                chunks.pop_first();
                if chunks.is_empty() {
                    *chunks = BTreeMap::new();
                }
            }
        }
        self.release_cursor = 0;
    }
    pub fn empty(&self) -> bool {
        match &self.storage {
            ValueStorage::Inline(_) => self.len == 0,
            ValueStorage::Chunks(chunks) => chunks.is_empty(),
        }
    }
    pub fn release_bytes(&self) -> u64 {
        match &self.storage {
            ValueStorage::Inline(_) => 0,
            ValueStorage::Chunks(_) => VALUE_CHUNK_BYTES,
        }
    }
}

enum ByteStorage {
    Inline([u8; INLINE_BYTES]),
    Chunks(BTreeMap<u32, Box<[u8; BYTE_CHUNK_LEN]>>),
    Shared(
        std::rc::Rc<crate::fault::CodeLease>,
        crate::bytecode::ConstId,
    ),
}
pub(crate) struct Bytes {
    storage: ByteStorage,
    pub len: u32,
}
impl Default for Bytes {
    fn default() -> Self {
        Self {
            storage: ByteStorage::Inline([0; INLINE_BYTES]),
            len: 0,
        }
    }
}

impl Bytes {
    pub fn chunks_for_len(len: u64) -> u64 {
        if len <= INLINE_BYTES as u64 {
            0
        } else {
            len.div_ceil(BYTE_CHUNK_LEN as u64)
        }
    }
    pub fn growth_bytes(len: u64, additional: u64) -> u64 {
        (Self::chunks_for_len(len + additional) - Self::chunks_for_len(len)) * BYTE_CHUNK_BYTES
    }
    pub fn constant(
        code: std::rc::Rc<crate::fault::CodeLease>,
        id: crate::bytecode::ConstId,
    ) -> Self {
        let len = code.constant_bytes(id).expect("bound byte constant").len() as u32;
        Self {
            storage: ByteStorage::Shared(code, id),
            len,
        }
    }
    pub fn owned_chunks(&self) -> u64 {
        match &self.storage {
            ByteStorage::Chunks(chunks) => chunks.len() as u64,
            _ => 0,
        }
    }
    pub fn constant_id(&self) -> Option<crate::bytecode::ConstId> {
        match &self.storage {
            ByteStorage::Shared(_, id) => Some(*id),
            _ => None,
        }
    }
    pub fn release_bytes(&self) -> u64 {
        match &self.storage {
            ByteStorage::Chunks(_) => BYTE_CHUNK_BYTES,
            _ => 0,
        }
    }
    pub fn extra_chunks(&self, additional: usize) -> u64 {
        Self::growth_bytes(self.len.into(), additional as u64) / BYTE_CHUNK_BYTES
    }
    pub fn append(&mut self, input: &[u8]) {
        if let ByteStorage::Inline(bytes) = &mut self.storage {
            if self.len as usize + input.len() <= INLINE_BYTES {
                bytes[self.len as usize..self.len as usize + input.len()].copy_from_slice(input);
                self.len += input.len() as u32;
                return;
            }
            let mut first = Box::new([0; BYTE_CHUNK_LEN]);
            first[..self.len as usize].copy_from_slice(&bytes[..self.len as usize]);
            self.storage = ByteStorage::Chunks(BTreeMap::from([(0, first)]));
        }
        let ByteStorage::Chunks(chunks) = &mut self.storage else {
            unreachable!("immutable code bytes")
        };
        let mut rest = input;
        while !rest.is_empty() {
            let within = self.len as usize % BYTE_CHUNK_LEN;
            let count = rest.len().min(BYTE_CHUNK_LEN - within);
            chunks
                .entry(self.len / BYTE_CHUNK_LEN as u32)
                .or_insert_with(|| Box::new([0; BYTE_CHUNK_LEN]))[within..within + count]
                .copy_from_slice(&rest[..count]);
            self.len += count as u32;
            rest = &rest[count..];
        }
    }
    pub fn read(&self, offset: u32, output: &mut [u8]) -> usize {
        let count = output.len().min(self.len.saturating_sub(offset) as usize);
        if let ByteStorage::Shared(code, id) = &self.storage {
            output[..count].copy_from_slice(
                &code.constant_bytes(*id).expect("bound byte constant")
                    [offset as usize..offset as usize + count],
            );
            return count;
        }
        if let ByteStorage::Inline(bytes) = &self.storage {
            output[..count].copy_from_slice(&bytes[offset as usize..offset as usize + count]);
            return count;
        }
        let ByteStorage::Chunks(chunks) = &self.storage else {
            unreachable!()
        };
        let mut n = 0;
        while n < count {
            let pos = offset as usize + n;
            let within = pos % BYTE_CHUNK_LEN;
            let part = (count - n).min(BYTE_CHUNK_LEN - within);
            output[n..n + part]
                .copy_from_slice(&chunks[&((pos / BYTE_CHUNK_LEN) as u32)][within..within + part]);
            n += part;
        }
        count
    }
    pub fn segment(&self, offset: u32) -> &[u8] {
        if offset == self.len {
            return &[];
        }
        let within = offset as usize % BYTE_CHUNK_LEN;
        let count = (self.len - offset).min((BYTE_CHUNK_LEN - within) as u32) as usize;
        match &self.storage {
            ByteStorage::Shared(code, id) => &code
                .constant_bytes(*id)
                .expect("bound byte constant")[offset as usize..offset as usize + count],
            ByteStorage::Inline(bytes) => &bytes[offset as usize..offset as usize + count],
            ByteStorage::Chunks(chunks) => {
                &chunks[&(offset / BYTE_CHUNK_LEN as u32)][within..within + count]
            }
        }
    }
    /// Overwrite an admitted range of a private mutable buffer.
    pub(crate) fn overwrite(&mut self, offset: u32, input: &[u8]) {
        debug_assert!(offset as usize + input.len() <= self.len as usize);
        match &mut self.storage {
            ByteStorage::Inline(bytes) => {
                bytes[offset as usize..offset as usize + input.len()].copy_from_slice(input)
            }
            ByteStorage::Chunks(chunks) => {
                let mut copied = 0;
                while copied < input.len() {
                    let position = offset as usize + copied;
                    let within = position % BYTE_CHUNK_LEN;
                    let count = (input.len() - copied).min(BYTE_CHUNK_LEN - within);
                    chunks
                        .get_mut(&((position / BYTE_CHUNK_LEN) as u32))
                        .unwrap()[within..within + count]
                        .copy_from_slice(&input[copied..copied + count]);
                    copied += count;
                }
            }
            ByteStorage::Shared(_, _) => unreachable!("mutable buffer"),
        }
    }
    pub fn drop_first_chunk(&mut self) {
        if let ByteStorage::Chunks(chunks) = &mut self.storage {
            chunks.pop_first();
            if chunks.is_empty() {
                *chunks = BTreeMap::new();
            }
        } else {
            self.storage = ByteStorage::Inline([0; INLINE_BYTES]);
            self.len = 0;
        }
    }
    pub fn empty(&self) -> bool {
        match &self.storage {
            ByteStorage::Inline(_) => self.len == 0,
            ByteStorage::Chunks(chunks) => chunks.is_empty(),
            ByteStorage::Shared(_, _) => false,
        }
    }
}

pub(crate) enum Payload {
    Cell(crate::cell::CellState),
    Fault(crate::fault::FaultPayload),
    Values(Values),
    Table(Values),
    Dependencies(Box<crate::dependencies::Dependencies>),
    Bytes(Bytes),
    Empty,
}
impl Payload {
    pub fn edge(&self, cursor: u32) -> Option<Value> {
        match self {
            Self::Cell(v) if cursor == 0 => v.value,
            Self::Values(v) | Self::Table(v) => v.get(cursor),
            Self::Dependencies(d) => d.values.get(cursor),
            Self::Fault(p) => p.edges.get(cursor as usize).copied(),
            _ => None,
        }
    }
    pub fn len(&self) -> u32 {
        match self {
            Self::Cell(v) => u32::from(v.value.is_some()),
            Self::Values(v) | Self::Table(v) => v.len,
            Self::Dependencies(d) => d.values.len,
            Self::Fault(_) => 3,
            Self::Bytes(b) => b.len,
            Self::Empty => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_storage_cost_measurement() {
        assert!(size_of::<Values>() <= size_of::<crate::fault::FaultPayload>());
        assert!(size_of::<Bytes>() <= size_of::<crate::fault::FaultPayload>());
        println!(
            "layout Object={} Payload={} Fault={} Values={} Bytes={}",
            size_of::<crate::heap::Object>(),
            size_of::<Payload>(),
            size_of::<crate::fault::FaultPayload>(),
            size_of::<Values>(),
            size_of::<Bytes>()
        );
        for len in [1, 4, 8, 9, 64, 65] {
            let mut values = Values::default();
            let cost = allocation_counter::measure(|| {
                for index in 0..len {
                    values.push(Value::int(index));
                }
                std::hint::black_box(&values);
            });
            println!(
                "values len={len} allocations={} resident={} peak={}",
                cost.count_total, cost.bytes_current, cost.bytes_max
            );
            if len as usize <= INLINE_VALUES {
                assert_eq!(cost.count_total, 0);
            }
        }
        for len in [1, 32, 64, 128, 129, 1024, 1025] {
            let mut bytes = Bytes::default();
            let input = [b'x'; 1025];
            let cost = allocation_counter::measure(|| {
                bytes.append(&input[..len]);
                std::hint::black_box(&bytes);
            });
            println!(
                "bytes len={len} allocations={} resident={} peak={}",
                cost.count_total, cost.bytes_current, cost.bytes_max
            );
            if len <= INLINE_BYTES {
                assert_eq!(cost.count_total, 0);
            }
        }
    }

    #[test]
    fn inline_growth_and_shrink_preserve_contents_and_account_for_allocations() {
        let mut values = Values::default();
        let mut charged = 0;
        for index in 0..130 {
            charged += Values::growth_bytes(index, 1);
            values.push(Value::int(index as i64));
            assert_eq!(charged, values.owned_chunks() * VALUE_CHUNK_BYTES);
        }
        for index in (0..130).rev() {
            let (value, freed) = values.pop().unwrap();
            assert_eq!(value.as_int(), Some(index));
            charged -= freed;
            assert_eq!(charged, values.owned_chunks() * VALUE_CHUNK_BYTES);
        }
        assert_eq!(charged, 0);
        assert!(values.empty());
        let cost = allocation_counter::measure(|| {
            for _ in 0..3 {
                for index in 0..9 {
                    values.push(Value::int(index));
                }
                for index in (0..9).rev() {
                    assert_eq!(values.pop().unwrap().0.as_int(), Some(index));
                }
            }
        });
        assert_eq!(
            cost.bytes_current, 0,
            "demotion must free both chunk and tree"
        );

        let input: Vec<_> = (0..2200).map(|i| (i % 251) as u8).collect();
        let mut bytes = Bytes::default();
        let mut charged = 0;
        for part in input.chunks(17) {
            charged += Bytes::growth_bytes(bytes.len.into(), part.len() as u64);
            bytes.append(part);
            assert_eq!(charged, bytes.owned_chunks() * BYTE_CHUNK_BYTES);
            let mut read = vec![0; bytes.len as usize];
            assert_eq!(bytes.read(0, &mut read), read.len());
            assert_eq!(read, input[..bytes.len as usize]);
        }
        let mut offset = 0;
        while offset < bytes.len {
            let segment = bytes.segment(offset);
            assert!(segment.len() <= BYTE_CHUNK_LEN);
            assert_eq!(
                segment,
                &input[offset as usize..offset as usize + segment.len()]
            );
            offset += segment.len() as u32;
        }
    }
}
