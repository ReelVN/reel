//! Version-specific slotmap 1.1.1 adaptation. Never truncate keys for storage.
use crate::{HeapBudgetUsage, HeapError};
use slotmap::{Key, KeyData, SlotMap, new_key_type};
use std::num::NonZeroU32;

new_key_type! { pub(crate) struct HeapKey; pub(crate) struct RootKey; pub(crate) struct RangeKey; }

pub(crate) fn heap_key(raw: u64) -> Option<HeapKey> {
    let data = KeyData::from_ffi(raw);
    // from_ffi normalizes even generations: reject anything not a genuine round trip.
    (data.as_ffi() == raw).then(|| data.into())
}

pub(crate) fn generation<K: Key>(key: K) -> u32 {
    (key.data().as_ffi() >> 32) as u32
}
pub(crate) fn index<K: Key>(key: K) -> usize {
    key.data().as_ffi() as u32 as usize
}

pub(crate) enum Slot<T> {
    Occupied(T),
    Retired,
}

const OBJECTS_PER_PAGE: usize = 32;
// Pinned slotmap 1.1.1: a four-byte position/free-list union and a u32 version.
const POSITION_SLOT_BYTES: u64 = 8;

struct ObjectEntry<T> {
    key: HeapKey,
    value: T,
}

/// Generations outlive vacant slots; the much larger entities do not.
/// All entity movement stays behind the ordinary stable HeapKey lookup.
pub(crate) struct ObjectSlots<T> {
    positions: Option<SlotMap<HeapKey, Option<NonZeroU32>>>,
    pages: Vec<Vec<ObjectEntry<T>>>,
    len: usize,
    limit: usize,
    retired: usize,
    retirement_version: u32,
}

impl<T> ObjectSlots<T> {
    pub fn new(initial: usize, limit: usize, retirement_version: u32) -> Self {
        Self {
            positions: Some(SlotMap::with_capacity_and_key(initial)),
            pages: Vec::new(),
            len: 0,
            limit,
            retired: 0,
            retirement_version,
        }
    }

    pub fn full(&self) -> bool {
        self.len + self.retired >= self.limit
    }

    pub fn reserve_one(&mut self, usage: &HeapBudgetUsage) -> Result<(), HeapError> {
        let position_capacity = self.position_growth_capacity(1);
        let positions = self.positions.as_mut().unwrap();
        if let Some(capacity) = position_capacity {
            let old = positions.capacity();
            let reservation = usage.reserve((capacity as u64 + 1) * POSITION_SLOT_BYTES)?;
            let result = positions.try_reserve(1);
            let added = (positions.capacity() - old) as u64 * POSITION_SLOT_BYTES;
            drop(reservation);
            usage.charge(added);
            result.map_err(|_| HeapError::AllocationFailure)?;
        }
        if self.len == self.pages.len() * OBJECTS_PER_PAGE {
            if let Some(capacity) = self.page_growth_capacity(1) {
                let old = self.pages.capacity();
                let reservation =
                    usage.reserve((capacity * size_of::<Vec<ObjectEntry<T>>>()) as u64)?;
                let result = self.pages.try_reserve(1);
                let added =
                    ((self.pages.capacity() - old) * size_of::<Vec<ObjectEntry<T>>>()) as u64;
                drop(reservation);
                usage.charge(added);
                result.map_err(|_| HeapError::AllocationFailure)?;
            }
            let mut page = Vec::new();
            let capacity = self.limit.min(OBJECTS_PER_PAGE);
            let bytes = (capacity * size_of::<ObjectEntry<T>>()) as u64;
            let reservation = usage.reserve(bytes)?;
            page.try_reserve_exact(capacity)
                .map_err(|_| HeapError::AllocationFailure)?;
            self.pages.push(page);
            drop(reservation);
            usage.charge(bytes);
        }
        Ok(())
    }

    fn position_growth_capacity(&self, additional: usize) -> Option<usize> {
        let positions = self.positions.as_ref().unwrap();
        Self::position_growth(positions.len() + additional, positions.capacity())
    }

    fn position_growth(required: usize, capacity: usize) -> Option<usize> {
        (required > capacity).then(|| ((capacity + 1) * 2).max(4).max(required + 1) - 1)
    }

    fn page_growth_capacity(&self, additional: usize) -> Option<usize> {
        let required = (self.len + additional).div_ceil(OBJECTS_PER_PAGE);
        Self::page_growth(required, self.pages.capacity())
    }

    fn page_growth(required: usize, capacity: usize) -> Option<usize> {
        (required > capacity).then(|| (capacity * 2).max(4).max(required))
    }

