//! Generational descriptors and the shared byte book. Never stores Value addresses.
use crate::slots::{RangeKey, index};
use crate::{Heap, HeapError, HeapId, RootKind, RootOwner, SessionEpoch};
use std::cell::{Cell, OnceCell};
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

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

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum RootProviderState {
    Detached,
    Attached,
    Executing,
}
impl RootProviderState {
    pub(crate) fn is_alive(self) -> bool {
        self != Self::Detached
    }
}

pub(crate) struct ByteBook {
    limit: u64,
    used: Cell<u64>,
    peak: Cell<u64>,
    pub heap_alive: Cell<bool>,
    pub attachment: Cell<Option<u64>>,
    pub(crate) provider_state: Cell<RootProviderState>,
    shared_refunds: OnceCell<Arc<AtomicUsize>>,
    shared_bytes: Cell<usize>,
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
            provider_state: Cell::new(RootProviderState::Detached),
            shared_refunds: OnceCell::new(),
            shared_bytes: Cell::new(0),
        }))
    }
    pub fn charged_bytes(&self) -> u64 {
        self.collect_shared_refunds();
        self.0.used.get()
    }
    pub fn peak_charged_bytes(&self) -> u64 {
        self.collect_shared_refunds();
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
    /// Admit storage whose final owner may be released by another thread.
    /// Ordinary VM allocations retain their non-atomic fast path.
    pub fn reserve_shared(&self, bytes: u64) -> Result<SharedHeapReservation, HeapError> {
        self.collect_shared_refunds();
        self.reserve(bytes)?.into_shared()
    }
    fn collect_shared_refunds(&self) {
        if let Some(refunds) = self.0.shared_refunds.get() {
            let bytes = refunds.swap(0, Ordering::Acquire);
            self.0.shared_bytes.set(self.0.shared_bytes.get() - bytes);
            self.refund(bytes as u64);
        }
    }
    pub(crate) fn ensure(&self, bytes: u64) -> Result<(), HeapError> {
        if bytes <= self.0.limit.saturating_sub(self.0.used.get()) {
            return Ok(());
        }
        self.collect_shared_refunds();
        if bytes > self.0.limit.saturating_sub(self.0.used.get()) {
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

// A VM exclusively owns Heap/FrameSlots during run. The guard owns an existing
// accounting Rc; Drop ends the proof on every return and unwind. Public
// FrameSlots operations retain their complete validation.
pub(crate) struct VmAccessGuard {
    usage: HeapBudgetUsage,
    attachment: u64,
}
impl VmAccessGuard {
    pub(crate) fn enter(usage: &HeapBudgetUsage, attachment: u64) -> Self {
        debug_assert!(usage.0.provider_state.get() == RootProviderState::Attached);
        usage.0.provider_state.set(RootProviderState::Executing);
        Self {
            usage: usage.clone(),
            attachment,
        }
    }
}
impl Drop for VmAccessGuard {
    fn drop(&mut self) {
        if self.usage.0.attachment.get() == Some(self.attachment)
            && self.usage.0.provider_state.get() == RootProviderState::Executing
        {
            self.usage.0.provider_state.set(RootProviderState::Attached);
        }
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
    /// Transfer this charge to a sendable final-owner token without refunding it.
    pub fn into_shared(mut self) -> Result<SharedHeapReservation, HeapError> {
        self.usage.collect_shared_refunds();
        let bytes = usize::try_from(self.bytes).map_err(|_| HeapError::InvalidLimits)?;
        let outstanding = self
            .usage
            .0
            .shared_bytes
            .get()
            .checked_add(bytes)
            .ok_or(HeapError::InvalidLimits)?;
        if self.usage.0.shared_refunds.get().is_none() {
            let overhead = (size_of::<AtomicUsize>() + 2 * size_of::<usize>()) as u64;
            self.usage.ensure(overhead)?;
            self.usage
                .0
                .shared_refunds
                .set(Arc::new(AtomicUsize::new(0)))
                .unwrap();
            self.usage.charge(overhead);
        }
        let refunds = self.usage.0.shared_refunds.get().unwrap().clone();
        self.usage.0.shared_bytes.set(outstanding);
        self.bytes = 0;
        Ok(SharedHeapReservation { refunds, bytes })
    }
    /// Adjust accounting before growing or after releasing host-owned storage.
    pub fn resize(&mut self, bytes: u64) -> Result<(), HeapError> {
        if bytes > self.bytes {
            self.usage.ensure(bytes - self.bytes)?;
            self.usage.charge(bytes - self.bytes);
        } else {
            self.usage.refund(self.bytes - bytes);
        }
        self.bytes = bytes;
        Ok(())
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        self.usage.refund(self.bytes);
    }
}

/// A storage charge released with its last owning allocation. It contains no VM
/// handles and keeps the refund mailbox alive after the original heap is dropped.
pub struct SharedHeapReservation {
    refunds: Arc<AtomicUsize>,
    bytes: usize,
}
impl SharedHeapReservation {
    /// Bytes retained by this allocation owner.
    pub fn bytes(&self) -> u64 {
        self.bytes as u64
    }
}
impl Drop for SharedHeapReservation {
    fn drop(&mut self) {
        self.refunds.fetch_add(self.bytes, Ordering::Release);
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
        self.range_occupied[index(key) / 64] |= 1 << (index(key) % 64);
        self.range_scan_len = self.range_scan_len.max(index(key) + 1);
        Ok(RootRangeToken::new(self, key))
    }
    pub(crate) fn unregister_range(&mut self, token: RootRangeToken) -> Result<(), HeapError> {
        self.range_descriptor(token)?;
        self.range_index[index(token.key)] = None;
        self.range_occupied[index(token.key) / 64] &= !(1 << (index(token.key) % 64));
        self.ranges
            .remove(token.key)
            .expect("checked range descriptor");
        Ok(())
    }
}

#[cfg(test)]
mod shared_reservation_tests {
    use super::*;

    #[test]
    fn ordinary_reservations_keep_the_refund_mailbox_unallocated() {
        let usage = HeapBudgetUsage::new(4096);
        let before = usage.charged_bytes();
        let mut charge = usage.reserve(256).unwrap();
        charge.resize(512).unwrap();
        drop(charge);
        assert_eq!(usage.charged_bytes(), before);
        assert!(usage.0.shared_refunds.get().is_none());
    }

    #[test]
    fn shared_final_release_refunds_statistics_and_allocation_pressure() {
        let usage = HeapBudgetUsage::new(4096);
        let shared = usage.reserve_shared(1024).unwrap();
        let before = usage.charged_bytes();
        std::thread::spawn(move || drop(shared)).join().unwrap();
        assert_eq!(usage.charged_bytes(), before - 1024);
        let shared = usage.reserve_shared(1024).unwrap();
        let fill = usage.reserve(usage.remaining_bytes()).unwrap();
        std::thread::spawn(move || drop(shared)).join().unwrap();
        // No statistics read between the final release and the allocation.
        let reused = usage.reserve(1024).unwrap();
        assert_eq!(usage.charged_bytes(), usage.limit());
        drop((fill, reused));
        let final_owner = usage.reserve_shared(64).unwrap();
        drop(usage);
        std::thread::spawn(move || drop(final_owner))
            .join()
            .unwrap();
    }
}
