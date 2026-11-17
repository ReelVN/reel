//! One load pipeline: current data, current bootstrap, then its native business callback.
use super::context::{ContextSlots, Lookup};
use super::{CheckpointBinding, CheckpointError, CheckpointRecord, flow_continuation};
use crate::aside::{
    AdapterError, SessionVm, SliceBudget, budget::SliceMinimum, data::TypedRestore,
    stage::resources::SceneRestore,
};
use aside_vm::{
    HeapReservation, ObjectKind, RootKind, RootOwner, RootToken, Value,
    bytecode::Context,
    executor::{HostTicket, SliceUsage, TaskId, Vm},
    resume::{ResumeFrameValues, ResumeProgress, ResumeRestoreCursor, ResumeValue},
};
use std::sync::Arc;

/// The restored shared graph, kept alive while the current Root builds its view.
pub struct RestoredCheckpoint {
    record: Arc<CheckpointRecord>,
    root: RootToken,
    _charge: HeapReservation,
}
impl RestoredCheckpoint {
    /// Root State is independent of the live business bindings.
    pub fn root_state(&self, vm: &Vm) -> Result<Value, CheckpointError> {
        Ok(vm
            .heap()
            .sequence_get(vm.heap().root_value(self.root)?, 0)?)
    }
    /// Stable source contract and detached state used by this load.
    pub fn record(&self) -> &CheckpointRecord {
        &self.record
    }
    /// Release after adoption or cancellation in the same candidate heap.
    pub fn release(self, vm: &mut Vm) -> Result<(), CheckpointError> {
        vm.heap_mut().release_root(self.root)?;
        Ok(())
    }
}

/// Data preparation finishes before the current Root function is started.
pub struct CheckpointDataRestore {
    owner: RootOwner,
    record: Arc<CheckpointRecord>,
    graph: TypedRestore,
    charge: HeapReservation,
    complete: bool,
}
impl SessionVm {
    /// Bind current native await adapters after graph restoration has released
    /// its stage and activity borrows, before running the current bootstrap.
    pub fn prepare_checkpoint_resources(&mut self, work: u32) -> Result<bool, AdapterError> {
        if !self.activities.prepare_restored_step(&mut self.vm, work)? {
            return Ok(false);
        }
        self.media
            .prepare_restored_step(&mut self.vm, &self.stage, self.root_owner, work)
    }

    /// Rebase restored activity progress at candidate adoption. Resources stay
    /// frozen while the current bootstrap rebuilds its views.
    pub fn adopt_checkpoint_resources(&mut self, work: u32) -> Result<bool, AdapterError> {
        let (task, _) = self.flow_boundary().ok_or(AdapterError::Input(
            "restored resources require the current business flow",
        ))?;
        let owner = self.default_activity_owner(task)?;
        self.activities.set_restored_default_owner(owner);
        if !self
            .activities
            .adopt_restored_step(self.vm.heap_mut(), work)?
        {
            return Ok(false);
        }
        self.media
            .adopt_restored_step(self.vm.heap_mut(), owner, work)
    }