    /// Allocation traffic for a bounded group, including replacement capacities.
    /// The constant materializer requests at most two fresh objects at once.
    pub(crate) fn allocation_bytes(&self, additional: usize) -> u64 {
        let additional = additional.min(self.limit.saturating_sub(self.len + self.retired));
        let positions = self.positions.as_ref().unwrap();
        let mut position_capacity = positions.capacity();
        let mut page_capacity = self.pages.capacity();
        let mut pages = self.pages.len();
        let mut bytes = 0;
        for index in 1..=additional {
            if let Some(capacity) =
                Self::position_growth(positions.len() + index, position_capacity)
            {
                position_capacity = capacity;
                bytes += (capacity as u64 + 1) * POSITION_SLOT_BYTES;
            }
            let required = (self.len + index).div_ceil(OBJECTS_PER_PAGE);
            if let Some(capacity) = Self::page_growth(required, page_capacity) {
                page_capacity = capacity;
                bytes += (capacity * size_of::<Vec<ObjectEntry<T>>>()) as u64;
            }
            if required > pages {
                pages = required;
                bytes += (self.limit.min(OBJECTS_PER_PAGE) * size_of::<ObjectEntry<T>>()) as u64;
            }
        }
        bytes
    }

    pub fn insert(&mut self, value: T) -> HeapKey {
        assert!(!self.full());
        let position = NonZeroU32::new(self.len as u32 + 1).unwrap();
        let key = self.positions.as_mut().unwrap().insert(Some(position));
        self.pages[self.len / OBJECTS_PER_PAGE].push(ObjectEntry { key, value });
        self.len += 1;
        key
    }

    pub fn get(&self, key: HeapKey) -> Option<&T> {
        let position = self.positions.as_ref().unwrap().get(key)?.as_ref()?.get() as usize - 1;
        Some(&self.pages[position / OBJECTS_PER_PAGE][position % OBJECTS_PER_PAGE].value)
    }

    pub fn get_mut(&mut self, key: HeapKey) -> Option<&mut T> {
        let position = self.positions.as_ref().unwrap().get(key)?.as_ref()?.get() as usize - 1;
        Some(&mut self.pages[position / OBJECTS_PER_PAGE][position % OBJECTS_PER_PAGE].value)
    }

    pub fn remove(&mut self, key: HeapKey) -> Option<T> {
        let positions = self.positions.as_mut().unwrap();
        let position = if generation(key) >= self.retirement_version {
            let position = positions.get_mut(key)?.take()?;
            self.retired += 1;
            position
        } else {
            positions.remove(key)??
        }
        .get() as usize
            - 1;
        self.len -= 1;
        let last = self.pages[self.len / OBJECTS_PER_PAGE].pop().unwrap();
        let removed = if position == self.len {
            last
        } else {
            *positions.get_mut(last.key).unwrap() = NonZeroU32::new(position as u32 + 1);
            std::mem::replace(
                &mut self.pages[position / OBJECTS_PER_PAGE][position % OBJECTS_PER_PAGE],
                last,
            )
        };
        // A removal does constant work, including at most one allocator release.
        if self.pages.last().is_some_and(Vec::is_empty) {
            self.pages.pop();
        }
        Some(removed.value)
    }

    pub fn occupied(&self) -> usize {
        self.len
    }
    pub fn retired(&self) -> usize {
        self.retired
    }
    pub fn reserved_bytes(&self) -> u64 {
        (self.positions.as_ref().unwrap().capacity() as u64 + 1) * POSITION_SLOT_BYTES
            + (self.pages.capacity() * size_of::<Vec<ObjectEntry<T>>>()) as u64
            + (self.pages.len() * self.limit.min(OBJECTS_PER_PAGE) * size_of::<ObjectEntry<T>>())
                as u64
    }
    pub fn bytes_for(initial: usize) -> u64 {
        (initial as u64 + 1) * POSITION_SLOT_BYTES
    }
    pub fn drop_storage(&mut self) {
        drop(self.positions.take());
        drop(std::mem::take(&mut self.pages));
    }
}

pub(crate) struct Slots<K: Key, T> {
    map: Option<SlotMap<K, Slot<T>>>,
    limit: usize,
    retired: usize,
    retirement_version: u32,
}

