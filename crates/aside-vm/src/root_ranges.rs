//! Generational descriptors and the shared byte book. Never stores Value addresses.
use crate::slots::{RangeKey, index};
use crate::{Heap, HeapError, HeapId, RootKind, RootOwner, SessionEpoch};
use std::cell::Cell;
use std::marker::PhantomData;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RootRangeToken {
    pub(crate) heap: HeapId,
    pub(crate) epoch: SessionEpoch,
    pub(crate) key: RangeKey,
    _thread: PhantomData<Rc<()>>,
}
impl RootRangeToken {
    pub fn heap_id(self) -> HeapId {
        self.heap
    }
    pub fn epoch(self) -> SessionEpoch {
        self.epoch
    }
    pub(crate) fn new(heap: &Heap, key: RangeKey) -> Self {
        Self {
            heap: heap.id,
            epoch: heap.epoch,
            key,
            _thread: PhantomData,
        }
    }
}
impl serde::Serialize for RootRangeToken {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use slotmap::Key;
        serde::Serialize::serialize(&self.key.data().as_ffi(), s)
    }
}
impl<'de> serde::Deserialize<'de> for RootRangeToken {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let key = <u64 as serde::Deserialize>::deserialize(d)?;
        if crate::slots::heap_key(key).is_none() {
            return Err(serde::de::Error::custom("invalid snapshot range"));
        }
        Ok(Self {
            heap: HeapId(0),
            epoch: SessionEpoch(0),
            key: slotmap::KeyData::from_ffi(key).into(),
            _thread: PhantomData,
        })
    }
}
impl crate::snapshot::SnapshotMap {
    pub(crate) fn rebase_range(
        &self,
        range: RootRangeToken,
    ) -> Result<RootRangeToken, crate::snapshot::SnapshotError> {
        use slotmap::Key;
        self.check(range.heap, Some(range.epoch))?;
        self.ranges
            .get(&range.key.data().as_ffi())
            .copied()
            .ok_or_else(|| crate::snapshot::SnapshotError::Invalid("missing root range".into()))
    }
}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub(crate) struct RangeDescriptor {
    pub owner: RootOwner,
    pub kind: RootKind,
    pub len: u32,
}

pub(crate) struct ByteBook {
    limit: u64,
    used: Cell<u64>,
    peak: Cell<u64>,
    pub heap_alive: Cell<bool>,
    pub attachment: Cell<Option<u64>>,
    pub provider_alive: Cell<bool>,
}
// Rc's two counts plus value; conservative stack/slot accounting is separate.
pub(crate) const BOOK_BYTES: u64 =
    (std::mem::size_of::<ByteBook>() + 2 * std::mem::size_of::<usize>()) as u64;

/// A live accounting observation and reservation source; cloning allocates nothing.
#[derive(Clone)]
pub struct HeapBudgetUsage(pub(crate) Rc<ByteBook>);
impl HeapBudgetUsage {
    pub(crate) fn new(limit: u64) -> Self {
        Self(Rc::new(ByteBook {
            limit,
            used: Cell::new(BOOK_BYTES),
            peak: Cell::new(BOOK_BYTES),
            heap_alive: Cell::new(true),
            attachment: Cell::new(None),
            provider_alive: Cell::new(false),
        }))
    }
    pub fn charged_bytes(&self) -> u64 {
        self.0.used.get()
    }
    pub fn peak_charged_bytes(&self) -> u64 {
        self.0.peak.get()
    }
    pub fn remaining_bytes(&self) -> u64 {
        self.0.limit.saturating_sub(self.0.used.get())
    }
    pub fn limit(&self) -> u64 {
        self.0.limit
    }
    /// Charge host-owned temporary storage against the same heap budget.
    /// Keep the reservation until the corresponding allocation has been released.
    pub fn reserve(&self, bytes: u64) -> Result<Reservation, HeapError> {
        Reservation::new(self, bytes)
    }
    pub(crate) fn ensure(&self, bytes: u64) -> Result<(), HeapError> {
        if bytes > self.remaining_bytes() {
            Err(HeapError::ResourceLimit {
                requested: bytes,
                used: self.charged_bytes(),
                limit: self.limit(),
            })
        } else {
            Ok(())
        }
    }
    pub(crate) fn charge(&self, bytes: u64) {
        let used = self
            .0
            .used
            .get()
            .checked_add(bytes)
            .expect("checked byte reservation");
        assert!(used <= self.0.limit);
        self.0.used.set(used);
        self.0.peak.set(self.0.peak.get().max(used));
    }
    pub(crate) fn refund(&self, bytes: u64) {
        self.0.used.set(
            self.0
                .used
                .get()
                .checked_sub(bytes)
                .expect("owned byte reservation"),
        );
    }
}

pub struct Reservation {
    usage: HeapBudgetUsage,
    bytes: u64,
}
impl Reservation {
    pub fn new(usage: &HeapBudgetUsage, bytes: u64) -> Result<Self, HeapError> {
        usage.ensure(bytes)?;
        usage.charge(bytes);
        Ok(Self {
            usage: usage.clone(),
            bytes,
        })
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        self.usage.refund(self.bytes);
    }
}

// Field order is intentional: data deallocates before its reservation is refunded.
pub(crate) struct ChargedVec<T> {
    pub data: Vec<T>,
    charge: Reservation,
}
impl<T> ChargedVec<T> {
    pub fn new(usage: &HeapBudgetUsage, capacity: usize) -> Result<Self, HeapError> {
        let bytes = (capacity as u64)
            .checked_mul(std::mem::size_of::<T>() as u64)
            .filter(|&n| n <= isize::MAX as u64)
            .ok_or(HeapError::InvalidLimits)?;
        let charge = Reservation::new(usage, bytes)?;
        let mut data = Vec::new();
        data.try_reserve_exact(capacity)
            .map_err(|_| HeapError::AllocationFailure)?;
        // The pinned Rust 1.96 Global allocator requests exactly this layout.
        // Refuse an adapter change instead of publishing under-accounted capacity.
        if data.capacity() != capacity {
            return Err(HeapError::AllocationFailure);
        }
        Ok(Self { data, charge })
    }
    pub fn bytes(&self) -> u64 {
        self.charge.bytes()
    }
}

impl Heap {
    pub fn budget_usage(&self) -> HeapBudgetUsage {
        self.usage.clone()
    }
    pub(crate) fn range_descriptor(
        &self,
        token: RootRangeToken,
    ) -> Result<RangeDescriptor, HeapError> {
        if token.heap != self.id {
            return Err(HeapError::WrongHeap);
        }
        if token.epoch != self.epoch {
            return Err(HeapError::WrongEpoch);
        }
        self.ranges
            .get(token.key)
            .copied()
            .ok_or(HeapError::InvalidRootRange)
    }
    pub(crate) fn register_null_range(
        &mut self,
        owner: RootOwner,
        kind: RootKind,
        len: u32,
    ) -> Result<RootRangeToken, HeapError> {
        if self.ranges.full() {
            return Err(HeapError::RootRangeCapacity);
        }
        let key = self.ranges.insert(RangeDescriptor { owner, kind, len });
        self.range_index[index(key)] = Some(key);
        self.range_scan_len = self.range_scan_len.max(index(key) + 1);
        Ok(RootRangeToken::new(self, key))
    }
    pub(crate) fn unregister_range(&mut self, token: RootRangeToken) -> Result<(), HeapError> {
        self.range_descriptor(token)?;
        self.range_index[index(token.key)] = None;
        self.ranges
            .remove(token.key)
            .expect("checked range descriptor");
        Ok(())
    }
}