    /// Restore data after module initialization and automatic State registration.
    pub fn restore_checkpoint_data_begin(
        &mut self,
        record: Arc<CheckpointRecord>,
    ) -> Result<CheckpointDataRestore, CheckpointError> {
        if self.task.is_some() || self.pending.is_some() || self.delivery.is_some() {
            return Err(CheckpointError::Contract(
                "restore checkpoint data before starting the current Root",
            ));
        }
        if self.resume.table().root.as_ref().map(|root| &root.key) != Some(record.root()) {
            return Err(CheckpointError::Contract(
                "saved Root key differs from the current declaration",
            ));
        }
        self.resume.verify_plan(record.continuation())?;
        self.flow
            .registry
            .as_ref()
            .ok_or(CheckpointError::Contract(
                "prepare current State types before restoring a checkpoint",
            ))?;
        let charge = self.vm.heap().budget_usage().reserve(
            record.retained_bytes() - record.values().retained_bytes()
                + size_of::<CheckpointDataRestore>() as u64,
        )?;
        let graph =
            TypedRestore::new(self.vm.heap_mut(), self.root_owner, record.values().clone())?;
        Ok(CheckpointDataRestore {
            owner: self.root_owner,
            record,
            graph,
            charge,
            complete: false,
        })
    }
    /// Start the current declared Root. A loaded State follows the same argument
    /// binding and type checks as a normal script call.
    pub fn start_root(
        &mut self,
        session: &crate::Session,
        saved: Option<Value>,
    ) -> Result<TaskId, AdapterError> {
        self.live(session)?;
        if self.task.is_some() || self.pending.is_some() || self.delivery.is_some() {
            return Err(AdapterError::Input("main task already occupied"));
        }
        let function = self
            .resume
            .table()
            .root
            .as_ref()
            .ok_or(AdapterError::Input("story has no declared Root"))?
            .function;
        let argument = saved.map(|value| [value]);
        let task = self.vm.start(
            function,
            argument.as_ref().map_or(&[], |v| v.as_slice()),
            self.root_owner,
            Context::Ordinary,
        )?;
        self.task = Some(task);
        self.blocked_tasks_revision = None;
        self.main_minimum = SliceMinimum::default();
        self.main_wait = None;
        Ok(task)
    }
}
impl CheckpointDataRestore {
    /// Match classes, allocate objects, connect edges and apply defaults in bounded units.
    pub fn step(
        &mut self,
        candidate: &mut SessionVm,
        work: u32,
        chunk: usize,
    ) -> Result<bool, CheckpointError> {
        let references = SceneRestore::new(
            &mut candidate.stage,
            &mut candidate.activities,
            candidate.root_owner,
            candidate.read_limit,
        )
        .with_media(&mut candidate.media);
        self.complete = self.graph.step(
            candidate.vm.heap_mut(),
            candidate.flow.registry.as_ref().unwrap(),
            &references,
            work,
            chunk,
        )?;
        Ok(self.complete)
    }
    /// Transfer one rooted graph to bootstrap without retaining a VM or Registry borrow.
    pub fn finish(
        mut self,
        candidate: &mut SessionVm,
    ) -> Result<RestoredCheckpoint, CheckpointError> {
        if !self.complete {
            self.cancel(candidate)?;
            return Err(CheckpointError::Contract(
                "checkpoint data restoration incomplete",
            ));
        }
        let heap = candidate.vm.heap_mut();
        // Release the graph's temporary root even if creating the result root fails.
        let root = self.graph.value_root(heap, self.owner);
        let released = self.graph.release(heap);
        let root = root?;
        let restored = (|| {
            released?;
            let array = heap.root_value(root)?;
            let info = heap.info(array)?;
            let count = 1 + self
                .record
                .bindings()
                .iter()
                .flatten()
                .filter(|binding| matches!(binding, CheckpointBinding::Data(_)))
                .count();
            if info.kind != ObjectKind::Array || info.len as usize != count {
                return Err(CheckpointError::Contract(
                    "checkpoint root array differs from its binding contract",
                ));
            }
            self.charge.resize(self.record.retained_bytes())?;
            Ok(RestoredCheckpoint {
                record: self.record,
                root,
                _charge: self.charge,
            })
        })();
        if restored.is_err() {
            heap.release_root(root)?;
        }
        restored
    }
    /// Cancel data preparation before discarding the isolated candidate session.
    pub fn cancel(self, candidate: &mut SessionVm) -> Result<(), CheckpointError> {
        self.graph.release(candidate.vm.heap_mut())?;
        Ok(())
    }
}

