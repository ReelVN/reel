//! Cache ownership and submission recency, independent of native target handles.
use super::{GpuError, target_plan::Requirements};
use reel_core::budget::{BudgetPool, BudgetTicket};

/// Shared idle threshold for cold optional target retention in accepted submissions.
pub(crate) const MAX_IDLE_SUBMISSIONS: u8 = 32;

struct Slot<T> {
    size: [u32; 2],
    value: T,
    age: u8,
    uses: u8,
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
    /// Describe compatible scratch owners without exposing platform handles.
    pub(crate) fn capacities(
        &self,
        mut eligible: impl FnMut(&T) -> bool,
        reserve: impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<Requirements, GpuError> {
        Requirements::collect(
            self.slots
                .iter()
                .map(|slot| (slot.size, u64::from(eligible(&slot.value)))),
            reserve,
        )
    }
    /// Visit cached owners without changing their recency or ownership.
    #[cfg(all(feature = "profiling", feature = "gxm", target_os = "vita"))]
    pub(crate) fn observe(&self, mut visit: impl FnMut(&T, u8, u8)) {
        for slot in &self.slots {
            visit(&slot.value, slot.age, slot.uses);
        }
    }
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
    #[cfg(test)]
    pub(crate) fn get(&self, size: [u32; 2], index: usize) -> Option<&T> {
        let start = self.slots.partition_point(|slot| slot.size < size);
        self.slots
            .get(start.checked_add(index)?)
            .filter(|slot| slot.size == size)
            .map(|slot| &slot.value)
    }

    /// Select a scratch owner while excluding allocations retained as effect results.
    pub(crate) fn get_eligible(
        &self,
        size: [u32; 2],
        index: usize,
        mut eligible: impl FnMut(&T) -> bool,
    ) -> Option<&T> {
        let start = self.slots.partition_point(|slot| slot.size < size);
        self.slots[start..]
            .iter()
            .take_while(|slot| slot.size == size)
            .filter(|slot| eligible(&slot.value))
            .nth(index)
            .map(|slot| &slot.value)
    }

    /// Replace one compatible owner's immutable view and its sorted size key.
    /// Failed view construction leaves the old owner, key and recency intact;
    /// successful replacement keeps the existing metadata capacity and usage.
    #[cfg(any(feature = "gxm", test))]
    pub(crate) fn map_eligible(
        &mut self,
        size: [u32; 2],
        mut eligible: impl FnMut(&T) -> bool,
        map: impl FnOnce(&T) -> Result<T, GpuError>,
    ) -> Result<Option<T>, GpuError>
    where
        T: Clone,
    {
        let Some(index) = self.slots.iter().position(|slot| eligible(&slot.value)) else {
            return Ok(None);
        };
        let value = map(&self.slots[index].value)?;
        let mut slot = self.slots.remove(index);
        slot.size = size;
        slot.value = value.clone();
        let index = self.slots.partition_point(|slot| slot.size <= size);
        self.slots.insert(index, slot);
        Ok(Some(value))
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
                uses: 0,
            },
        );
        true
    }

    /// Only accepted submissions advance recency and reuse frequency.
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
            slot.uses = slot.uses.saturating_add(1);
        }
    }

    pub(crate) fn prune_cold(&mut self, mut unique: impl FnMut(&T) -> bool) -> bool {
        let before = self.slots.len();
        self.slots.retain(|slot| {
            let retention = 2 + slot
                .uses
                .saturating_sub(1)
                .saturating_mul(2)
                .min(MAX_IDLE_SUBMISSIONS - 2);
            slot.age < retention || !unique(&slot.value)
        });
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

    /// Reclaim one independent owner outside the plan's physical multiplicity.
    /// Oldest owners go first; equal-age candidates prefer larger storage. The
    /// caller retries admission before discarding another reusable target.
    #[cfg(test)]
    pub(crate) fn reclaim(
        &mut self,
        required: &Requirements,
        recent: bool,
        unique: impl FnMut(&T) -> bool,
    ) -> bool {
        self.reclaim_eligible(required, recent, |_| true, unique)
    }

    /// Retained effect results are outside scratch requirements and must not
    /// consume the protected scratch multiplicity for a size class.
    #[cfg(any(feature = "wgpu", test))]
    pub(crate) fn reclaim_eligible(
        &mut self,
        required: &Requirements,
        recent: bool,
        eligible: impl FnMut(&T) -> bool,
        unique: impl FnMut(&T) -> bool,
    ) -> bool {
        self.reclaim_eligible_observed(required, recent, eligible, unique, |_, _, _| {})
    }

    /// Report the selected removal before dropping it. Observers cannot change
    /// candidate selection, protected multiplicity, recency or owner lifetime.
    pub(crate) fn reclaim_eligible_observed(
        &mut self,
        required: &Requirements,
        recent: bool,
        mut eligible: impl FnMut(&T) -> bool,
        mut unique: impl FnMut(&T) -> bool,
        mut removed: impl FnMut(&T, u8, u8),
    ) -> bool {
        let mut previous = None;
        let mut index = 0;
        let mut candidate = None;
        for (position, slot) in self.slots.iter().enumerate() {
            if !eligible(&slot.value) {
                continue;
            }
            if previous == Some(slot.size) {
                index += 1;
            } else {
                previous = Some(slot.size);
                index = 0;
            }
            if required.count(slot.size) > index || !recent && slot.age < 2 || !unique(&slot.value)
            {
                continue;
            }
            let priority = (slot.age, u64::from(slot.size[0]) * u64::from(slot.size[1]));
            if candidate.is_none_or(|(_, old)| priority > old) {
                candidate = Some((position, priority));
            }
        }
        if let Some((position, _)) = candidate {
            let slot = &self.slots[position];
            removed(&slot.value, slot.age, slot.uses);
            self.slots.remove(position);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests;
