//! One exclusive initialization of the three fixed argument windows.
use super::*;

impl FrameSlots {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn initialize_value_entry(
        &mut self,
        heap: &mut Heap,
        args: RootRangeToken,
        bound: RootRangeToken,
        registers: RootRangeToken,
        values: &[Value],
        prefix: usize,
        locals: u64,
        admit: impl FnOnce(),
    ) -> Result<(), HeapError> {
        let (args, bound, registers) = if self.vm_access_active() {
            self.check_heap_in::<true>(heap)?;
            (
                self.window_in::<true>(args)?,
                self.window_in::<true>(bound)?,
                self.window_in::<true>(registers)?,
            )
        } else {
            self.check_heap(heap)?;
            (
                self.window(args)?,
                self.window(bound)?,
                self.window(registers)?,
            )
        };
        if prefix > values.len()
            || prefix > 1
            || args.len as usize != 2 * (values.len() - prefix)
            || bound.len as usize != values.len()
            || (registers.len as usize) < values.len()
            || (locals != 0 && 64 - locals.leading_zeros() > registers.len)
        {
            return Err(HeapError::OutOfBounds);
        }
        // Tokens may map into a shared bitmap word; establish disjoint Value
        // ranges, then retain the single bitmap rather than splitting words.
        for (a, b) in [(args, bound), (args, registers), (bound, registers)] {
            if a.len != 0 && b.len != 0 && a.base < b.base + b.len && b.base < a.base + a.len {
                return Err(HeapError::InvalidRootRange);
            }
        }
        for index in 0..values.len() {
            if self.initialized(bound.base as usize + index) != (index < prefix) {
                return Err(HeapError::AlreadyInitialized);
            }
        }
        for &value in values {
            if value.tag() == crate::ValueTag::Object {
                heap.read_object(value)?;
            }
        }
        admit();
        // Original Copy shades explicit values before Parameter installs the
        // receiver. No collection/mutation can intervene in this transaction.
        for &value in values[prefix..].iter().chain(&values[..prefix]) {
            heap.shade(value);
        }
        for (index, &value) in values.iter().enumerate() {
            if index >= prefix {
                let argument = index - prefix;
                self.put(args.base as usize + 2 * argument, Value::int(-1), true);
                self.put(args.base as usize + 2 * argument + 1, value, true);
                self.put(bound.base as usize + index, value, true);
            }
            self.put(registers.base as usize + index, value, true);
        }
        let mut remaining = locals;
        while remaining != 0 {
            let index = remaining.trailing_zeros() as usize;
            self.put(registers.base as usize + index, Value::NULL, false);
            remaining &= remaining - 1;
        }
        #[cfg(feature = "execution-counts")]
        {
            self.execution_counts.add(
                SlotCounter::Writes,
                (3 * (values.len() - prefix) + values.len()) as u64,
            );
            self.execution_counts
                .add(SlotCounter::ClearCalls, u64::from(locals != 0));
            self.execution_counts
                .add(SlotCounter::ClearValues, u64::from(locals.count_ones()));
        }
        Ok(())
    }

    pub(crate) fn initialize_scalar_entry(
        &mut self,
        heap: &Heap,
        args: RootRangeToken,
        bound: RootRangeToken,
        registers: RootRangeToken,
        values: &[Value],
        admit: impl FnOnce(),
    ) -> Result<(), HeapError> {
        let (args, bound, registers) = if self.vm_access_active() {
            self.check_heap_in::<true>(heap)?;
            (
                self.window_in::<true>(args)?,
                self.window_in::<true>(bound)?,
                self.window_in::<true>(registers)?,
            )
        } else {
            self.check_heap(heap)?;
            (
                self.window(args)?,
                self.window(bound)?,
                self.window(registers)?,
            )
        };
        if args.len as usize != 2 * values.len()
            || bound.len as usize != values.len()
            || (registers.len as usize) < values.len()
        {
            return Err(HeapError::OutOfBounds);
        }
        debug_assert!(values.iter().all(|v| v.tag() != crate::ValueTag::Object));
        // The ordinary binder rejects an already-bound argument. Retain that
        // path if a restored/incomplete frame does not have an empty window.
        if (0..values.len()).any(|i| self.initialized(bound.base as usize + i)) {
            return Err(HeapError::AlreadyInitialized);
        }
        admit();
        for (index, &value) in values.iter().enumerate() {
            self.put(args.base as usize + 2 * index, Value::int(-1), true);
            self.put(args.base as usize + 2 * index + 1, value, true);
            self.put(bound.base as usize + index, value, true);
            self.put(registers.base as usize + index, value, true);
        }
        #[cfg(feature = "execution-counts")]
        self.execution_counts
            .add(SlotCounter::Writes, 4 * values.len() as u64);
        Ok(())
    }
}
