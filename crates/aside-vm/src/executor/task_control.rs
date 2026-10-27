//! Task completion, cancellation and scheduling ownership.
use super::*;

impl Vm {
    pub fn complete(&mut self, wait: WaitId, result: Completion) -> Result<(), VmError> {
        self.select_task(wait.task)
            .map_err(|_| VmError::InvalidWait)?;
        if self.active.cancelling {
            return Err(VmError::InvalidWait);
        }
        let Some(Frame::Native(mut n)) = self.active.frames.data.last().copied() else {
            return Err(VmError::InvalidWait);
        };
        if !matches!(n.phase,NativePhase::Waiting(w) if w==wait) {
            return Err(VmError::InvalidWait);
        }
        match result {
            Completion::Ready(v) => {
                self.put(n.roots, 0, v)?;
                n.pending_fault = None;
                n.phase = if n.continuation_active {
                    NativePhase::Resume(NativeInput::WaitingReturned)
                } else {
                    NativePhase::Commit(false)
                };
            }
            Completion::Error(new) => {
                self.check_new_fault(new)?;
                self.put(n.roots, 0, new.value)?;
                self.put(
                    n.roots,
                    self.native_pending_slot(),
                    new.cause.map_or(Value::NULL, FaultId::as_value),
                )?;
                n.pending_fault = Some(FaultSeed {
                    kind: new.code.kind,
                    location: n.location,
                    fault: None,
                    explicit: new.cause,
                    author_value: true,
                    awaited: None,
                });
                n.phase = if n.continuation_active {
                    NativePhase::WaitingFaultBuild
                } else {
                    NativePhase::Commit(true)
                };
            }
            Completion::ForwardFault(id) => {
                if !self.native_holds(n, id)? {
                    return Err(VmError::InvalidFault);
                };
                self.put(n.roots, 0, self.heap.fault_value(id)?)?;
                self.put(n.roots, self.native_pending_slot(), id.as_value())?;
                // A continuation can inspect/recover this producer fault first.
                // Only wrap when the result actually crosses back through await.
                n.pending_fault = Some(self.forwarded_fault(n, id, !n.continuation_active)?);
                n.phase = if n.continuation_active {
                    NativePhase::WaitingFaultBuild
                } else {
                    NativePhase::Commit(true)
                };
            }
        }
        *self.active.frames.data.last_mut().unwrap() = Frame::Native(n);
        self.wait_revision = self.wait_revision.wrapping_add(1);
        Ok(())
    }
    pub fn delivery_value(&self, id: DeliveryId) -> Result<Value, VmError> {
        self.delivery_check(id)?;
        self.value(self.task(id.task)?.delivery.unwrap(), 0)
    }
    pub fn delivery_cause(&self, id: DeliveryId) -> Result<Option<Value>, VmError> {
        let Some(info) = self.delivery_fault(id)? else {
            return Ok(None);
        };
        Ok(self
            .heap
            .fault_causes(info.id)?
            .next(&self.heap)?
            .map(|c| c.value))
    }
    pub fn delivery_fault(&self, id: DeliveryId) -> Result<Option<FaultInfo>, VmError> {
        let t = self.delivery_check(id)?;
        t.fault
            .and_then(|s| s.fault)
            .map(|f| self.heap.fault_info(f).map_err(VmError::from))
            .transpose()
    }
    pub fn take_fault_delivery(
        &mut self,
        id: DeliveryId,
        owner: RootOwner,
    ) -> Result<FaultRoot, VmError> {
        let info = self.delivery_fault(id)?.ok_or(VmError::InvalidDelivery)?;
        let root = self.heap.retain_fault(owner, info.id)?;
        self.discard_delivery(id)?;
        Ok(root)
    }
    pub fn copy_delivery_value_to_root(
        &mut self,
        id: DeliveryId,
        target: RootToken,
    ) -> Result<(), VmError> {
        let value = self.delivery_value(id)?;
        self.heap.set_root(target, value)?;
        Ok(())
    }
    pub(super) fn delivery_check(&self, id: DeliveryId) -> Result<Terminal, VmError> {
        self.task(id.task)
            .map_err(|_| VmError::InvalidDelivery)?
            .terminal
            .filter(|t| t.id == id)
            .ok_or(VmError::InvalidDelivery)
    }
    pub fn take_delivery_root(&mut self, id: DeliveryId, target: RootToken) -> Result<(), VmError> {
        if self.delivery_check(id)?.fault.is_some() {
            return Err(VmError::Contract(
                "use take_fault_delivery or copy_delivery_value_to_root",
            ));
        }
        let v = self.delivery_value(id)?;
        self.heap.set_root(target, v)?;
        self.discard_delivery(id)
    }
    pub fn discard_delivery(&mut self, id: DeliveryId) -> Result<(), VmError> {
        self.delivery_check(id)?;
        self.select_task(id.task)?;
        self.clear(self.active.delivery.unwrap(), 0)?;
        self.clear(self.active.delivery.unwrap(), 1)?;
        self.active.terminal = None;
        self.active.task_owner = None;
        self.forget_active_task();
        Ok(())
    }
    /// Invalidate this task's tickets immediately and schedule budgeted frame cleanup.
    /// Independently retained host roots remain the host's responsibility.
    pub fn cancel_task(&mut self, task: TaskId) -> Result<(), VmError> {
        self.select_task(task)?;
        self.cancel_active()
    }
    /// Revoke a native callback's inner frames while retaining its continuation.
    /// Tickets and waits become invalid immediately; slices reclaim the frames
    /// before returning `CallbackCancelled` to the named native frame.
    pub fn cancel_host_callback(&mut self, task: TaskId, frame: FrameId) -> Result<bool, VmError> {
        self.retire_host_callback(task, frame, None)
    }
    /// Finish a native callback region with `value`, skipping its inner handlers.
    /// Tickets and waits become invalid immediately; slices reclaim inner frames
    /// before resuming the native continuation with `CallbackReturned`. The value
    /// stays in that frame's existing resume root through GC and snapshots.
    /// Return false when the frame has ended or an earlier retirement request
    /// already covers it. An outer request supersedes an inner request.
    pub fn finish_host_callback(
        &mut self,
        task: TaskId,
        frame: FrameId,
        value: Value,
    ) -> Result<bool, VmError> {
        self.retire_host_callback(task, frame, Some(value))
    }
    pub(super) fn retire_host_callback(
        &mut self,
        task: TaskId,
        frame: FrameId,
        value: Option<Value>,
    ) -> Result<bool, VmError> {
        self.select_task(task)?;
        if self.active.cancelling
            && self
                .active
                .cancel_boundary
                .is_none_or(|boundary| boundary.frame().0 <= frame.0)
        {
            return Ok(false);
        }
        let Some(native) = self
            .active
            .frames
            .data
            .iter()
            .find_map(|candidate| match candidate {
                Frame::Native(n) if n.id == frame && n.continuation_active => Some(*n),
                _ => None,
            })
        else {
            return Ok(false);
        };
        if let Some(value) = value {
            self.put(native.roots, 0, value)?;
        }
        self.cancel_active()?;
        self.active.cancel_boundary = Some(if value.is_some() {
            CallbackBoundary::Returned(frame)
        } else {
            CallbackBoundary::Cancelled(frame)
        });
        Ok(true)
    }
    pub(super) fn cancel_active(&mut self) -> Result<(), VmError> {
        self.active.cancel_boundary = None;
        if self.active.cancelling {
            return Ok(());
        }
        self.active.cancelling = true;
        self.wait_revision = self.wait_revision.wrapping_add(1);
        self.active.fault_build = None;
        self.active.blocked = None;
        self.active.cooperative_cancel = false;
        self.active.abort_equality = false;
        if self.active.terminal.take().is_some() || self.active.task_id.is_some() {
            self.clear(self.active.delivery.unwrap(), 0)?;
            self.clear(self.active.delivery.unwrap(), 1)?;
        }
        Ok(())
    }
    /// Hard-cancel every matching task; subsequent slices reclaim task-owned state.
    /// Idle means no matching task, not that the owner's resources are retired.
    /// The host must reject new work for a revoked owner and separately release
    /// its host roots (individually or with Heap::release_owner_step) and resources.
    pub fn cancel(&mut self, owner: RootOwner) -> Result<(), VmError> {
        self.restore_active();
        let mut found = false;
        for slot in 0..self.tasks.data.len() {
            if self.tasks.data[slot].task_owner == Some(owner) {
                self.select_slot(slot);
                self.cancel_active()?;
                self.restore_active();
                found = true;
            }
        }
        if found { Ok(()) } else { Err(VmError::Idle) }
    }
    pub(super) fn restore_active(&mut self) {
        if let Some(slot) = self.active_slot.take() {
            std::mem::swap(&mut self.active, &mut self.tasks.data[slot]);
        }
    }
    pub(super) fn select_slot(&mut self, slot: usize) {
        self.restore_active();
        std::mem::swap(&mut self.active, &mut self.tasks.data[slot]);
        self.active_slot = Some(slot);
    }
    pub(super) fn task(&self, id: TaskId) -> Result<&TaskState, VmError> {
        if self.active.task_id == Some(id) {
            return Ok(&self.active);
        }
        Ok(&self.tasks.data[self.task_slot(id)?])
    }
    pub(super) fn select_task(&mut self, id: TaskId) -> Result<(), VmError> {
        if self.active.task_id == Some(id) {
            return Ok(());
        }
        let slot = self.task_slot(id)?;
        self.select_slot(slot);
        Ok(())
    }
    fn task_slot(&self, id: TaskId) -> Result<usize, VmError> {
        if id.heap != self.heap.id() || id.epoch != self.heap.epoch() {
            return Err(VmError::UnknownTask);
        }
        let index = self
            .task_index
            .data
            .binary_search_by_key(&id.serial, |&(serial, _)| serial)
            .map_err(|_| VmError::UnknownTask)?;
        Ok(self.task_index.data[index].1)
    }
    pub(super) fn forget_active_task(&mut self) {
        if let Some(id) = self.active.task_id.take() {
            let index = self
                .task_index
                .data
                .binary_search_by_key(&id.serial, |&(serial, _)| serial)
                .expect("live task identity is indexed");
            self.task_index.data.remove(index);
        }
    }
    pub fn pooled_root_ranges(&self) -> u32 {
        2 + 2 * self.tasks.data.len() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vm(limits: VmLimits) -> Vm {
        Vm::load(
            verify(fixtures::helper_native(), Limits::default()).unwrap(),
            super::super::review_tests::echo_registry(),
            limits,
            SessionEpoch(17),
        )
        .unwrap()
    }

    #[test]
    fn identity_index_tracks_selection_cancellation_and_reused_slots() {
        let mut vm = vm(VmLimits::default());
        let mut tasks = Vec::new();
        for _ in 0..32 {
            tasks.push(
                vm.start(FunctionId(0), &[], RootOwner(1), Context::Ordinary)
                    .unwrap(),
            );
        }
        let slots = vm.tasks.data.len();
        let capacity = vm.task_index.data.capacity();
        assert_eq!(vm.task_index.data.len(), 32);
        assert_eq!(
            vm.task_index.bytes(),
            (capacity * std::mem::size_of::<(u64, usize)>()) as u64
        );
        for &task in tasks.iter().rev() {
            vm.select_task(task).unwrap();
            assert_eq!(vm.task(task).unwrap().task_id, Some(task));
        }
        for &task in tasks.iter().step_by(2) {
            vm.cancel_task(task).unwrap();
            assert!(matches!(
                vm.run_slice(task, SliceBudget::default(), || 0),
                Err(VmError::Idle)
            ));
            assert!(matches!(vm.task(task), Err(VmError::UnknownTask)));
            let replacement = vm
                .start(FunctionId(0), &[], RootOwner(1), Context::Ordinary)
                .unwrap();
            assert_eq!(vm.task(replacement).unwrap().task_id, Some(replacement));
            assert!(matches!(vm.select_task(task), Err(VmError::UnknownTask)));
        }
        assert_eq!(vm.tasks.data.len(), slots);
        assert_eq!(vm.task_index.data.capacity(), capacity);
        assert_eq!(vm.task_index.data.len(), 32);
    }

    #[test]
    fn failed_start_releases_its_index_entry() {
        let mut vm = vm(VmLimits::default());
        vm.limits.max_frames = 0;
        assert!(
            vm.start(FunctionId(0), &[], RootOwner(1), Context::Ordinary)
                .is_err()
        );
        assert!(vm.task_index.data.is_empty());
        assert!(vm.is_idle());
    }
}
