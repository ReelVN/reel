//! Native root storage follows actual responses, retaining the v1 slot layout.
use super::*;
#[cfg(test)]
mod tests;

impl Vm {
    pub(super) fn clear_native_pending(&mut self, roots: RootRangeToken) -> Result<(), VmError> {
        let slot = self.native_pending_slot();
        if slot < self.slots.len(roots)? {
            self.clear(roots, slot)?;
        }
        Ok(())
    }

    fn continuation_root_len(&mut self, continuation: Continuation<'_>) -> Result<u32, VmError> {
        if continuation.values.len() > self.limits.max_continuation_values as usize {
            return Err(VmError::Limit);
        }
        let mut len = 3.max(2 + continuation.values.len() as u32);
        if let FaultRetention::Replace(ids) = continuation.faults {
            self.validate_native_faults(ids)?;
            if !ids.is_empty() {
                len = len.max(self.native_fault_base() + ids.len() as u32);
            }
        }
        for &value in continuation.values {
            self.heap.root_insertion_barrier(value)?;
        }
        Ok(len)
    }

    /// Validate borrowed inputs and admit their entire window before changing
    /// continuation contents or consuming the issued ticket. Growth preserves
    /// the root descriptor identity and can relocate the pool at this safe point.
    pub(super) fn prepare_native_response(
        &mut self,
        native: &mut NativeFrame,
        ticket: HostTicket,
        response: &HostResponse<'_>,
    ) -> Result<(), VmError> {
        let len = match *response {
            HostResponse::Ready(value) => {
                self.heap.root_insertion_barrier(value)?;
                3
            }
            HostResponse::Pending(wait) => {
                if wait != ticket.wait_id() {
                    return Err(VmError::InvalidWait);
                }
                3
            }
            HostResponse::Error(new) => {
                self.check_new_fault(new)?;
                self.heap.root_insertion_barrier(new.value)?;
                if let Some(id) = new.cause {
                    self.heap.root_insertion_barrier(id.as_value())?;
                    self.native_pending_slot() + 1
                } else {
                    3
                }
            }
            HostResponse::ForwardFault(id) => {
                if !self.native_holds(*native, id)? {
                    return Err(VmError::InvalidFault);
                }
                self.native_pending_slot() + 1
            }
            HostResponse::Continue(continuation) => self.continuation_root_len(continuation)?,
            HostResponse::CallScript {
                closure,
                arguments,
                continuation,
            } => {
                if arguments.len() > self.limits.max_arguments as usize {
                    return Err(VmError::Limit);
                }
                self.closure_function(closure)?;
                self.heap.root_insertion_barrier(closure)?;
                for &value in arguments {
                    self.heap.root_insertion_barrier(value)?;
                }
                let len = self.continuation_root_len(continuation)?;
                if arguments.is_empty() {
                    len
                } else {
                    len.max(2 + self.limits.max_continuation_values + arguments.len() as u32)
                }
            }
        };
        self.slots.grow(&mut self.heap, native.roots, len)?;
        if native.inline_argument && matches!(response, HostResponse::CallScript { .. }) {
            // Root 1 will hold the new callback. Preserve the original supplied
            // argument in ordinary windows before that root is overwritten.
            let value = self.value(native.roots, 1)?;
            self.slots.grow(&mut self.heap, native.args, 2)?;
            self.slots.grow(&mut self.heap, native.bound, 1)?;
            self.put(native.args, 0, Value::int(0))?;
            self.put(native.args, 1, value)?;
            self.put(native.bound, 0, value)?;
            native.inline_argument = false;
        }
        Ok(())
    }

    pub(super) fn native_root_admission(
        &mut self,
        roots: RootRangeToken,
        len: u32,
    ) -> Result<Option<crate::frame_slots::WindowAdmission>, VmError> {
        match self.slots.growth_admission(&self.heap, roots, len) {
            Ok(plan) => Ok(Some(plan)),
            Err(error) => {
                self.block_heap(error)?;
                Ok(None)
            }
        }
    }

    fn commit_root_growth(
        &self,
        ret: ReturnTo,
        error: Option<FaultSeed>,
    ) -> Result<Option<(RootRangeToken, u32)>, VmError> {
        let Some(error) = error else {
            return Ok(None);
        };
        let (frame, retained) = match ret {
            ReturnTo::Native { frame } => (frame, true),
            ReturnTo::NativeDefault { frame, .. } => (frame, false),
            _ => return Ok(None),
        };
        let Frame::Native(native) = self.active.frames.data[self.index(frame)?] else {
            return Err(VmError::Contract("native return target"));
        };
        let mut len = self.native_pending_slot() + 1;
        if retained && let Some(id) = error.fault {
            let count = native.fault_count + u32::from(!self.native_fault_slot_holds(native, id)?);
            len = len.max(self.native_fault_base() + count.min(self.limits.max_native_faults));
        }
        Ok(Some((native.roots, len)))
    }

    pub(super) fn commit_root_admission(
        &mut self,
        ret: ReturnTo,
        error: Option<FaultSeed>,
    ) -> Result<Option<(u64, u64)>, VmError> {
        let Some((roots, len)) = self.commit_root_growth(ret, error)? else {
            return Ok(Some((0, 0)));
        };
        Ok(self
            .native_root_admission(roots, len)?
            .map(|p| (p.work, p.allocation_bytes)))
    }

    pub(super) fn grow_commit_roots(
        &mut self,
        ret: ReturnTo,
        error: Option<FaultSeed>,
    ) -> Result<bool, VmError> {
        if let Some((roots, len)) = self.commit_root_growth(ret, error)?
            && let Err(error) = self.slots.grow(&mut self.heap, roots, len)
        {
            self.block_heap(error)?;
            return Ok(false);
        }
        Ok(true)
    }
}
