//! In-place root-window growth, retaining the descriptor's full identity.
use super::*;

impl FrameSlots {
    pub(crate) fn len(&self, range: RootRangeToken) -> Result<u32, HeapError> {
        Ok(self.window(range)?.len)
    }

    /// Plan actual initialization, copying and replacement-buffer residency.
    /// Failed admission keeps the original window and its initialized prefix.
    pub(crate) fn growth_admission(
        &self,
        heap: &Heap,
        range: RootRangeToken,
        len: u32,
    ) -> Result<WindowAdmission, HeapError> {
        self.check_heap(heap)?;
        let w = self.window(range)?;
        let mut plan = WindowAdmission {
            capacity: None,
            work: 0,
            allocation_bytes: 0,
        };
        if len <= w.len {
            return Ok(plan);
        }
        if len > self.limits.max_window_len {
            return Err(HeapError::OperationLimit {
                requested: u64::from(len),
                limit: u64::from(self.limits.max_window_len),
            });
        }
        let delta = len - w.len;
        let minimum = self
            .active_values
            .checked_add(delta)
            .ok_or(HeapError::WindowCapacity)?;
        if minimum > self.limits.max_capacity {
            return Err(HeapError::WindowCapacity);
        }
        let spans = &self.free.as_ref().unwrap().data;
        let adjacent = spans
            .iter()
            .any(|span| span.base == w.base + w.len && span.len >= delta);
        plan.work = u64::from(delta) + u64::from(delta).div_ceil(64) + 4 * spans.len() as u64 + 8;
        if adjacent {
            return Ok(plan);
        }
        if spans.iter().any(|span| span.len >= len) {
            plan.work = u64::from(len)
                + u64::from(len).div_ceil(64)
                + 3 * u64::from(w.len)
                + 8 * spans.len() as u64
                + 16;
            return Ok(plan);
        }
        let old_capacity = self.values.as_ref().unwrap().data.len() as u32;
        let capacity = if minimum <= old_capacity {
            old_capacity
        } else {
            old_capacity
                .saturating_mul(2)
                .max(minimum)
                .min(self.limits.max_capacity)
        };
        let bitmap = u64::from(capacity).div_ceil(64);
        plan.capacity = Some(capacity);
        plan.work = 8 * self.mappings.as_ref().unwrap().data.len() as u64
            + u64::from(capacity)
            + bitmap
            + 5 * u64::from(self.active_values)
            + spans.len() as u64
            + 16;
        plan.allocation_bytes = u64::from(capacity) * size_of::<Value>() as u64 + bitmap * 8;
        Ok(plan)
    }

    /// Grow without another descriptor. New positions are null and uninitialized;
    /// existing values and initialization bits survive moves and incremental GC.
    pub(crate) fn grow(
        &mut self,
        heap: &mut Heap,
        range: RootRangeToken,
        len: u32,
    ) -> Result<(), HeapError> {
        let plan = self.growth_admission(heap, range, len)?;
        let w = self.window(range)?;
        if len <= w.len {
            return Ok(());
        }
        if let Some(capacity) = plan.capacity {
            self.relocate_growing(heap, capacity, Some((w.key, len)))?;
            return Ok(());
        }
        let delta = len - w.len;
        let spans = &self.free.as_ref().unwrap().data;
        let adjacent = spans
            .iter()
            .position(|span| span.base == w.base + w.len && span.len >= delta);
        let (free_index, base, consumed) = if let Some(i) = adjacent {
            (i, w.base, delta)
        } else {
            let i = spans.iter().position(|span| span.len >= len).unwrap();
            // Validate/barrier every old root before changing either mapping.
            for offset in 0..w.len {
                heap.root_insertion_barrier(
                    self.values.as_ref().unwrap().data[(w.base + offset) as usize],
                )?;
            }
            (i, spans[i].base, len)
        };
        let start = if adjacent.is_some() {
            base + w.len
        } else {
            base
        };
        self.values.as_mut().unwrap().data[start as usize..(base + len) as usize].fill(Value::NULL);
        self.clear_initialized(start as usize, (base + len - start) as usize);
        if adjacent.is_none() {
            self.values
                .as_mut()
                .unwrap()
                .data
                .copy_within(w.base as usize..(w.base + w.len) as usize, base as usize);
            for offset in 0..w.len {
                self.set_initialized(
                    (base + offset) as usize,
                    self.initialized((w.base + offset) as usize),
                );
            }
        }
        let spans = &mut self.free.as_mut().unwrap().data;
        spans[free_index].base += consumed;
        spans[free_index].len -= consumed;
        if spans[free_index].len == 0 {
            spans.remove(free_index);
        }
        self.largest_free = spans.iter().map(|span| span.len).max().unwrap_or(0);
        self.mappings.as_mut().unwrap().data[index(w.key)] = Some(Window { base, len, ..w });
        heap.ranges.get_mut(w.key).unwrap().len = len;
        self.active_values += delta;
        if adjacent.is_none() {
            self.recycle(Span {
                base: w.base,
                len: w.len,
            });
        }
        Ok(())
    }
}
