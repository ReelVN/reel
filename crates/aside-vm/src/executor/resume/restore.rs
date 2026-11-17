use super::*;

/// One restored business value, held by a strong root in the current candidate heap.
#[derive(Clone, Debug)]
pub struct ResumeValue {
    pub key: BindingKey,
    pub root: RootToken,
}
/// Business values for one saved activation, keyed independently of register layout.
#[derive(Clone, Debug, Default)]
pub struct ResumeFrameValues {
    pub bindings: Vec<ResumeValue>,
}
/// Budget result for construction through the ordinary VM frame stores.
#[derive(Clone, Copy, Debug)]
pub enum ResumeProgress {
    BudgetYield(SliceUsage),
    Complete(TaskId),
}
/// Exclusive callback construction. Dropping an unfinished build cancels its task.
pub struct ResumeRestore<'a> {
    vm: &'a mut Vm,
    plan: &'a VerifiedResumePlan,
    values: &'a [ResumeFrameValues],
    state: RestoreState,
}
/// Movable callback construction in one current native ticket's task.
///
/// Input root handles remain owned by the host and must stay live until completion
/// or cancellation. An unfinished cursor must be cancelled with its original VM
/// before being dropped. No author values or VM borrows are retained by this cursor.
pub struct ResumeRestoreCursor {
    plan: VerifiedResumePlan,
    values: Vec<ResumeFrameValues>,
    state: RestoreState,
}
struct RestoreState {
    heap: HeapId,
    epoch: SessionEpoch,
    owner: RootOwner,
    task: TaskId,
    native: FrameId,
    base: usize,
    context: ContextFlags,
    effects: Effects,
    frame: usize,
    cell: usize,
    binding: usize,
    admitted: bool,
    complete: bool,
    failed: bool,
}
impl Vm {
    /// Consume a current issued ticket and restore its callback from a verified plan.
    /// The current continuation is rooted normally; business inputs stay borrowed.
    pub fn restore_resume<'a>(
        &'a mut self,
        ticket: HostTicket,
        plan: &'a VerifiedResumePlan,
        values: &'a [ResumeFrameValues],
        continuation: Continuation<'_>,
    ) -> Result<ResumeRestore<'a>, ResumeError> {
        let state = RestoreState::begin(self, ticket, plan, values, continuation)?;
        Ok(ResumeRestore {
            vm: self,
            plan,
            values,
            state,
        })
    }
    /// Consume a current issued ticket and build its callback in the same task.
    /// The cursor inherits this native's owner, context and effects. Until completion
    /// the host advances this task only through the cursor and retains input roots.
    pub fn restore_resume_cursor(
        &mut self,
        ticket: HostTicket,
        plan: VerifiedResumePlan,
        values: Vec<ResumeFrameValues>,
        continuation: Continuation<'_>,
    ) -> Result<ResumeRestoreCursor, ResumeError> {
        let state = RestoreState::begin(self, ticket, &plan, &values, continuation)?;
        Ok(ResumeRestoreCursor {
            plan,
            values,
            state,
        })
    }
}
impl ResumeRestore<'_> {
    /// Build frames and cells within the supplied work, allocation and soft-clock budget.
    pub fn step(
        &mut self,
        budget: SliceBudget,
        mut now: impl FnMut() -> u64,
    ) -> Result<ResumeProgress, ResumeError> {
        self.state
            .step(self.vm, self.plan, self.values, budget, &mut now)
    }
}
impl Drop for ResumeRestore<'_> {
    fn drop(&mut self) {
        let _ = self.state.cancel(self.vm);
    }
}
impl ResumeRestoreCursor {
    /// Advance construction in its original candidate VM. Errors cancel any partial task.
    pub fn step(
        &mut self,
        vm: &mut Vm,
        budget: SliceBudget,
        mut now: impl FnMut() -> u64,
    ) -> Result<ResumeProgress, ResumeError> {
        self.state
            .step(vm, &self.plan, &self.values, budget, &mut now)
    }
    /// Cancel an unfinished partial task. Completed tasks retain ordinary VM ownership.
    /// Root handles supplied by the host are never released by this operation.
    pub fn cancel(&mut self, vm: &mut Vm) -> Result<(), ResumeError> {
        self.state.cancel(vm)
    }
    /// Return the ordinary task only after every current frame and binding is installed.
    pub fn completed_task(&self) -> Option<TaskId> {
        self.state.complete.then_some(self.state.task)
    }
}
impl RestoreState {
    fn begin(
        vm: &mut Vm,
        ticket: HostTicket,
        plan: &VerifiedResumePlan,
        values: &[ResumeFrameValues],
        continuation: Continuation<'_>,
    ) -> Result<Self, ResumeError> {
        super::descriptor::same_program(vm, &plan.verified)?;
        let native = vm.ticket(ticket).map_err(error)?;
        if no_wait(native.context) || native.effects.0 & Effects::WAIT == 0 {
            return Err(ResumeError::new(
                "resume callback requires an ordinary waiting context",
            ));
        }
        let base = vm.task(ticket.task).map_err(error)?.frames.data.len();
        if values.len() != plan.frame_count()
            || base
                .checked_add(values.len())
                .and_then(|n| n.checked_add(1))
                .is_none_or(|n| n > vm.limits.max_frames as usize)
        {
            return Err(ResumeError::new(
                "resume activation count differs from the plan",
            ));
        }
        for (index, values) in values.iter().enumerate() {
            let (_, site) = plan.frame(index);
            if values.bindings.len() != site.layout.bindings.len() {
                return Err(ResumeError::new(
                    "saved business binding count differs from the current layout",
                ));
            }
        }
        // The existing response path admits and roots current continuation values
        // before consuming the ticket. No script is executed by this publication.
        vm.respond(ticket, HostResponse::Continue(continuation))
            .map_err(error)?;
        Ok(Self {
            heap: vm.heap.id(),
            epoch: vm.heap.epoch(),
            owner: native.owner,
            task: ticket.task,
            native: native.id,
            base,
            context: native.context,
            effects: native.effects,
            frame: 0,
            cell: 0,
            binding: 0,
            admitted: false,
            complete: false,
            failed: false,
        })
    }
    fn check_vm(&self, vm: &Vm) -> Result<(), ResumeError> {
        if (self.heap, self.epoch) != (vm.heap.id(), vm.heap.epoch()) {
            return Err(ResumeError::new(
                "resume cursor belongs to another candidate heap or session epoch",
            ));
        }
        Ok(())
    }
    fn cancel(&mut self, vm: &mut Vm) -> Result<(), ResumeError> {
        self.check_vm(vm)?;
        if !self.complete && !self.failed {
            vm.cancel_task(self.task).map_err(error)?;
            self.failed = true;
        }
        Ok(())
    }
    fn step(
        &mut self,
        vm: &mut Vm,
        plan: &VerifiedResumePlan,
        values: &[ResumeFrameValues],
        budget: SliceBudget,
        now: &mut impl FnMut() -> u64,
    ) -> Result<ResumeProgress, ResumeError> {
        self.check_vm(vm)?;
        if self.failed {
            return Err(ResumeError::new(
                "resume construction was cancelled or failed",
            ));
        }
        if self.complete {
            return Ok(ResumeProgress::Complete(self.task));
        }
        let result = (|| {
            vm.select_task(self.task).map_err(error)?;
            if vm.active.cancelling || vm.active.terminal.is_some() {
                return Err(ResumeError::new(
                    "partial resume task is no longer available",
                ));
            }
            let restored = self.frame + usize::from(self.admitted);
            if vm.active.frames.data.len() != self.base + restored
                || !matches!(vm.active.frames.data.get(self.base - 1), Some(Frame::Native(frame))
                        if frame.id == self.native && frame.continuation_active && matches!(frame.phase, NativePhase::Continue))
            {
                return Err(ResumeError::new(
                    "resume callback's current native boundary changed",
                ));
            }
            if restored != 0 {
                let index = self.frame.saturating_sub(usize::from(!self.admitted));
                let (flow, site) = plan.frame(index);
                if !matches!(vm.active.frames.data.last(), Some(Frame::Script(frame))
                        if frame.code.function == flow.function && frame.pc == site.entry
                            && matches!(frame.phase, ScriptPhase::Body))
                {
                    return Err(ResumeError::new(
                        "partial callback advanced outside its construction cursor",
                    ));
                }
            }
            self.advance(vm, plan, values, budget, now)
        })();
        if result.is_err() {
            let _ = vm.cancel_task(self.task);
            self.failed = true;
        }
        result
    }
    fn advance(
        &mut self,
        vm: &mut Vm,
        plan: &VerifiedResumePlan,
        values: &[ResumeFrameValues],
        budget: SliceBudget,
        now: &mut impl FnMut() -> u64,
    ) -> Result<ResumeProgress, ResumeError> {
        let mut meter = Meter::new(budget);
        loop {
            if meter.deadline_reached(now) {
                return Ok(ResumeProgress::BudgetYield(meter.usage));
            }
            if self.frame == plan.frame_count() {
                self.complete = true;
                return Ok(ResumeProgress::Complete(self.task));
            }
            let (flow, site) = plan.frame(self.frame);
            if !self.admitted {
                let (work, allocation) = vm.script_push_cost(flow.function, 0);
                if !meter.take(0, work, allocation) {
                    return Ok(ResumeProgress::BudgetYield(meter.usage));
                }
                let ret = if self.frame == 0 {
                    ReturnTo::Native { frame: self.native }
                } else {
                    let Frame::Script(parent) = vm.active.frames.data.last().copied().unwrap()
                    else {
                        unreachable!()
                    };
                    let (_, parent_site) = plan.frame(self.frame - 1);
                    ReturnTo::Register {
                        frame: parent.id,
                        reg: parent_site.result,
                        call: parent_site.call,
                    }
                };
                vm.push_script(
                    flow.function,
                    None,
                    ArgSource::None,
                    0,
                    self.owner,
                    self.context,
                    self.effects,
                    ret,
                )
                .map_err(error)?;
                let Frame::Script(frame) = vm.active.frames.data.last_mut().unwrap() else {
                    unreachable!()
                };
                frame.pc = site.entry;
                frame.phase = ScriptPhase::Body;
                self.admitted = true;
                continue;
            }
            let Frame::Script(frame) = vm.active.frames.data.last().copied().unwrap() else {
                unreachable!()
            };
            if let Some(&id) = site.layout.cells.get(self.cell) {
                let def =
                    &vm.program.package().functions[flow.function.index()].bindings[id.index()];
                let storage = def.storage;
                let init = def.initialization;
                let contract = vm.contract(ModuleId(frame.module.0), def.ty, def.mutable);
                if !meter.take(0, 5, std::mem::size_of::<crate::cell::CellState>() as u64) {
                    return Ok(ResumeProgress::BudgetYield(meter.usage));
                }
                let cell = vm.heap.alloc_language_cell(contract).map_err(error)?;
                vm.put(frame.regs, storage.reg().0, cell).map_err(error)?;
                if let BindingInitialization::Parameter(index) = init {
                    vm.put(frame.bound, index, cell).map_err(error)?;
                }
                self.cell += 1;
                continue;
            }
            if let Some(binding) = site.layout.bindings.get(self.binding) {
                let def = &vm.program.package().functions[flow.function.index()].bindings
                    [binding.binding.index()];
                let ty = def.ty;
                let storage = def.storage;
                let init = def.initialization;
                let lookup = (values[self.frame].bindings.len() as u64)
                    .saturating_mul(binding.key.0.len() as u64 + 1)
                    .saturating_mul(2);
                let work = lookup
                    .checked_add(5)
                    .and_then(|n| n.checked_add(vm.type_checks.extra(ty)))
                    .ok_or_else(|| ResumeError::new("resume binding work overflow"))?;
                if !meter.take(0, work, 0) {
                    return Ok(ResumeProgress::BudgetYield(meter.usage));
                }
                if values[self.frame]
                    .bindings
                    .iter()
                    .filter(|value| value.key == binding.key)
                    .count()
                    != 1
                {
                    return Err(ResumeError::new(format!(
                        "missing or duplicate business binding {}",
                        binding.key.0
                    ))
                    .at(flow.function, site.entry));
                }
                let value = values[self.frame]
                    .bindings
                    .iter()
                    .find(|value| value.key == binding.key)
                    .unwrap();
                let input = vm.heap.root_value(value.root).map_err(error)?;
                vm.check_type(ty, input).map_err(|e| {
                    ResumeError::new(format!(
                        "restored binding {} violates its current type: {e:?}",
                        binding.key.0
                    ))
                    .at(flow.function, site.entry)
                })?;
                match storage {
                    BindingStorage::Value(reg) => {
                        vm.put(frame.regs, reg.0, input).map_err(error)?
                    }
                    BindingStorage::Cell(reg) => {
                        let cell = vm.value(frame.regs, reg.0).map_err(error)?;
                        vm.heap.cell_initialize(cell, input).map_err(error)?;
                    }
                }
                if let BindingInitialization::Parameter(index) = init {
                    if matches!(storage, BindingStorage::Value(_)) {
                        vm.put(frame.bound, index, input).map_err(error)?;
                    }
                }
                self.binding += 1;
                continue;
            }
            if let Some(&id) = site
                .layout
                .initialized_dead
                .get(self.binding - site.layout.bindings.len())
            {
                if !meter.take(0, 2, 0) {
                    return Ok(ResumeProgress::BudgetYield(meter.usage));
                }
                let def =
                    &vm.program.package().functions[flow.function.index()].bindings[id.index()];
                let (register, init) = (def.storage.reg(), def.initialization);
                // Current-code liveness proves the old value cannot be read.
                // BindWrite still needs this register to be initialized.
                vm.put(frame.regs, register.0, Value::NULL).map_err(error)?;
                if let BindingInitialization::Parameter(index) = init {
                    vm.put(frame.bound, index, Value::NULL).map_err(error)?;
                }
                self.binding += 1;
                continue;
            }
            self.frame += 1;
            self.cell = 0;
            self.binding = 0;
            self.admitted = false;
        }
    }
}
