use crate::Value;
use std::collections::BTreeMap;
pub(crate) mod table;
pub(crate) use table::Table;

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
const FIRST_VALUES: usize = 32;
const FIRST_VALUE_BYTES: u64 = (FIRST_VALUES * size_of::<Value>()) as u64;

enum ValueStorage {
    Inline([Value; INLINE_VALUES]),
    // Common records need one compact block. Larger collections retain that
    // block and append bounded chunks, without relocating their existing edges.
    Small(Box<[Value; FIRST_VALUES]>),
    Chunks {
        first: Option<Box<[Value; FIRST_VALUES]>>,
        rest: BTreeMap<u32, Box<[Value; VALUE_CHUNK_LEN]>>,
    },
}
pub(crate) struct Values {
    storage: ValueStorage,
    pub len: u32,
    // Verified and restored type IDs leave u32::MAX available. Using the
    // existing padding keeps ordinary heap objects at their original size.
    array_item: crate::bytecode::TypeId,
}
impl Default for Values {
    fn default() -> Self {
        Self {
            storage: ValueStorage::Inline([Value::NULL; INLINE_VALUES]),
            len: 0,
            array_item: crate::bytecode::TypeId(u32::MAX),
        }
    }
}

impl Values {
    pub fn array_item(&self) -> Option<crate::bytecode::TypeId> {
        (self.array_item.0 != u32::MAX).then_some(self.array_item)
    }
    pub fn set_array_item(&mut self, item: crate::bytecode::TypeId) {
        self.array_item = item;
    }

