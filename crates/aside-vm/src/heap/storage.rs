//! Prepare storage before publishing heap identities. Barriers and GC only use
//! the already reserved queues; growth preserves their order and scan cursors.
use super::*;

fn grow_queue(
    queue: &mut VecDeque<HeapKey>,
    capacity: usize,
    usage: &HeapBudgetUsage,
) -> Result<u64, HeapError> {
    let old = queue.capacity();
    if capacity <= old {
        return Ok(0);
    }
    let reservation = usage.reserve(capacity as u64 * size_of::<HeapKey>() as u64)?;
    queue
        .try_reserve_exact(capacity - queue.len())
        .map_err(|_| HeapError::AllocationFailure)?;
    let added = ((queue.capacity() - old) * size_of::<HeapKey>()) as u64;
    drop(reservation);
    usage.charge(added);
    Ok(added)
}

fn grow_index<T: Copy + Default>(
    index: &mut Vec<T>,
    len: usize,
    usage: &HeapBudgetUsage,
) -> Result<u64, HeapError> {
    let old = index.capacity();
    if len <= index.len() {
        return Ok(0);
    }
    let reservation = usage.reserve(len as u64 * size_of::<T>() as u64)?;
    index
        .try_reserve_exact(len - index.len())
        .map_err(|_| HeapError::AllocationFailure)?;
    index.resize(len, T::default());
    let added = ((index.capacity() - old) * size_of::<T>()) as u64;
    drop(reservation);
    usage.charge(added);
    Ok(added)
}

impl Heap {
    pub(crate) fn ensure_weak_slot(&mut self) -> Result<(), HeapError> {
        if self.weak_refs.len() == self.weak_refs.capacity() {
            let capacity = self.weak_refs.len().saturating_mul(2).max(4);
            self.reserved += grow_queue(&mut self.weak_refs, capacity, &self.usage)?;
        }
        Ok(())
    }
    /// Prepare slice admission using the same slot/queue growth as allocation.
    pub(crate) fn object_allocation_bytes(&self, additional: usize) -> u64 {
        let additional = additional.min(
            (self.limits.max_objects as usize)
                .saturating_sub(self.objects.occupied() + self.objects.retired()),
        );
        let mut bytes = self.objects.allocation_bytes(additional);
        for queue in [&self.allocations, &self.gray, &self.release] {
            let mut capacity = queue.capacity();
            for index in 1..=additional {
                let needed = self.objects.occupied() + index;
                if capacity < needed {
                    capacity = (capacity * 2).max(needed);
                    bytes += (capacity * size_of::<HeapKey>()) as u64;
                }
            }
        }
        bytes
    }
    pub(crate) fn remove_object(&mut self, key: HeapKey) {
        self.unlink_derived(key);
        self.changes
            .record(key, self.objects.get(key).unwrap().watched);
        self.mutation_revision = self.mutation_revision.wrapping_add(1);
        let before = self.objects.reserved_bytes();
        self.objects
            .remove(key)
            .expect("release record owns a dying slot");
        let released = before - self.objects.reserved_bytes();
        self.reserved -= released;
        self.usage.refund(released);
    }

    pub(crate) fn ensure_object_slot(&mut self) -> Result<(), HeapError> {
        if self.objects.full() {
            return Err(HeapError::ObjectCapacity);
        }
        // A reused identity can still require a fresh entity page or queue space.
        // Prepare by live demand, independently of the generation table's peak.
        let needed = self.objects.occupied() + 1;
        for queue in [&mut self.allocations, &mut self.gray, &mut self.release] {
            if queue.capacity() < needed {
                let capacity = (queue.capacity() * 2).max(needed);
                self.reserved += grow_queue(queue, capacity, &self.usage)?;
            }
        }
        let before = self.objects.reserved_bytes();
        let result = self.objects.reserve_one(&self.usage);
        self.reserved += self.objects.reserved_bytes() - before;
        result
    }

    pub(super) fn ensure_root_slot(&mut self) -> Result<(), HeapError> {
        if self.roots.full() {
            return Err(HeapError::RootCapacity);
        }
        if let Some(capacity) = self.roots.growth_capacity() {
            self.reserved += grow_index(&mut self.root_index, capacity + 1, &self.usage)?;
            self.reserved += self.roots.reserve_one(&self.usage)?;
        }
        Ok(())
    }

    pub(crate) fn ensure_range_slot(&mut self) -> Result<(), HeapError> {
        self.ensure_range_slots(1)
    }
    /// Side-table size, conservative work, and full replacement allocations.
    pub(crate) fn range_admission(&self, additional: usize) -> (usize, u64, u64) {
        let Some(capacity) = self
            .ranges
            .growth_capacity_for(additional)
            .filter(|_| self.ranges.admits(additional))
        else {
            return (self.range_index.len(), 0, 0);
        };
        let slots = capacity + 1;
        let index_bytes = if slots > self.range_index.len() {
            (slots * size_of::<Option<RangeKey>>()) as u64
        } else {
            0
        };
        let words = slots.div_ceil(64);
        let occupied_bytes = if words > self.range_occupied.len() {
            (words * size_of::<u64>()) as u64
        } else {
            0
        };
        (
            self.range_index.len().max(slots),
            2 * slots as u64 + words as u64,
            index_bytes + occupied_bytes + Slots::<RangeKey, RangeDescriptor>::bytes_for(capacity),
        )
    }
    pub(crate) fn ensure_range_slots(&mut self, additional: usize) -> Result<(), HeapError> {
        if !self.ranges.admits(additional) {
            return Err(HeapError::RootRangeCapacity);
        }
        if let Some(capacity) = self.ranges.growth_capacity_for(additional) {
            self.reserved += grow_index(&mut self.range_index, capacity + 1, &self.usage)?;
            self.reserved += grow_index(
                &mut self.range_occupied,
                (capacity + 1).div_ceil(64),
                &self.usage,
            )?;
            self.reserved += self.ranges.reserve(&self.usage, additional)?;
        }
        Ok(())
    }
}
