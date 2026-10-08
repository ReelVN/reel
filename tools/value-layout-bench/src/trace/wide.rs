use crate::{Handle, FIELD_MAX};
use std::mem::size_of;

const NONE: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, Default)]
pub struct WideStats {
    pub live: usize,
    pub peak_live: usize,
    pub table_bytes: usize,
    pub box_bytes: usize,
    pub peak_requested_bytes: usize,
    pub value_allocations: u64,
    pub box_allocations: u64,
    pub box_deallocations: u64,
    pub retains: u64,
    pub releases: u64,
    pub table_allocations: u64,
    pub table_reallocations: u64,
}

/// Immutable wide ints only. This is explicit fixture ownership, not VM GC.
pub trait Wide: Default {
    fn allocate(&mut self, value: i64) -> Handle;
    fn get(&self, handle: Handle) -> Option<i64>;
    fn retain(&mut self, handle: Handle) -> bool;
    fn release(&mut self, handle: Handle) -> bool;
    fn stats(&self) -> WideStats;
}

#[derive(Default)]
pub struct NoWide;
impl Wide for NoWide {
    fn allocate(&mut self, _: i64) -> Handle {
        unreachable!("explicit int64 is inline")
    }
    fn get(&self, _: Handle) -> Option<i64> {
        None
    }
    fn retain(&mut self, _: Handle) -> bool {
        false
    }
    fn release(&mut self, _: Handle) -> bool {
        false
    }
    fn stats(&self) -> WideStats {
        WideStats::default()
    }
}

pub trait Payload {
    const BOX_BYTES: usize;
    fn new(value: i64) -> Self;
    fn get(&self) -> i64;
    fn clear(&mut self);
}

pub struct BoxPayload(Option<Box<i64>>);
impl Payload for BoxPayload {
    const BOX_BYTES: usize = 8;
    fn new(value: i64) -> Self {
        Self(Some(Box::new(value)))
    }
    fn get(&self) -> i64 {
        **self.0.as_ref().unwrap()
    }
    fn clear(&mut self) {
        self.0 = None;
    }
}

pub struct InlinePayload(i64);
impl Payload for InlinePayload {
    const BOX_BYTES: usize = 0;
    fn new(value: i64) -> Self {
        Self(value)
    }
    fn get(&self) -> i64 {
        self.0
    }
    fn clear(&mut self) {} // refs=0 marks the payload dead.
}

#[repr(C)]
struct Slot<P> {
    generation: u32,
    next: u32,
    refs: u32,
    payload: P,
}

pub struct WideStore<P> {
    slots: Vec<Slot<P>>,
    free: u32,
    stats: WideStats,
}

pub type BoxWide = WideStore<BoxPayload>;
pub type InlineWide = WideStore<InlinePayload>;

impl<P> Default for WideStore<P> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            free: NONE,
            stats: WideStats::default(),
        }
    }
}

impl<P: Payload> Wide for WideStore<P> {
    fn allocate(&mut self, value: i64) -> Handle {
        let index;
        if self.free == NONE {
            assert!(
                self.slots.len() <= FIELD_MAX as usize,
                "trace wide slots exhausted"
            );
            index = self.slots.len() as u32;
            let old_capacity = self.slots.capacity();
            self.slots.push(Slot {
                generation: 1,
                next: NONE,
                refs: 1,
                payload: P::new(value),
            });
            if old_capacity != self.slots.capacity() {
                if old_capacity == 0 {
                    self.stats.table_allocations += 1;
                } else {
                    self.stats.table_reallocations += 1;
                }
            }
        } else {
            index = self.free;
            let slot = &mut self.slots[index as usize];
            self.free = slot.next;
            slot.next = NONE;
            slot.refs = 1;
            slot.payload = P::new(value);
        }
        self.stats.live += 1;
        self.stats.value_allocations += 1;
        self.stats.box_allocations += u64::from(P::BOX_BYTES != 0);
        self.stats.peak_live = self.stats.peak_live.max(self.stats.live);
        self.stats.table_bytes = self.slots.capacity() * size_of::<Slot<P>>();
        self.stats.box_bytes = self.stats.live * P::BOX_BYTES;
        self.stats.peak_requested_bytes = self
            .stats
            .peak_requested_bytes
            .max(self.stats.table_bytes + self.stats.box_bytes);
        Handle::new(index, self.slots[index as usize].generation).unwrap()
    }
    fn get(&self, handle: Handle) -> Option<i64> {
        let slot = self.slots.get(handle.index() as usize)?;
        (slot.generation == handle.generation() && slot.refs != 0).then(|| slot.payload.get())
    }
    fn retain(&mut self, handle: Handle) -> bool {
        let Some(slot) = self.slots.get_mut(handle.index() as usize) else {
            return false;
        };
        if slot.generation != handle.generation() || slot.refs == 0 {
            return false;
        }
        slot.refs = slot
            .refs
            .checked_add(1)
            .expect("trace reference count overflow");
        self.stats.retains += 1;
        true
    }
    fn release(&mut self, handle: Handle) -> bool {
        let Some(slot) = self.slots.get_mut(handle.index() as usize) else {
            return false;
        };
        if slot.generation != handle.generation() || slot.refs == 0 {
            return false;
        }
        slot.refs -= 1;
        self.stats.releases += 1;
        if slot.refs == 0 {
            slot.payload.clear();
            self.stats.live -= 1;
            self.stats.box_deallocations += u64::from(P::BOX_BYTES != 0);
            self.stats.box_bytes = self.stats.live * P::BOX_BYTES;
            if slot.generation < FIELD_MAX {
                slot.generation += 1;
                slot.next = self.free;
                self.free = handle.index();
            }
        }
        true
    }
    fn stats(&self) -> WideStats {
        self.stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sharing_and_reuse<W: Wide>() {
        let mut wide = W::default();
        let h = wide.allocate(i64::MIN);
        assert!(wide.retain(h));
        assert!(wide.release(h));
        assert_eq!(wide.get(h), Some(i64::MIN));
        assert!(wide.release(h));
        assert_eq!(wide.get(h), None);
        assert!(!wide.release(h));
        let next = wide.allocate(i64::MAX);
        assert_eq!(h.index(), next.index());
        assert_ne!(h.generation(), next.generation());
        assert!(!wide.retain(h));
        assert!(wide.release(next));
        assert_eq!(wide.stats().live, 0);
        assert_eq!(wide.stats().peak_live, 1);
    }
    #[test]
    fn both_wide_stores_share_and_release() {
        sharing_and_reuse::<BoxWide>();
        sharing_and_reuse::<InlineWide>();
    }
    #[test]
    fn generation_exhaustion_retires_slot() {
        let mut wide = InlineWide::default();
        let h = wide.allocate(123);
        wide.slots[h.index() as usize].generation = FIELD_MAX;
        let last = Handle::new(h.index(), FIELD_MAX).unwrap();
        assert!(wide.release(last));
        let next = wide.allocate(456);
        assert_ne!(last.index(), next.index());
        assert!(wide.get(last).is_none());
    }
}
