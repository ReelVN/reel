//! Private dependency payload: ordered strong edges plus an identity-only index.
use crate::payload::Values;
use crate::{HeapError, Value};
use std::collections::BTreeSet;

// Conservative admission bounds for std's tree nodes (including internal nodes).
// Allocator measurements cover sparse and dense cardinalities in the tests below.
pub(crate) const INDEX_ENTRY_BYTES: u64 = 128;
pub(crate) const BASE_BYTES: u64 = std::mem::size_of::<Dependencies>() as u64 + 512;

#[derive(Default)]
pub(crate) struct Dependencies {
    pub values: Values,
    // Inline captures use bounded pair probes and allocate no tree nodes.
    pub index: BTreeSet<(u64, u32)>,
    pub index_reserved: u64,
    // Non-owning offsets into `values`, rebuilt after binding topology changes.
    pub ancestors: Vec<u32>,
    pub ancestor_revision: u64,
    // Non-owning mark for one synchronous freshness walk; restored cold.
    pub freshness_visit: u64,
    pub ancestor_reserved: u64,
}
impl Dependencies {
    const INLINE_PAIRS: u32 = crate::payload::INLINE_VALUES as u32 / 2;

    fn contains_key(&self, key: (u64, u32)) -> bool {
        if !self.index.is_empty() {
            return self.index.contains(&key);
        }
        (0..self.values.len / 2).any(|pair| {
            Self::key(
                self.values.get(pair * 2).unwrap(),
                self.values.get(pair * 2 + 1).unwrap(),
            ) == Ok(key)
        })
    }

    fn index_bytes(pairs: u32) -> u64 {
        if pairs <= Self::INLINE_PAIRS {
            0
        } else {
            u64::from(pairs) * INDEX_ENTRY_BYTES
        }
    }

    fn insert_key(&mut self, key: (u64, u32)) {
        let pairs = self.values.len / 2 + 1;
        if pairs > Self::INLINE_PAIRS {
            if self.index.is_empty() {
                for pair in 0..self.values.len / 2 {
                    self.index.insert(
                        Self::key(
                            self.values.get(pair * 2).unwrap(),
                            self.values.get(pair * 2 + 1).unwrap(),
                        )
                        .unwrap(),
                    );
                }
            }
            self.index.insert(key);
        }
        self.index_reserved = Self::index_bytes(pairs);
    }

    fn key(source: Value, selector: Value) -> Result<(u64, u32), HeapError> {
        let (_, source) = source.object_parts().ok_or(HeapError::WrongKind)?;
        let selector = selector.as_int().ok_or(HeapError::WrongKind)?;
        if !(-1..=i64::from(u32::MAX - 1)).contains(&selector) {
            return Err(HeapError::OutOfBounds);
        }
        Ok((source, selector as u32))
    }
    pub fn contains(&self, source: Value, field: Option<u32>) -> bool {
        self.contains_key((source.object_parts().unwrap().1, field.unwrap_or(u32::MAX)))
    }
    pub fn overlaps(&self, source: Value, field: Option<u32>) -> bool {
        let key = source.object_parts().unwrap().1;
        if self.index.is_empty() {
            return (0..self.values.len / 2).any(|pair| {
                let source = self.values.get(pair * 2).unwrap().object_parts().unwrap().1;
                let selector = self.values.get(pair * 2 + 1).unwrap().as_int().unwrap() as u32;
                source == key && field.is_none_or(|field| selector == field || selector == u32::MAX)
            });
        }
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
        Ok(Values::growth_bytes(self.values.len.into(), 2)
            + Self::index_bytes(self.values.len / 2 + 1)
            - self.index_reserved)
    }
    // Both values and the index are admitted before this mutation. No fallible
    // operation can leave a published pair without its corresponding index entry.
    pub fn push_pair(&mut self, source: Value, selector: Value) {
        let key = Self::key(source, selector).expect("validated dependency");
        self.insert_key(key);
        self.values.push(source);
        self.values.push(selector);
        self.ancestor_revision = 0;
    }
    pub fn restore_bytes(&self, value: Value) -> Result<u64, HeapError> {
        if self.values.len.is_multiple_of(2) {
            value.object_parts().ok_or(HeapError::WrongKind)?;
        } else {
            let source = self.values.get(self.values.len - 1).unwrap();
            let key = Self::key(source, value)?;
            if self.contains_key(key) {
                return Err(HeapError::WrongKind);
            }
        }
        Ok(Values::growth_bytes(self.values.len.into(), 1)
            + if self.values.len % 2 == 1 {
                Self::index_bytes(self.values.len / 2 + 1) - self.index_reserved
            } else {
                0
            })
    }
    pub fn restore_push(&mut self, value: Value) {
        if self.values.len % 2 == 1 {
            self.insert_key(
                Self::key(self.values.get(self.values.len - 1).unwrap(), value).unwrap(),
            );
        }
        self.values.push(value);
    }

    /// Recycle only inline captures. Removing at most eight strong edges keeps
    /// normal completion, failure and cancellation bounded; larger captures retain
    /// their existing incremental-GC release path.
    pub fn recycle_small(&mut self) -> bool {
        if self.values.len > Self::INLINE_PAIRS * 2 {
            return false;
        }
        while self.values.pop().is_some() {}
        debug_assert!(self.index.is_empty() && self.index_reserved == 0);
        self.ancestors.clear();
        self.ancestor_revision = 0;
        true
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
