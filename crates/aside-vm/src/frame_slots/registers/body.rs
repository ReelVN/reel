//! A synchronous Body borrow shares one bitmap while its value slices are disjoint.
use super::*;

pub(crate) struct EnvironmentWindow<'a> {
    values: &'a [Value],
    first_bit: usize,
}

impl EnvironmentWindow<'_> {
    // Non-closure Body frames reject CaptureRead before reading their environment.
    pub(crate) fn empty() -> Self {
        Self {
            values: &[],
            first_bit: 0,
        }
    }
}

impl FrameSlots {
    pub(crate) fn body_registers_vm<'a>(
        &'a mut self,
        heap: &'a mut Heap,
        registers: RootRangeToken,
        environment: RootRangeToken,
    ) -> Result<(RegisterWindow<'a>, EnvironmentWindow<'a>), HeapError> {
        self.check_heap_in::<true>(heap)?;
        let registers = self.window_in::<true>(registers)?;
        let environment = self.window_in::<true>(environment)?;
        let start = registers.base as usize;
        let end = start + registers.len as usize;
        let env_start = environment.base as usize;
        let env_end = env_start + environment.len as usize;
        let values = &mut self.values.as_mut().unwrap().data[..];
        // Prove actual physical extents, including empty windows. Token identity
        // alone does not prove disjoint storage after pool relocation/reuse.
        let (values, env_values): (&mut [Value], &[Value]) = if start == end {
            let (empty, all) = values.split_at_mut(0);
            (empty, &all[env_start..env_end])
        } else if env_start == env_end {
            (&mut values[start..end], &[])
        } else if end <= env_start {
            let (left, right) = values.split_at_mut(env_start);
            (&mut left[start..end], &right[..environment.len as usize])
        } else if env_end <= start {
            let (left, right) = values.split_at_mut(start);
            (
                &mut right[..registers.len as usize],
                &left[env_start..env_end],
            )
        } else {
            return Err(HeapError::InvalidRootRange);
        };
        Ok((
            RegisterWindow {
                values,
                // Both windows may occupy the same 64-slot word. Retain one
                // exclusive bitmap borrow; every read tests its own source bit.
                bitmap: &mut self.bitmap.as_mut().unwrap().data,
                first_bit: start,
                heap,
                #[cfg(feature = "execution-counts")]
                counts: &self.execution_counts,
            },
            EnvironmentWindow {
                values: env_values,
                first_bit: env_start,
            },
        ))
    }
}

impl RegisterWindow<'_> {
    pub(crate) fn read_environment(
        &self,
        environment: &EnvironmentWindow<'_>,
        index: u32,
    ) -> Result<Value, HeapError> {
        #[cfg(feature = "execution-counts")]
        self.counts.add(SlotCounter::Reads, 1);
        let index = index as usize;
        let value = environment
            .values
            .get(index)
            .ok_or(HeapError::OutOfBounds)?;
        let bit = environment.first_bit + index;
        if !self.bitmap[bit / 64].contains(bit) {
            return Err(HeapError::UninitializedSlot);
        }
        Ok(*value)
    }
    pub(crate) fn peek(&self, index: u32) -> Result<Value, HeapError> {
        self.read_value(index)
    }
    pub(crate) fn count_peek_read(&self) {
        #[cfg(feature = "execution-counts")]
        self.counts.add(SlotCounter::Reads, 1);
    }
    pub(crate) fn count_peek_pair(&self) {
        #[cfg(feature = "execution-counts")]
        self.counts.add(SlotCounter::PairReads, 1);
    }
}
