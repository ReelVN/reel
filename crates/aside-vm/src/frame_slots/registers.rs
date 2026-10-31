//! Borrowed register access validates ownership once for one synchronous block.
use super::*;

/// A current register window. Its mutable pool/heap borrow excludes relocation,
/// range release, GC and reentrant publication until the executor drops the view.
pub(crate) struct RegisterWindow<'a> {
    values: &'a mut [Value],
    bitmap: &'a mut [u64],
    first_bit: usize,
    heap: &'a mut Heap,
    #[cfg(feature = "execution-counts")]
    counts: &'a SlotState,
}

impl FrameSlots {
    /// Validate one frame's register mapping for a bounded synchronous block.
    /// The view exposes no operation that can move or release its storage.
    pub(crate) fn registers<'a>(
        &'a mut self,
        heap: &'a mut Heap,
        range: RootRangeToken,
    ) -> Result<RegisterWindow<'a>, HeapError> {
        self.check_heap(heap)?;
        let window = self.window(range)?;
        let start = window.base as usize;
        let end = start + window.len as usize;
        Ok(RegisterWindow {
            values: &mut self.values.as_mut().unwrap().data[start..end],
            bitmap: &mut self.bitmap.as_mut().unwrap().data[start / 64..end.div_ceil(64)],
            first_bit: start % 64,
            heap,
            #[cfg(feature = "execution-counts")]
            counts: &self.execution_counts,
        })
    }
}

impl RegisterWindow<'_> {
    /// Borrow the heap for pure metadata while this register mapping is pinned.
    pub(crate) fn heap(&self) -> &Heap {
        self.heap
    }

    /// Run synchronous value work without exposing the pool or its root provider.
    /// The executor ends this borrow before publishing another frame or collecting.
    pub(crate) fn with_heap<T>(&mut self, operation: impl FnOnce(&mut Heap) -> T) -> T {
        operation(self.heap)
    }

    fn read_value(&self, register: u32) -> Result<Value, HeapError> {
        let index = register as usize;
        let value = self.values.get(index).ok_or(HeapError::OutOfBounds)?;
        let bit = self.first_bit + index;
        if self.bitmap[bit / 64] & (1 << (bit % 64)) == 0 {
            return Err(HeapError::UninitializedSlot);
        }
        Ok(*value)
    }

    /// Read an initialized register, retaining its checked logical bounds.
    pub(crate) fn read(&self, register: u32) -> Result<Value, HeapError> {
        #[cfg(feature = "execution-counts")]
        self.counts.add(SlotCounter::Reads, 1);
        self.read_value(register)
    }

    /// Read two operands in source order from the same validated mapping.
    pub(crate) fn read_pair(&self, first: u32, second: u32) -> Result<(Value, Value), HeapError> {
        #[cfg(feature = "execution-counts")]
        self.counts.add(SlotCounter::PairReads, 1);
        Ok((self.read_value(first)?, self.read_value(second)?))
    }

    /// Resolve a Cell through the heap already borrowed by this window.
    pub(crate) fn cell_read(&self, value: Value) -> Result<Value, HeapError> {
        self.heap.cell_read(value)
    }

    /// Publish through the ordinary insertion barrier before setting its bitmap.
    pub(crate) fn write(&mut self, register: u32, value: Value) -> Result<(), HeapError> {
        #[cfg(feature = "execution-counts")]
        self.counts.add(SlotCounter::Writes, 1);
        self.put(register, value, true)
    }

    /// Clear operands in order; a later invalid operand retains the valid prefix.
    pub(crate) fn clear<const N: usize>(&mut self, registers: [u32; N]) -> Result<(), HeapError> {
        #[cfg(feature = "execution-counts")]
        {
            self.counts.add(SlotCounter::ClearCalls, 1);
            self.counts.add(SlotCounter::ClearValues, N as u64);
        }
        for register in registers {
            self.put(register, Value::NULL, false)?;
        }
        Ok(())
    }

    fn put(&mut self, register: u32, value: Value, initialized: bool) -> Result<(), HeapError> {
        let index = register as usize;
        let slot = self.values.get_mut(index).ok_or(HeapError::OutOfBounds)?;
        self.heap.root_insertion_barrier(value)?;
        *slot = value;
        let bit = self.first_bit + index;
        let word = &mut self.bitmap[bit / 64];
        let mask = 1 << (bit % 64);
        if initialized {
            *word |= mask;
        } else {
            *word &= !mask;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
