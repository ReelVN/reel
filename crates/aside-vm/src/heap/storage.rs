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
    pub(crate) fn ensure_object_slot(&mut self) -> Result<(), HeapError> {
        if self.objects.full() {
            return Err(HeapError::ObjectCapacity);
        }
        if let Some(capacity) = self.objects.growth_capacity() {
            // Grow the object table last. A failed allocation can retain charged
            // queue capacity, but cannot admit an object without all its queues.
            for queue in [
                &mut self.allocations,
                &mut self.gray,
                &mut self.release,
                &mut self.reactive_queue,
            ] {
                self.reserved += grow_queue(queue, capacity, &self.usage)?;
            }
            self.reserved += self.objects.reserve_one(&self.usage)?;
        }
        Ok(())
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
