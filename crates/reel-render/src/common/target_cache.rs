//! Cache ownership and submission recency, independent of native target handles.
use super::{GpuError, target_plan::Requirements};
use reel_core::budget::{BudgetPool, BudgetTicket};

struct Slot<T> {
    size: [u32; 2],
    value: T,
    age: u8,
}

pub(crate) struct Cache<T> {
    slots: Vec<Slot<T>>,
    storage: Option<BudgetTicket>,
}
impl<T> Default for Cache<T> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            storage: None,
        }
    }
}
impl<T> Cache<T> {
    #[cfg(all(test, feature = "wgpu"))]
    pub(crate) fn capacity_bytes(&self) -> u64 {
        (self.slots.capacity() * size_of::<Slot<T>>()) as u64
    }
    #[cfg(feature = "wgpu")]
    pub(crate) fn contains(&self, size: [u32; 2], mut same: impl FnMut(&T) -> bool) -> bool {
        let start = self.slots.partition_point(|slot| slot.size < size);
        self.slots[start..]
            .iter()
            .take_while(|slot| slot.size == size)
            .any(|slot| same(&slot.value))
    }
    pub(crate) fn get(&self, size: [u32; 2], index: usize) -> Option<&T> {
        let start = self.slots.partition_point(|slot| slot.size < size);
        self.slots
            .get(start.checked_add(index)?)
            .filter(|slot| slot.size == size)
            .map(|slot| &slot.value)
    }

    /// Admit overlapping old/new metadata capacity before growing the vector.
    /// Successful submission keeps the actual capacity warm through idle frames.
    pub(crate) fn reserve(
        &mut self,
        additional: usize,
        pool: &BudgetPool,
        limit: u64,
    ) -> Result<(), GpuError> {
        let required = self
            .slots
            .len()
            .checked_add(additional)
            .ok_or(GpuError::InvalidSize)?;
        if required <= self.slots.capacity() {
            return Ok(());
        }
        let capacity = required.max(self.slots.capacity().saturating_mul(2));
        let bytes = |count: usize| {
            count
                .checked_mul(size_of::<Slot<T>>())
                .and_then(|bytes| u64::try_from(bytes).ok())
                .ok_or(GpuError::InvalidSize)
        };
        let peak = bytes(self.slots.capacity())?
            .checked_add(bytes(capacity)?)
            .ok_or(GpuError::InvalidSize)?;
        let denied = || GpuError::Pressure {
            domain: "target cache metadata",
            requested: peak,
            available: limit.saturating_sub(pool.usage().bytes),
        };
        if let Some(storage) = &mut self.storage {
            storage.resize(peak).map_err(|_| denied())?;
        } else {
            self.storage = Some(pool.reserve(peak).map_err(|_| denied())?);
        }
        self.slots.reserve_exact(capacity - self.slots.len());
        self.storage
            .as_mut()
            .unwrap()
            .resize(bytes(self.slots.capacity())?)
            .expect("metadata capacity was admitted before allocation");
        Ok(())
    }

    /// Cache an owner within admitted metadata capacity. A full optional cache
    /// leaves that owner with its preparation or flight rather than growing.
    pub(crate) fn insert(&mut self, size: [u32; 2], value: T) -> bool {
        if self.slots.len() == self.slots.capacity() {
            return false;
        }
        let index = self.slots.partition_point(|slot| slot.size <= size);
        self.slots.insert(
            index,
            Slot {
                size,
                value,
                age: 0,
            },
        );
        true
    }

    /// Only accepted submissions advance recency. Alternating neighboring size
    /// buckets remain warm; repeated simple frames let old complex plans retire.
    pub(crate) fn submitted(&mut self) {
        for slot in &mut self.slots {
            slot.age = slot.age.saturating_add(1);
        }
    }

    pub(crate) fn mark_used(&mut self, size: [u32; 2], mut same: impl FnMut(&T) -> bool) {
        let start = self.slots.partition_point(|slot| slot.size < size);
        if let Some(slot) = self.slots[start..]
            .iter_mut()
            .take_while(|slot| slot.size == size)
            .find(|slot| same(&slot.value))
        {
            slot.age = 0;
        }
    }

    pub(crate) fn prune_cold(&mut self, mut unique: impl FnMut(&T) -> bool) -> bool {
        let before = self.slots.len();
        self.slots
            .retain(|slot| slot.age < 2 || !unique(&slot.value));
        self.slots.len() != before
    }

    /// Required transient work can displace optional cache bookkeeping. Release
    /// its vector capacity only after all remaining cache owners are independent.
    pub(crate) fn release_idle(&mut self, mut unique: impl FnMut(&T) -> bool) {
        self.slots.retain(|slot| !unique(&slot.value));
        if self.slots.is_empty() {
            *self = Self::default();
        }
    }

    /// Remove only independently reclaimable cache owners outside the plan's
    /// physical multiplicity. Required owners and external readers stay pinned.
    pub(crate) fn reclaim(
        &mut self,
        required: &Requirements,
        recent: bool,
        mut unique: impl FnMut(&T) -> bool,
    ) -> bool {
        let before = self.slots.len();
        let mut previous = None;
        let mut index = 0;
        self.slots.retain(|slot| {
            if previous == Some(slot.size) {
                index += 1;
            } else {
                previous = Some(slot.size);
                index = 0;
            }
            required.count(slot.size) > index || !recent && slot.age < 2 || !unique(&slot.value)
        });
        self.slots.len() != before
    }
}

#[cfg(test)]
mod tests;