/// Frame construction at the current run_flow ticket. The input graph remains
/// owned by the caller until this cursor finishes or is cancelled.
pub struct CheckpointResumeCursor {
    ticket: HostTicket,
    plan: Option<aside_vm::resume::VerifiedResumePlan>,
    values: Vec<ResumeFrameValues>,
    roots: Vec<RootToken>,
    frame: usize,
    binding: usize,
    offset: u32,
    cursor: Option<ResumeRestoreCursor>,
    complete: Option<TaskId>,
    failed: bool,
    charge: HeapReservation,
    context: Option<ContextSlots>,
    lookup: Lookup,
}
impl SessionVm {
    /// Connect restored business values to the freshly constructed native context.
    pub fn resume_checkpoint_begin(
        &mut self,
        ticket: HostTicket,
        restored: &RestoredCheckpoint,
    ) -> Result<CheckpointResumeCursor, CheckpointError> {
        if self.issued != Some(ticket) || self.flow_boundary() != Some((ticket.task, ticket.frame))
        {
            return Err(CheckpointError::Contract(
                "checkpoint resume requires the current business ticket",
            ));
        }
        let state = restored.root_state(&self.vm)?;
        let active = self
            .flow_root_state()
            .map_err(|e| CheckpointError::Data(e.into()))?;
        if !active.is_some_and(|value| value.same_bits(state)) {
            return Err(CheckpointError::Contract(
                "run_flow must receive the restored Root State instance",
            ));
        }
        let plan = self.resume.verify_plan(restored.record.continuation())?;
        let args = self.vm.host_arguments(ticket)?;
        if self.vm.unbound_function(args[0])? != plan.entry_function() {
            return Err(CheckpointError::Contract(
                "current run_flow entry differs from the saved flow",
            ));
        }
        let charge = self
            .vm
            .heap()
            .budget_usage()
            .reserve(size_of::<CheckpointResumeCursor>() as u64)?;
        Ok(CheckpointResumeCursor {
            ticket,
            plan: Some(plan),
            values: Vec::new(),
            roots: Vec::new(),
            frame: 0,
            binding: 0,
            offset: 1,
            cursor: None,
            complete: None,
            failed: false,
            charge,
            context: Some(ContextSlots::new(self.vm.heap(), args[2])?),
            lookup: Lookup::default(),
        })
    }
}
impl CheckpointResumeCursor {
    /// Populate a bounded group of input roots or advance the VM frame budget.
    pub fn step(
        &mut self,
        vm: &mut Vm,
        restored: &RestoredCheckpoint,
        work: u32,
        budget: SliceBudget,
        now: impl FnMut() -> u64,
    ) -> Result<ResumeProgress, CheckpointError> {
        if self.failed {
            return Err(CheckpointError::Contract(
                "checkpoint frame restoration failed",
            ));
        }
        let result = self.advance(vm, restored, work, budget, now);
        if result.is_err() {
            self.failed = true;
            let _ = vm.cancel_task(self.ticket.task);
        }
        result
    }
    fn advance(
        &mut self,
        vm: &mut Vm,
        restored: &RestoredCheckpoint,
        work: u32,
        budget: SliceBudget,
        now: impl FnMut() -> u64,
    ) -> Result<ResumeProgress, CheckpointError> {
        if let Some(task) = self.complete {
            return Ok(ResumeProgress::Complete(task));
        }
        if let Some(cursor) = &mut self.cursor {
            match cursor.step(vm, budget, now)? {
                ResumeProgress::Complete(task) => self.complete = Some(task),
                yielded => return Ok(yielded),
            }
            return Ok(pending());
        }
        let array = vm.heap().root_value(restored.root)?;
        for _ in 0..work {
            if let Some(context) = &mut self.context
                && !context.complete()
            {
                context.step(vm.heap())?;
                continue;
            }
            if self.frame == restored.record.bindings().len() {
                let state = restored.root_state(vm)?;
                let context = vm.host_arguments(self.ticket)?[2];
                self.context = None;
                self.cursor = Some(vm.restore_resume_cursor(
                    self.ticket,
                    self.plan.take().unwrap(),
                    std::mem::take(&mut self.values),
                    flow_continuation(&[state, context]),
                )?);
                return Ok(pending());
            }
            if self.values.len() == self.frame {
                let before = self.values.capacity();
                self.values.push(ResumeFrameValues::default());
                self.grow((self.values.capacity() - before) * size_of::<ResumeFrameValues>())?;
                continue;
            }
            let keys = &restored.record.bindings()[self.frame];
            if self.binding == keys.len() {
                self.frame += 1;
                self.binding = 0;
                continue;
            }
            let binding = &keys[self.binding];
            let value = match binding {
                CheckpointBinding::Data(_) => {
                    let value = vm.heap().sequence_get(array, self.offset)?;
                    self.offset += 1;
                    value
                }
                CheckpointBinding::CurrentContext(_) => {
                    let value = vm.host_arguments(self.ticket)?[2];
                    if value.tag() == aside_vm::ValueTag::Null {
                        return Err(CheckpointError::Contract(
                            "saved flow requires the current Root context",
                        ));
                    }
                    value
                }
                CheckpointBinding::CurrentContextMember { member, .. } => {
                    let Some(value) = self.context.as_ref().unwrap().lookup_step(
                        vm.heap(),
                        member,
                        &mut self.lookup,
                        1,
                    )?
                    else {
                        continue;
                    };
                    self.lookup = Lookup::default();
                    value
                }
            };
            let key = binding.key().clone();
            let key_bytes = key.0.capacity();
            let root = vm
                .heap_mut()
                .add_root(self.ticket.owner, RootKind::Host, value)?;
            let before = self.roots.capacity();
            self.roots.push(root);
            let root_bytes = (self.roots.capacity() - before) * size_of::<RootToken>();
            let bindings = &mut self.values[self.frame].bindings;
            let before = bindings.capacity();
            bindings.push(ResumeValue { key, root });
            let bytes =
                (bindings.capacity() - before) * size_of::<ResumeValue>() + key_bytes + root_bytes;
            self.grow(bytes)?;
            self.binding += 1;
        }
        Ok(pending())
    }
    fn grow(&mut self, bytes: usize) -> Result<(), CheckpointError> {
        self.charge.resize(self.charge.bytes() + bytes as u64)?;
        Ok(())
    }
    /// Release temporary binding roots after the current frames own their values.
    pub fn finish(mut self, vm: &mut Vm) -> Result<TaskId, CheckpointError> {
        let Some(task) = self.complete else {
            self.cancel(vm)?;
            return Err(CheckpointError::Contract(
                "checkpoint frame restoration incomplete",
            ));
        };
        for root in self.roots.drain(..) {
            vm.heap_mut().release_root(root)?;
        }
        Ok(task)
    }
    /// Cancel the candidate task and release temporary binding roots.
    pub fn cancel(mut self, vm: &mut Vm) -> Result<(), CheckpointError> {
        vm.cancel_task(self.ticket.task)?;
        for root in self.roots.drain(..) {
            vm.heap_mut().release_root(root)?;
        }
        Ok(())
    }
}
fn pending() -> ResumeProgress {
    ResumeProgress::BudgetYield(SliceUsage {
        required_work: 1,
        ..Default::default()
    })
}
