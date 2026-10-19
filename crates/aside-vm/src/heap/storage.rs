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

fn grow_index<K: Copy>(
    index: &mut Vec<Option<K>>,
    len: usize,
    usage: &HeapBudgetUsage,
) -> Result<u64, HeapError> {
    let old = index.capacity();
    if len <= index.len() {
        return Ok(0);
    }
    let reservation = usage.reserve(len as u64 * size_of::<Option<K>>() as u64)?;
    index
        .try_reserve_exact(len - index.len())
        .map_err(|_| HeapError::AllocationFailure)?;
    index.resize(len, None);
    let added = ((index.capacity() - old) * size_of::<Option<K>>()) as u64;
    drop(reservation);
    usage.charge(added);
    Ok(added)
}

impl Heap {
    pub(crate) fn remove_object(&mut self, key: HeapKey) {
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
        for queue in [
            &mut self.allocations,
            &mut self.gray,
            &mut self.release,
            &mut self.reactive_queue,
        ] {
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
        if self.ranges.full() {
            return Err(HeapError::RootRangeCapacity);
        }
        if let Some(capacity) = self.ranges.growth_capacity() {
            self.reserved += grow_index(&mut self.range_index, capacity + 1, &self.usage)?;
            self.reserved += self.ranges.reserve_one(&self.usage)?;
        }
        Ok(())
    }
}
