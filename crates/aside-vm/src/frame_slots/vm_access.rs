//! Executor-only root access while one Vm exclusively owns its Heap/provider.
//! Public FrameSlots entry points keep their complete validation. A VM scope
//! carries no physical address; mapping generations, bounds and bits stay live.
use super::*;
use crate::root_ranges::VmAccessGuard;
use std::rc::Rc;

impl FrameSlots {
    pub(crate) fn begin_vm_access(&self, heap: &Heap) -> Option<VmAccessGuard> {
        #[cfg(any(test, feature = "vm-validation-oracle"))]
        if self.checked_vm_access {
            return None;
        }
        self.check_heap(heap).ok()?;
        if self.vm_access_active() {
            return None;
        }
        Some(VmAccessGuard::enter(&self.usage, self.attachment))
    }

    #[inline]
    pub(super) fn vm_access_active(&self) -> bool {
        self.usage.0.provider_state.get() == RootProviderState::Executing
            && self.usage.0.attachment.get() == Some(self.attachment)
    }

    pub(super) fn check_heap_in<const VM: bool>(&self, heap: &Heap) -> Result<(), HeapError> {
        if VM && self.vm_access_active() && Rc::ptr_eq(&self.usage.0, &heap.usage.0) {
            #[cfg(test)]
            self.vm_access_stats
                .heap_proofs
                .set(self.vm_access_stats.heap_proofs.get() + 1);
            return Ok(());
        }
        self.check_heap(heap)
    }

    pub(super) fn window_in<const VM: bool>(
        &self,
        token: RootRangeToken,
    ) -> Result<Window, HeapError> {
        if VM && self.vm_access_active() {
            #[cfg(test)]
            self.vm_access_stats
                .window_proofs
                .set(self.vm_access_stats.window_proofs.get() + 1);
            // All executor tokens are published by this Vm's pool or rebased at
            // restore. The exclusive run scope excludes external replacement.
            debug_assert_eq!(token.heap, self.heap);
            debug_assert_eq!(token.epoch, self.epoch);
            return self.window_mapping(token);
        }
        self.window(token)
    }

    pub(super) fn position_in<const VM: bool>(
        &self,
        at: RangePosition,
    ) -> Result<usize, HeapError> {
        let window = self.window_in::<VM>(at.range)?;
        if at.index >= window.len {
            return Err(HeapError::OutOfBounds);
        }
        Ok((window.base + at.index) as usize)
    }

    // These entry points accept only executor-owned tokens. Outside run their
    // scope is absent and they take the same checked path as public operations.
    pub(crate) fn read_vm(&self, at: RangePosition) -> Result<Value, HeapError> {
        self.read_in::<true>(at)
    }
    pub(crate) fn read_pair_vm(
        &self,
        range: RootRangeToken,
        a: u32,
        b: u32,
    ) -> Result<(Value, Value), HeapError> {
        if !self.vm_access_active() {
            return self.read_pair(range, a, b);
        }
        self.read_pair_in::<true>(range, a, b)
    }
    pub(crate) fn write_vm(
        &mut self,
        heap: &mut Heap,
        at: RangePosition,
        value: Value,
    ) -> Result<(), HeapError> {
        self.write_in::<true>(heap, at, value)
    }
    pub(crate) fn clear_vm(&mut self, heap: &mut Heap, at: RangePosition) -> Result<(), HeapError> {
        self.clear_in::<true>(heap, at)
    }
    pub(crate) fn clear_registers_vm<const N: usize>(
        &mut self,
        heap: &mut Heap,
        range: RootRangeToken,
        registers: [u32; N],
    ) -> Result<(), HeapError> {
        if !self.vm_access_active() {
            return self.clear_registers(heap, range, registers);
        }
        self.clear_registers_in::<true, N>(heap, range, registers)
    }
    pub(crate) fn open_vm(
        &mut self,
        heap: &mut Heap,
        owner: RootOwner,
        kind: RootKind,
        len: u32,
    ) -> Result<RootRangeToken, HeapError> {
        self.open_with_limit::<true>(heap, owner, kind, len, self.limits.max_window_len)
    }
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct AccessStats {
    pub(crate) provider_checks: std::cell::Cell<u64>,
    pub(crate) heap_proofs: std::cell::Cell<u64>,
    pub(crate) window_proofs: std::cell::Cell<u64>,
}
#[cfg(test)]
mod tests;

#[cfg(any(test, feature = "vm-validation-oracle"))]
pub(super) fn checked_oracle() -> bool {
    std::env::var_os("ASIDE_VM_CHECKED_ACCESS").is_some_and(|value| value == "1")
}