    pub fn chunks_for_len(len: u64) -> u64 {
        if len <= INLINE_VALUES as u64 {
            0
        } else {
            1 + len
                .saturating_sub(FIRST_VALUES as u64)
                .div_ceil(VALUE_CHUNK_LEN as u64)
        }
    }
    pub fn allocation_bytes(len: u64) -> u64 {
        let chunks = Self::chunks_for_len(len);
        if chunks == 0 {
            0
        } else {
            FIRST_VALUE_BYTES + (chunks - 1) * VALUE_CHUNK_BYTES
        }
    }
    pub fn growth_bytes(len: u64, additional: u64) -> u64 {
        Self::allocation_bytes(len + additional) - Self::allocation_bytes(len)
    }
    pub fn owned_chunks(&self) -> u64 {
        match &self.storage {
            ValueStorage::Inline(_) => 0,
            ValueStorage::Small(_) => 1,
            ValueStorage::Chunks { first, rest } => u64::from(first.is_some()) + rest.len() as u64,
        }
    }
    pub fn get(&self, index: u32) -> Option<Value> {
        if index >= self.len {
            return None;
        }
        match &self.storage {
            ValueStorage::Inline(values) => Some(values[index as usize]),
            ValueStorage::Small(values) => Some(values[index as usize]),
            ValueStorage::Chunks { first, rest } => {
                if (index as usize) < FIRST_VALUES {
                    first.as_ref().map(|values| values[index as usize])
                } else {
                    let offset = index as usize - FIRST_VALUES;
                    rest.get(&((offset / VALUE_CHUNK_LEN) as u32))
                        .map(|values| values[offset % VALUE_CHUNK_LEN])
                }
            }
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
            ValueStorage::Small(values) => &values[offset as usize..self.len as usize],
            ValueStorage::Chunks { first, .. } if (offset as usize) < FIRST_VALUES => {
                &first.as_ref().unwrap()[offset as usize..FIRST_VALUES]
            }
            ValueStorage::Chunks { rest, .. } => {
                let relative = offset as usize - FIRST_VALUES;
                let within = relative % VALUE_CHUNK_LEN;
                let count = (self.len - offset).min((VALUE_CHUNK_LEN - within) as u32) as usize;
                &rest[&((relative / VALUE_CHUNK_LEN) as u32)][within..within + count]
            }
        }
    }
    /// Mutable suffix of one already allocated block. Internal writers retain
    /// exclusive payload ownership and never return it across a heap operation.
    fn segment_mut(&mut self, offset: u32) -> &mut [Value] {
        if offset == self.len {
            return &mut [];
        }
        match &mut self.storage {
            ValueStorage::Inline(values) => &mut values[offset as usize..self.len as usize],
            ValueStorage::Small(values) => &mut values[offset as usize..self.len as usize],
            ValueStorage::Chunks { first, .. } if (offset as usize) < FIRST_VALUES => {
                &mut first.as_mut().unwrap()[offset as usize..FIRST_VALUES]
            }
            ValueStorage::Chunks { rest, .. } => {
                let relative = offset as usize - FIRST_VALUES;
                let within = relative % VALUE_CHUNK_LEN;
                let count = (self.len - offset).min((VALUE_CHUNK_LEN - within) as u32) as usize;
                &mut rest
                    .get_mut(&((relative / VALUE_CHUNK_LEN) as u32))
                    .unwrap()[within..within + count]
            }
        }
    }
    pub fn write_slice(&mut self, mut offset: u32, mut input: &[Value]) {
        while !input.is_empty() {
            let destination = self.segment_mut(offset);
            let count = input.len().min(destination.len());
            destination[..count].copy_from_slice(&input[..count]);
            offset += count as u32;
            input = &input[count..];
        }
    }
    pub fn extend_from_slice(&mut self, mut input: &[Value]) {
        while !input.is_empty() {
            if self.needs_chunk() {
                self.push(input[0]);
                input = &input[1..];
                continue;
            }
            let count = input.len().min(self.remaining_in_block());
            let start = self.len;
            self.len += count as u32;
            self.write_slice(start, &input[..count]);
            input = &input[count..];
        }
    }
    pub fn extend_repeated(&mut self, value: Value, mut remaining: usize) {
        while remaining != 0 {
            if self.needs_chunk() {
                self.push(value);
                remaining -= 1;
                continue;
            }
            let count = remaining.min(self.remaining_in_block());
            let start = self.len;
            self.len += count as u32;
            self.segment_mut(start).fill(value);
            remaining -= count;
        }
    }
    fn remaining_in_block(&self) -> usize {
        match &self.storage {
            ValueStorage::Inline(_) => INLINE_VALUES - self.len as usize,
            ValueStorage::Small(_) => FIRST_VALUES - self.len as usize,
            ValueStorage::Chunks { .. } => {
                VALUE_CHUNK_LEN - (self.len as usize - FIRST_VALUES) % VALUE_CHUNK_LEN
            }
        }
    }
    pub fn set(&mut self, index: u32, value: Value) {
        match &mut self.storage {
            ValueStorage::Inline(values) => values[index as usize] = value,
            ValueStorage::Small(values) => values[index as usize] = value,
            ValueStorage::Chunks { first, rest } => {
                if (index as usize) < FIRST_VALUES {
                    first.as_mut().unwrap()[index as usize] = value;
                } else {
                    let offset = index as usize - FIRST_VALUES;
                    rest.get_mut(&((offset / VALUE_CHUNK_LEN) as u32)).unwrap()
                        [offset % VALUE_CHUNK_LEN] = value;
                }
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
            let mut first = Box::new([Value::NULL; FIRST_VALUES]);
            first[..INLINE_VALUES].copy_from_slice(values);
            self.storage = ValueStorage::Small(first);
        }
        if self.len as usize == FIRST_VALUES {
            let ValueStorage::Small(first) = std::mem::replace(
                &mut self.storage,
                ValueStorage::Inline([Value::NULL; INLINE_VALUES]),
            ) else {
                unreachable!()
            };
            self.storage = ValueStorage::Chunks {
                first: Some(first),
                rest: BTreeMap::new(),
            };
        }
        if let ValueStorage::Chunks { rest, .. } = &mut self.storage
            && (self.len as usize - FIRST_VALUES).is_multiple_of(VALUE_CHUNK_LEN)
        {
            rest.insert(
                ((self.len as usize - FIRST_VALUES) / VALUE_CHUNK_LEN) as u32,
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
            if self.len as usize == INLINE_VALUES {
                let mut inline = [Value::NULL; INLINE_VALUES];
                let ValueStorage::Small(first) = &self.storage else {
                    unreachable!()
                };
                inline.copy_from_slice(&first[..INLINE_VALUES]);
                self.storage = ValueStorage::Inline(inline);
            } else {
                let ValueStorage::Chunks { first, rest } = &mut self.storage else {
                    unreachable!()
                };
                rest.remove(&(((self.len as usize - FIRST_VALUES) / VALUE_CHUNK_LEN) as u32));
                if rest.is_empty() {
                    self.storage = ValueStorage::Small(first.take().unwrap());
                }
            }
        }
        Some((value, freed))
    }
    /// Releases one allocated block from a dying object and returns its charge.
    pub fn release_chunk(&mut self, available_bytes: u64) -> Option<u64> {
        match &mut self.storage {
            ValueStorage::Small(_) if available_bytes >= FIRST_VALUE_BYTES => {
                *self = Self::default();
                Some(FIRST_VALUE_BYTES)
            }
            ValueStorage::Chunks {
                first: first @ Some(_),
                ..
            } if available_bytes >= FIRST_VALUE_BYTES => {
                *first = None;
                Some(FIRST_VALUE_BYTES)
            }
            ValueStorage::Chunks { first: None, rest } if available_bytes >= VALUE_CHUNK_BYTES => {
                rest.pop_first();
                if rest.is_empty() {
                    *rest = BTreeMap::new();
                }
                Some(VALUE_CHUNK_BYTES)
            }
            _ => None,
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
    Table(Table),
    Dependencies(Box<crate::dependencies::Dependencies>),
    Bytes(Bytes),
    Empty,
}
impl Payload {
    pub fn has_edges(&self) -> bool {
        match self {
            Self::Cell(cell) => cell
                .value
                .is_some_and(|value| value.tag() == crate::ValueTag::Object),
            Self::Values(values) | Self::Table(Table { values, .. }) => values.len != 0,
            Self::Dependencies(dependencies) => dependencies.values.len != 0,
            Self::Fault(_) => true,
            Self::Bytes(_) | Self::Empty => false,
        }
    }

    pub fn edge_segment(&self, cursor: u32) -> Option<&[Value]> {
        let values = match self {
            Self::Values(values) | Self::Table(Table { values, .. }) => values,
            Self::Dependencies(dependencies) => &dependencies.values,
            _ => return None,
        };
        (cursor < values.len).then(|| values.segment(cursor))
    }

    pub fn edge(&self, cursor: u32) -> Option<Value> {
        match self {
            Self::Cell(v) if cursor == 0 => v.value,
            Self::Values(v) | Self::Table(Table { values: v, .. }) => v.get(cursor),
            Self::Dependencies(d) => d.values.get(cursor),
            Self::Fault(p) => p.edges.get(cursor as usize).copied(),
            _ => None,
        }
    }
    pub fn len(&self) -> u32 {
        match self {
            Self::Cell(v) => u32::from(v.value.is_some()),
            Self::Values(v) | Self::Table(Table { values: v, .. }) => v.len,
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
        for len in [1, 4, 8, 9, 32, 33, 64, 96, 97] {
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
            } else if len as usize <= FIRST_VALUES {
                assert_eq!(cost.count_total, 1);
                assert_eq!(cost.bytes_current, FIRST_VALUE_BYTES as i64);
            }
            assert!(cost.bytes_max as u64 <= Values::allocation_bytes(len as u64));
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
            assert_eq!(charged, Values::allocation_bytes(values.len.into()));
            let mut offset = 0;
            while offset < values.len {
                let segment = values.segment(offset);
                assert!(!segment.is_empty() && segment.len() <= VALUE_CHUNK_LEN);
                for (index, value) in segment.iter().enumerate() {
                    assert_eq!(value.as_int(), Some(i64::from(offset) + index as i64));
                }
                offset += segment.len() as u32;
            }
        }
        for index in (0..130).rev() {
            let (value, freed) = values.pop().unwrap();
            assert_eq!(value.as_int(), Some(index));
            charged -= freed;
            assert_eq!(charged, Values::allocation_bytes(values.len.into()));
        }
        assert_eq!(charged, 0);
        assert_eq!(values.len, 0);
        assert_eq!(values.owned_chunks(), 0);
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
