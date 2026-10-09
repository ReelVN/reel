//! Version-specific slotmap 1.1.1 adaptation. Never truncate keys for storage.
use slotmap::{Key, KeyData, SlotMap, new_key_type};

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

pub(crate) struct Slots<K: Key, T> {
    map: Option<SlotMap<K, Slot<T>>>,
    limit: usize,
    retired: usize,
    retirement_version: u32,
}

impl<K: Key, T> Slots<K, T> {
    pub fn new(limit: usize, retirement_version: u32) -> Self {
        Self {
            map: Some(SlotMap::with_capacity_and_key(limit)),
            limit,
            retired: 0,
            retirement_version,
        }
    }
    pub fn full(&self) -> bool {
        self.map.as_ref().unwrap().len() >= self.limit
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
        // Conservative upper bound for private SlotUnion<V> + u32 version + padding,
        // including the sentinel. Checked constructor bounds keep products in u64.
        let stride = std::mem::size_of::<Slot<T>>() + 2 * std::mem::align_of::<Slot<T>>().max(4);
        ((self.map.as_ref().unwrap().capacity() + 1) as u64) * stride as u64
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
    fn full_u64_keys_and_terminal_generation() {
        for raw in [
            0xffff_ffff_0000_0001,
            0x8000_0001_ffff_fffe,
            0x0000_0001_0000_0001,
        ] {
            assert_eq!(heap_key(raw).unwrap().data().as_ffi(), raw);
        }
        assert!(heap_key(0x0000_0002_0000_0001).is_none());
        let mut slots = Slots::<HeapKey, _>::new(4, 3);
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