impl<K: Key, T> Slots<K, T> {
    pub fn new(initial: usize, limit: usize, retirement_version: u32) -> Self {
        Self {
            map: Some(SlotMap::with_capacity_and_key(initial)),
            limit,
            retired: 0,
            retirement_version,
        }
    }
    pub fn full(&self) -> bool {
        self.map.as_ref().unwrap().len() >= self.limit
    }
    pub fn growth_capacity(&self) -> Option<usize> {
        self.growth_capacity_for(1)
    }
    pub fn growth_capacity_for(&self, additional: usize) -> Option<usize> {
        let map = self.map.as_ref().unwrap();
        let required = map.len() + additional;
        (required > map.capacity()).then(|| ((map.capacity() + 1) * 2).max(4).max(required + 1) - 1)
    }
    pub fn admits(&self, additional: usize) -> bool {
        self.map.as_ref().unwrap().len() + additional <= self.limit
    }
    /// The pinned slotmap delegates growth to Vec, including its sentinel.
    /// Callers prepare all side tables before allowing insertion to grow.
    pub fn reserve_one(&mut self, usage: &HeapBudgetUsage) -> Result<u64, HeapError> {
        self.reserve(usage, 1)
    }
    pub fn reserve(
        &mut self,
        usage: &HeapBudgetUsage,
        additional: usize,
    ) -> Result<u64, HeapError> {
        let Some(capacity) = self.growth_capacity_for(additional) else {
            return Ok(0);
        };
        let old = self.reserved_bytes();
        let reservation = usage.reserve(Self::bytes_for(capacity))?;
        let result = self.map.as_mut().unwrap().try_reserve(additional);
        let added = self.reserved_bytes() - old;
        drop(reservation);
        usage.charge(added);
        result.map_err(|_| HeapError::AllocationFailure)?;
        Ok(added)
    }
    pub fn insert(&mut self, value: T) -> K {
        assert!(!self.full()); // Caller checks capacity before taking payload ownership.
        self.map.as_mut().unwrap().insert(Slot::Occupied(value))
    }
    pub fn get(&self, key: K) -> Option<&T> {
        match self.map.as_ref().unwrap().get(key)? {
            Slot::Occupied(v) => Some(v),
            Slot::Retired => None,
        }
    }
    pub fn get_mut(&mut self, key: K) -> Option<&mut T> {
        match self.map.as_mut().unwrap().get_mut(key)? {
            Slot::Occupied(v) => Some(v),
            Slot::Retired => None,
        }
    }
    pub fn remove(&mut self, key: K) -> Option<T> {
        if generation(key) >= self.retirement_version {
            let slot = self.map.as_mut().unwrap().get_mut(key)?;
            match std::mem::replace(slot, Slot::Retired) {
                Slot::Occupied(v) => {
                    self.retired += 1;
                    Some(v)
                }
                Slot::Retired => None,
            }
        } else {
            match self.map.as_mut().unwrap().remove(key)? {
                Slot::Occupied(v) => Some(v),
                Slot::Retired => unreachable!(),
            }
        }
    }
    pub fn retired(&self) -> usize {
        self.retired
    }
    pub fn occupied(&self) -> usize {
        self.map.as_ref().unwrap().len() - self.retired
    }
    pub fn reserved_bytes(&self) -> u64 {
        Self::bytes_for(self.map.as_ref().unwrap().capacity())
    }
    pub fn bytes_for(capacity: usize) -> u64 {
        // Conservative upper bound for private SlotUnion<V> + u32 version + padding,
        // including the sentinel. Checked constructor bounds keep products in u64.
        let stride = std::mem::size_of::<Slot<T>>() + 2 * std::mem::align_of::<Slot<T>>().max(4);
        (capacity as u64 + 1) * stride as u64
    }
    // Heap teardown deallocates before returning its shared budget reservation.
    pub fn drop_storage(&mut self) {
        drop(self.map.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn object_identity_table_matches_pinned_slotmap_allocation() {
        let mut map = None;
        let allocation = allocation_counter::measure(|| {
            map = Some(SlotMap::<HeapKey, Option<NonZeroU32>>::with_capacity_and_key(33));
        });
        assert_eq!(allocation.count_total, 1);
        assert_eq!(
            allocation.bytes_current as u64,
            (map.unwrap().capacity() as u64 + 1) * POSITION_SLOT_BYTES
        );
    }

    #[test]
    fn grouped_object_cost_matches_sequential_growth_across_pages_and_position_boundaries() {
        let heap = crate::Heap::new(crate::SessionEpoch(1), Default::default()).unwrap();
        let usage = heap.budget_usage();
        for occupied in [0, 1, 30, 31, 32, 63, 64, 127, 128, 255, 256, 511] {
            for additional in [1, 2] {
                let mut slots = ObjectSlots::<[u64; 8]>::new(1, 1024, u32::MAX);
                for _ in 0..occupied {
                    slots.reserve_one(&usage).unwrap();
                    slots.insert([0; 8]);
                }
                let predicted = slots.allocation_bytes(additional);
                let actual = allocation_counter::measure(|| {
                    for _ in 0..additional {
                        slots.reserve_one(&usage).unwrap();
                        slots.insert([0; 8]);
                    }
                });
                assert_eq!(
                    predicted, actual.bytes_total as u64,
                    "occupied={occupied}, additional={additional}"
                );
            }
        }
    }

    #[test]
    fn full_u64_keys_and_terminal_generation() {
        for raw in [
            0xffff_ffff_0000_0001,
            0x8000_0001_ffff_fffe,
            0x0000_0001_0000_0001,
        ] {
            assert_eq!(heap_key(raw).unwrap().data().as_ffi(), raw);
        }
        assert!(heap_key(0x0000_0002_0000_0001).is_none());
        let mut slots = Slots::<HeapKey, _>::new(4, 4, 3);
        let first = slots.insert(1);
        assert_eq!(generation(first), 1);
        assert_eq!(slots.remove(first), Some(1));
        let last = slots.insert(2);
        assert_eq!(index(last), index(first));
        assert_eq!(generation(last), 3);
        assert_eq!(slots.remove(last), Some(2));
        assert_eq!(slots.retired(), 1);
        assert!(slots.get(first).is_none() && slots.get(last).is_none());
        let fresh = slots.insert(3);
        assert_ne!(index(fresh), index(first));
    }
}
