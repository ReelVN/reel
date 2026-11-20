//! Borrowed register access validates ownership once for one synchronous block.
use super::*;
mod body;
pub(crate) use body::EnvironmentWindow;

/// A current register window. Its mutable pool/heap borrow excludes relocation,
/// range release, GC and reentrant publication until the executor drops the view.
pub(crate) struct RegisterWindow<'a> {
    values: &'a mut [Value],
    bitmap: &'a mut [bitmap::Word],
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
        self.registers_in::<false>(heap, range)
    }
    pub(crate) fn registers_vm<'a>(
        &'a mut self,
        heap: &'a mut Heap,
        range: RootRangeToken,
    ) -> Result<RegisterWindow<'a>, HeapError> {
        if !self.vm_access_active() {
            return self.registers(heap, range);
        }
        self.registers_in::<true>(heap, range)
    }
    fn registers_in<'a, const VM: bool>(
        &'a mut self,
        heap: &'a mut Heap,
        range: RootRangeToken,
    ) -> Result<RegisterWindow<'a>, HeapError> {
        self.check_heap_in::<VM>(heap)?;
        let window = self.window_in::<VM>(range)?;
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
    pub(crate) fn is_initialized(&self, register: u32) -> bool {
        let index = register as usize;
        let bit = self.first_bit + index;
        index < self.values.len() && self.bitmap[bit / 64].contains(bit)
    }

    fn read_value(&self, register: u32) -> Result<Value, HeapError> {
        let index = register as usize;
        let value = self.values.get(index).ok_or(HeapError::OutOfBounds)?;
        let bit = self.first_bit + index;
        if !self.bitmap[bit / 64].contains(bit) {
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

    pub(crate) fn heap_read(&self) -> &Heap {
        self.heap
    }
    pub(crate) fn cell_initialize(&mut self, cell: Value, value: Value) -> Result<(), HeapError> {
        self.heap.cell_initialize(cell, value)
    }
    pub(crate) fn cell_write(&mut self, cell: Value, value: Value) -> Result<(), HeapError> {
        self.heap.cell_write(cell, value)
    }
    pub(crate) fn observed_cell_value(&mut self, cell: Value) -> Result<Option<Value>, HeapError> {
        self.heap.observed_cell_value(cell)
    }

    /// Publish through the ordinary insertion barrier before setting its bitmap.
    pub(crate) fn write(&mut self, register: u32, value: Value) -> Result<(), HeapError> {
        #[cfg(feature = "execution-counts")]
        self.counts.add(SlotCounter::Writes, 1);
        self.put(register, value, true)
    }

    /// Publish a span of immediate values, which carry no GC edges.
    pub(crate) fn write_immediates(
        &mut self,
        start: u32,
        values: &[Value],
    ) -> Result<(), HeapError> {
        debug_assert!(
            values
                .iter()
                .all(|value| value.tag() != crate::ValueTag::Object)
        );
        let start = start as usize;
        let end = start
            .checked_add(values.len())
            .ok_or(HeapError::OutOfBounds)?;
        self.values
            .get_mut(start..end)
            .ok_or(HeapError::OutOfBounds)?
            .copy_from_slice(values);
        for bit in self.first_bit + start..self.first_bit + end {
            self.bitmap[bit / 64].set(bit, true);
        }
        #[cfg(feature = "execution-counts")]
        self.counts.add(SlotCounter::Writes, values.len() as u64);
        Ok(())
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

    /// Admit once after the original producer's fallible insertion barrier. The
    /// caller has proved there is no clock/GC boundary inside this lifetime tail.
    #[cfg(any(test, feature = "vm-validation-oracle"))]
    pub(crate) fn write_suffix(
        &mut self,
        destination: u32,
        value: Value,
        clears: &[u32],
        dead: bool,
        admit: impl FnOnce(),
    ) -> Result<(), HeapError> {
        if destination as usize >= self.values.len()
            || clears.iter().any(|&r| r as usize >= self.values.len())
        {
            return Err(HeapError::OutOfBounds);
        }
        self.heap.root_insertion_barrier(value)?;
        admit();
        if !dead {
            #[cfg(feature = "execution-counts")]
            self.counts.add(SlotCounter::Writes, 1);
            self.values[destination as usize] = value;
            let bit = self.first_bit + destination as usize;
            self.bitmap[bit / 64].set(bit, true);
        }
        #[cfg(feature = "execution-counts")]
        {
            self.counts.add(SlotCounter::ClearCalls, 1);
            self.counts
                .add(SlotCounter::ClearValues, clears.len() as u64);
        }
        for &register in clears {
            if !dead && register == destination {
                continue;
            }
            self.values[register as usize] = Value::NULL;
            let bit = self.first_bit + register as usize;
            self.bitmap[bit / 64].set(bit, false);
        }
        Ok(())
    }

    fn put(&mut self, register: u32, value: Value, initialized: bool) -> Result<(), HeapError> {
        let index = register as usize;
        let slot = self.values.get_mut(index).ok_or(HeapError::OutOfBounds)?;
        self.heap.root_insertion_barrier(value)?;
        *slot = value;
        let bit = self.first_bit + index;
        self.bitmap[bit / 64].set(bit, initialized);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
