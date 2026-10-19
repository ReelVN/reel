//! Private dependency payload: ordered strong edges plus an identity-only index.
use crate::payload::{VALUE_CHUNK_BYTES, Values};
use crate::{HeapError, Value};
use std::collections::BTreeSet;

// Conservative admission bounds for std's tree nodes (including internal nodes).
// Allocator measurements cover sparse and dense cardinalities in the tests below.
pub(crate) const INDEX_ENTRY_BYTES: u64 = 128;
pub(crate) const BASE_BYTES: u64 = std::mem::size_of::<Dependencies>() as u64 + 512;

#[derive(Default)]
pub(crate) struct Dependencies {
    pub values: Values,
    pub index: BTreeSet<(u64, u32)>,
    pub index_reserved: u64,
}
impl Dependencies {
    fn key(source: Value, selector: Value) -> Result<(u64, u32), HeapError> {
        let (_, source) = source.object_parts().ok_or(HeapError::WrongKind)?;
        let selector = selector.as_int().ok_or(HeapError::WrongKind)?;
        if !(-1..=i64::from(u32::MAX - 1)).contains(&selector) {
            return Err(HeapError::OutOfBounds);
        }
        Ok((source, selector as u32))
    }
    pub fn contains(&self, source: Value, field: Option<u32>) -> bool {
        self.index
            .contains(&(source.object_parts().unwrap().1, field.unwrap_or(u32::MAX)))
    }
    pub fn overlaps(&self, source: Value, field: Option<u32>) -> bool {
        let key = source.object_parts().unwrap().1;
        match field {
            Some(field) => {
                self.index.contains(&(key, field)) || self.index.contains(&(key, u32::MAX))
            }
            None => self
                .index
                .range((key, 0)..=(key, u32::MAX))
                .next()
                .is_some(),
        }
    }
    pub fn pair_bytes(&self) -> Result<u64, HeapError> {
        self.values
            .len
            .checked_add(2)
            .ok_or(HeapError::OutOfBounds)?;
        Ok(Values::growth_bytes(self.values.len.into(), 2) + INDEX_ENTRY_BYTES)
    }
    // Both values and the index are admitted before this mutation. No fallible
    // operation can leave a published pair without its corresponding index entry.
    pub fn push_pair(&mut self, source: Value, selector: Value) {
        let key = Self::key(source, selector).expect("validated dependency");
        self.index.insert(key);
        self.index_reserved += INDEX_ENTRY_BYTES;
        self.values.push(source);
        self.values.push(selector);
    }
    pub fn restore_bytes(&self, value: Value) -> Result<u64, HeapError> {
        if self.values.len.is_multiple_of(2) {
            value.object_parts().ok_or(HeapError::WrongKind)?;
        } else {
            let source = self.values.get(self.values.len - 1).unwrap();
            let key = Self::key(source, value)?;
            if self.index.contains(&key) {
                return Err(HeapError::WrongKind);
            }
        }
        Ok(u64::from(self.values.needs_chunk()) * VALUE_CHUNK_BYTES
            + if self.values.len % 2 == 1 {
                INDEX_ENTRY_BYTES
            } else {
                0
            })
    }
    pub fn restore_push(&mut self, value: Value) {
        if self.values.len % 2 == 1 {
            self.index
                .insert(Self::key(self.values.get(self.values.len - 1).unwrap(), value).unwrap());
            self.index_reserved += INDEX_ENTRY_BYTES;
        }
        self.values.push(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dependency_index_reservation_covers_growth_and_sparse_release() {
        for n in [1, 4, 5, 11, 12, 32, 64, 128, 1024, 100_000] {
            let mut payload = Box::<Dependencies>::default();
            let allocation = allocation_counter::measure(|| {
                for i in 0..n {
                    payload.push_pair(Value::object(crate::HeapId(1), i + 1), Value::int(0));
                }
            });
            let charge = BASE_BYTES + payload.index_reserved + Values::allocation_bytes(n * 2);
            assert!(
                allocation.bytes_max as u64 <= charge,
                "n={n}: {allocation:?}, reserve={charge}"
            );
            let mut previous = n as usize;
            while !payload.index.is_empty() {
                payload.index.pop_first();
                assert_eq!(payload.index.len(), previous - 1);
                previous -= 1;
            }
        }
    }
}
