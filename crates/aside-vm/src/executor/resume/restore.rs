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
/// Exclusive candidate construction. Dropping an unfinished build cancels its task.
pub struct ResumeRestore<'a> {
    vm: &'a mut Vm,
    plan: &'a VerifiedResumePlan,
    values: &'a [ResumeFrameValues],
    state: RestoreState,
}
/// Movable candidate construction for a host event loop.
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
    task: Option<TaskId>,
    frame: usize,
    cell: usize,
    binding: usize,
    admitted: bool,
    complete: bool,
    failed: bool,
}
impl Vm {
    /// Begin restoration from a verified stable-key plan. Input roots remain borrowed
    /// until completion, so allocation and budget yields cannot collect future inputs.
    pub fn restore_resume<'a>(
        &'a mut self,
        plan: &'a VerifiedResumePlan,
        values: &'a [ResumeFrameValues],
        owner: RootOwner,
    ) -> Result<ResumeRestore<'a>, ResumeError> {
        let state = RestoreState::begin(self, plan, values, owner)?;
        Ok(ResumeRestore {
            vm: self,
            plan,
            values,
            state,
        })
    }
    /// Begin a movable restoration bound to this candidate heap and session epoch.
    /// The host retains all input roots and cancels an unfinished cursor explicitly.
    pub fn restore_resume_cursor(
        &mut self,
        plan: VerifiedResumePlan,
        values: Vec<ResumeFrameValues>,
        owner: RootOwner,
    ) -> Result<ResumeRestoreCursor, ResumeError> {
        let state = RestoreState::begin(self, &plan, &values, owner)?;
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
        self.state.complete.then_some(self.state.task).flatten()
    }
}
impl RestoreState {
    fn begin(
        vm: &Vm,
        plan: &VerifiedResumePlan,
        values: &[ResumeFrameValues],
        owner: RootOwner,
    ) -> Result<Self, ResumeError> {
        if vm.program.snapshot_identity() != plan.verified.program.snapshot_identity() {
            return Err(ResumeError::new(
                "resume plan belongs to another current program",
            ));
        }
        if values.len() != plan.frame_count() || values.len() > vm.limits.max_frames as usize {
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
        Ok(Self {
            heap: vm.heap.id(),
            epoch: vm.heap.epoch(),
            owner,
            task: None,
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
            if let Some(task) = self.task {
                vm.cancel_task(task).map_err(error)?;
            }
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
            return Ok(ResumeProgress::Complete(self.task.unwrap()));
        }
        let result = (|| {
            if let Some(task) = self.task {
                vm.select_task(task).map_err(error)?;
                if vm.active.cancelling || vm.active.terminal.is_some() {
                    return Err(ResumeError::new(
                        "partial resume task is no longer available",
                    ));
                }
                let expected = self.frame + usize::from(self.admitted);
                let index = self.frame.saturating_sub(usize::from(!self.admitted));
                let (flow, site) = plan.frame(index);
                if vm.active.frames.data.len() != expected
                    || !matches!(vm.active.frames.data.last(), Some(Frame::Script(frame))
                        if frame.code.function == flow.function && frame.pc == site.entry
                            && matches!(frame.phase, ScriptPhase::Body))
                {
                    return Err(ResumeError::new(
                        "partial resume task advanced outside its construction cursor",
                    ));
                }
            }
            self.advance(vm, plan, values, budget, now)
        })();
        if result.is_err() {
            if let Some(task) = self.task {
                let _ = vm.cancel_task(task);
            }
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
                return Ok(ResumeProgress::Complete(self.task.unwrap()));
            }
            let (flow, site) = plan.frame(self.frame);
            if !self.admitted {
                let (work, allocation) = vm.script_push_cost(flow.function, 0);
                if !meter.take(0, work, allocation) {
                    return Ok(ResumeProgress::BudgetYield(meter.usage));
                }
                if self.frame == 0 {
                    self.task = Some(
                        vm.start(flow.function, &[], self.owner, Context::Ordinary)
                            .map_err(error)?,
                    );
                } else {
                    let Frame::Script(parent) = vm.active.frames.data.last().copied().unwrap()
                    else {
                        unreachable!()
                    };
                    let (_, parent_site) = plan.frame(self.frame - 1);
                    vm.push_script(
                        flow.function,
                        None,
                        ArgSource::None,
                        0,
                        self.owner,
                        ContextFlags::from(Context::Ordinary),
                        Effects::ALL,
                        ReturnTo::Register {
                            frame: parent.id,
                            reg: parent_site.result,
                            call: parent_site.call,
                        },
                    )
                    .map_err(error)?;
                }
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
            self.frame += 1;
            self.cell = 0;
            self.binding = 0;
            self.admitted = false;
        }
    }
}
