use super::*;
use crate::heap::snapshot as graph;
use crate::snapshot::{
    Record, RecordKind, SnapshotBudget, SnapshotError, SnapshotMap, SnapshotProgress,
    SnapshotRegistry, VERSION, VmSnapshot,
};
use serde::{Deserialize, Serialize};

impl Serialize for TaskId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.serial.serialize(s)
    }
}
impl<'de> Deserialize<'de> for TaskId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self {
            heap: HeapId(0),
            epoch: SessionEpoch(0),
            serial: u64::deserialize(d)?,
        })
    }
}
macro_rules! scoped_serial {
    ($ty:ty) => {
        impl Serialize for $ty {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                (self.task, self.serial).serialize(s)
            }
        }
        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let (task, serial) = Deserialize::deserialize(d)?;
                Ok(Self {
                    task,
                    serial,
                    heap: HeapId(0),
                    epoch: SessionEpoch(0),
                })
            }
        }
    };
}
scoped_serial!(WaitId);
scoped_serial!(DeliveryId);
impl SnapshotMap {
    pub(crate) fn rebase_task(&self, task: TaskId) -> Result<TaskId, SnapshotError> {
        self.check(task.heap, Some(task.epoch))?;
        Ok(TaskId {
            heap: self.heap,
            epoch: self.epoch,
            serial: task.serial,
        })
    }
    pub(crate) fn rebase_wait(&self, wait: WaitId) -> Result<WaitId, SnapshotError> {
        self.check(wait.heap, Some(wait.epoch))?;
        Ok(WaitId {
            heap: self.heap,
            epoch: self.epoch,
            task: self.task(wait.task)?,
            serial: wait.serial,
        })
    }
    pub fn delivery(&self, delivery: DeliveryId) -> Result<DeliveryId, SnapshotError> {
        self.check(delivery.heap, Some(delivery.epoch))?;
        Ok(DeliveryId {
            heap: self.heap,
            epoch: self.epoch,
            task: self.task(delivery.task)?,
            serial: delivery.serial,
        })
    }
    pub fn ticket(&self, mut ticket: HostTicket) -> Result<HostTicket, SnapshotError> {
        self.check(ticket.heap, Some(ticket.epoch))?;
        ticket.task = self.task(ticket.task)?;
        ticket.heap = self.heap;
        ticket.epoch = self.epoch;
        Ok(ticket)
    }
}

#[derive(Serialize, Deserialize)]
struct Header {
    host_types: [u8; 32],
    active_slot: Option<usize>,
    tasks: usize,
    globals: RootRangeToken,
    constants: RootRangeToken,
    next_id: u64,
    native_entries: u64,
    native_resumes: u64,
    native_null_publications: u64,
    slices: u64,
    last_usage: SliceUsage,
    reactive_capture: Option<Value>,
    reactive_evaluation: u64,
}
#[derive(Serialize, Deserialize)]
struct SavedTask {
    frames: usize,
    unwinds: usize,
    caught: usize,
    equality_cursors: usize,
    equality_roots: Option<RootRangeToken>,
    delivery: Option<RootRangeToken>,
    terminal: Option<Terminal>,
    cancelling: bool,
    cancel_boundary: Option<CallbackBoundary>,
    task_owner: Option<RootOwner>,
    task_id: Option<TaskId>,
    root_owner: RootOwner,
    fault_build: Option<FaultBuild>,
    blocked: Option<ResourceBlocked>,
    cooperative_cancel: bool,
    abort_equality: bool,
    model_copy: bool,
}
impl SavedTask {
    fn capture(task: &TaskState) -> Result<Self, SnapshotError> {
        Ok(Self {
            frames: task.frames.data.len(),
            unwinds: task.unwinds.data.len(),
            caught: task.caught.data.len(),
            equality_cursors: task.equality_cursors.data.len(),
            equality_roots: task.equality_roots,
            delivery: task.delivery,
            terminal: task.terminal,
            cancelling: task.cancelling,
            cancel_boundary: task.cancel_boundary,
            task_owner: task.task_owner,
            task_id: task.task_id,
            root_owner: task.root_owner,
            fault_build: task.fault_build,
            blocked: task.blocked,
            cooperative_cancel: task.cooperative_cancel,
            abort_equality: task.abort_equality,
            model_copy: task.model_copy.is_some(),
        })
    }
    fn restore(mut self, vm: &mut Vm, map: &SnapshotMap) -> Result<TaskState, SnapshotError> {
        let limits = vm.limits;
        if self.frames > limits.max_frames as usize
            || self.unwinds > limits.max_unwinds as usize
            || self.caught > limits.max_unwinds as usize
            || self.equality_cursors > limits.max_unwinds as usize
        {
            return Err(SnapshotError::Limit);
        }
        if self.cancel_boundary.is_some()
            && (!self.cancelling || self.task_id.is_none() || self.terminal.is_some())
        {
            return Err(SnapshotError::Invalid("callback retirement state".into()));
        }
        let usage = vm.heap.budget_usage();
        let mut task = TaskState::new(&usage, &mut vm.heap, &mut vm.slots, limits, false)?;
        // Task pools retain execution headroom, not the serialized allocator layout.
        task.frames = ChargedVec::new(&usage, limits.max_frames as usize)?;
        task.unwinds = ChargedVec::new(&usage, limits.max_unwinds as usize)?;
        task.caught = ChargedVec::new(&usage, limits.max_unwinds as usize)?;
        task.equality_cursors = ChargedVec::new(&usage, limits.max_unwinds as usize)?;
        task.equality_roots = self.equality_roots.map(|r| map.range(r)).transpose()?;
        task.delivery = self.delivery.map(|r| map.range(r)).transpose()?;
        if let Some(terminal) = &mut self.terminal {
            terminal.id = map.delivery(terminal.id)?;
            rebase_fault_option(&mut terminal.fault, map)?;
        }
        task.terminal = self.terminal;
        task.cancelling = self.cancelling;
        task.cancel_boundary = self.cancel_boundary;
        task.task_owner = self.task_owner;
        task.task_id = self.task_id.map(|t| map.task(t)).transpose()?;
        task.root_owner = self.root_owner;
        if let Some(build) = &mut self.fault_build {
            rebase_fault(&mut build.seed, map)?;
            rebase_id(&mut build.implicit, map)?;
        }
        task.fault_build = self.fault_build;
        if let Some(blocked) = &mut self.blocked {
            blocked.task = map.task(blocked.task)?;
        }
        task.blocked = self.blocked;
        task.cooperative_cancel = self.cooperative_cancel;
        task.abort_equality = self.abort_equality;
        Ok(task)
    }
}

/// Exclusive read borrow enforces the capture barrier without runtime write checks.
pub struct VmCapture<'a> {
    vm: &'a Vm,
    registry: &'a SnapshotRegistry,
    image: VmSnapshot,
    graph: graph::GraphCapture,
    header: bool,
    task: usize,
    part: u8,
    item: usize,
    complete: bool,
}
impl Vm {
    pub fn snapshot_begin<'a>(
        &'a self,
        registry: &'a SnapshotRegistry,
    ) -> Result<VmCapture<'a>, SnapshotError> {
        Ok(VmCapture {
            vm: self,
            registry,
            graph: graph::GraphCapture::new(&self.heap)?,
            image: VmSnapshot {
                version: VERSION,
                code: self.program.snapshot_identity(),
                source_heap: self.heap.id().get(),
                source_epoch: self.heap.epoch().0,
                records: Vec::new(),
                bytes: 0,
            },
            header: false,
            task: 0,
            part: 0,
            item: 0,
            complete: false,
        })
    }
    /// Builds an unpublished candidate. The caller commits only after host adapters succeed.
    pub fn restore_begin<'a>(
        image: &'a VmSnapshot,
        program: VerifiedProgram,
        support: RuntimeSupport,
        limits: VmLimits,
        epoch: SessionEpoch,
        registry: &'a SnapshotRegistry,
    ) -> Result<VmRestore<'a>, SnapshotError> {
        if image.version != VERSION {
            return Err(SnapshotError::Version {
                format: "VM",
                expected: VERSION,
                actual: image.version,
            });
        }
        if image.code != program.snapshot_identity() {
            return Err(SnapshotError::CodeMismatch);
        }
        if epoch.0 == image.source_epoch {
            return Err(SnapshotError::Invalid(
                "restore requires a fresh session epoch".into(),
            ));
        }
        let mut vm = Self::load_initial(program, support, limits, epoch, false)?;
        // Discard the two empty bootstrap descriptors. No initializer or author code ran.
        vm.slots.release(&mut vm.heap, vm.globals)?;
        vm.slots.release(&mut vm.heap, vm.constants)?;
        let map = SnapshotMap::new(&vm.heap, image);
        Ok(VmRestore {
            vm,
            map,
            image,
            registry,
            pass: 0,
            cursor: 0,
            header: false,
            task_headers: 0,
            expected_tasks: 0,
            range_values: 0,
            constant_validation: None,
            callback_validation: None,
            generator_validation: None,
            complete: false,
        })
    }
}
impl VmCapture<'_> {
    pub fn step(&mut self, budget: SnapshotBudget) -> Result<SnapshotProgress, SnapshotError> {
        let before = self.image.bytes;
        let mut progress = SnapshotProgress::default();
        while !self.complete && progress.units < budget.units {
            progress.units += 1;
            if !self.header {
                let vm = self.vm;
                self.image.push(Record::new(
                    RecordKind::Vm,
                    0,
                    0,
                    &Header {
                        host_types: vm.program.checks.host_types.fingerprint,
                        active_slot: vm.active_slot,
                        tasks: vm.tasks.data.len(),
                        globals: vm.globals,
                        constants: vm.constants,
                        next_id: vm.next_id,
                        native_entries: vm.native_entries,
                        native_resumes: vm.native_resumes,
                        native_null_publications: vm.native_null_publications,
                        slices: vm.slices,
                        last_usage: vm.last_usage,
                        reactive_capture: vm.heap.snapshot_reactive_state().0,
                        reactive_evaluation: vm.heap.snapshot_reactive_state().1,
                    },
                )?);
                self.header = true;
            } else if self.task <= self.vm.tasks.data.len() {
                let task = if self.task == 0 {
                    &self.vm.active
                } else {
                    &self.vm.tasks.data[self.task - 1]
                };
                let key = self.task as u64;
                let record = match self.part {
                    0 => Some(Record::new(
                        RecordKind::Task,
                        key,
                        0,
                        &SavedTask::capture(task)?,
                    )?),
                    1 => task
                        .frames
                        .data
                        .get(self.item)
                        .map(|v| Record::new(RecordKind::Frame, key, self.item as u32, v))
                        .transpose()?,
                    2 => task
                        .unwinds
                        .data
                        .get(self.item)
                        .map(|v| Record::new(RecordKind::Unwind, key, self.item as u32, v))
                        .transpose()?,
                    3 => task
                        .caught
                        .data
                        .get(self.item)
                        .map(|v| Record::new(RecordKind::Caught, key, self.item as u32, v))
                        .transpose()?,
                    4 => task
                        .equality_cursors
                        .data
                        .get(self.item)
                        .map(|v| Record::new(RecordKind::Equality, key, self.item as u32, v))
                        .transpose()?,
                    5 => {
                        if self.item == 0 {
                            task.model_copy
                                .as_ref()
                                .map(|v| Record::new(RecordKind::ModelCopy, key, 0, &v.snapshot()))
                                .transpose()?
                        } else {
                            None
                        }
                    }
                    6 => task
                        .model_copy
                        .as_ref()
                        .and_then(|v| v.snapshot_object(self.item))
                        .map(|v| Record::new(RecordKind::CopyObject, key, self.item as u32, v))
                        .transpose()?,
                    _ => {
                        self.task += 1;
                        self.part = 0;
                        self.item = 0;
                        continue;
                    }
                };
                if let Some(record) = record {
                    self.image.push(record);
                    self.item += 1;
                } else {
                    self.part += 1;
                    self.item = 0;
                }
                if self.part == 0 {
                    self.part = 1;
                    self.item = 0;
                }
            } else {
                self.complete = self.graph.step(
                    &self.vm.heap,
                    &self.vm.slots,
                    self.registry,
                    &mut self.image,
                )?;
            }
        }
        progress.bytes = self.image.bytes - before;
        progress.complete = self.complete;
        Ok(progress)
    }
    pub fn finish(self) -> Result<VmSnapshot, SnapshotError> {
        if !self.complete {
            return Err(SnapshotError::Busy("snapshot capture is incomplete"));
        }
        Ok(self.image)
    }
}

pub struct VmRestore<'a> {
    vm: Vm,
    map: SnapshotMap,
    image: &'a VmSnapshot,
    registry: &'a SnapshotRegistry,
    pass: u8,
    cursor: usize,
    header: bool,
    task_headers: usize,
    expected_tasks: usize,
    range_values: u64,
    constant_validation: Option<ConstantValidation>,
    callback_validation: Option<CallbackValidation>,
    generator_validation: Option<generators::GeneratorValidation>,
    complete: bool,
}
impl VmRestore<'_> {
    pub fn step(&mut self, budget: SnapshotBudget) -> Result<SnapshotProgress, SnapshotError> {
        let mut progress = SnapshotProgress::default();
        while !self.complete && progress.units < budget.units {
            if let Some(validation) = &mut self.constant_validation {
                if validation.step(&self.vm)? {
                    self.constant_validation = None;
                }
                progress.units += 1;
                continue;
            }
            if let Some(validation) = &mut self.callback_validation {
                if validation.step(&self.vm)? {
                    self.callback_validation = None;
                }
                progress.units += 1;
                continue;
            }
            if let Some(validation) = &mut self.generator_validation {
                if validation.step(&self.vm)? {
                    self.generator_validation = None;
                }
                progress.units += 1;
                continue;
            }
            let Some(record) = self.image.records.get(self.cursor) else {
                self.cursor = 0;
                self.pass += 1;
                if self.pass == 4 {
                    if !self.header
                        || self.task_headers != self.expected_tasks + 1
                        || self.vm.tasks.data.len() != self.expected_tasks
                    {
                        return Err(SnapshotError::Invalid("missing VM/task records".into()));
                    }
                    self.complete = true;
                }
                continue;
            };
            match self.pass {
                0 => {
                    if record.kind == RecordKind::Range {
                        let range: crate::root_ranges::RangeDescriptor = record.read()?;
                        self.range_values += u64::from(range.len);
                        // Each captured slot has at least its initialized-state byte.
                        // Reject forged counts before allocating execution storage.
                        if self.range_values > self.image.bytes {
                            return Err(SnapshotError::Invalid(
                                "root ranges exceed encoded slots".into(),
                            ));
                        }
                    }
                    graph::allocate(
                        &mut self.vm.heap,
                        &mut self.vm.slots,
                        &mut self.map,
                        record,
                        self.registry,
                    )?;
                }
                1 => graph::connect(&mut self.vm.heap, &mut self.vm.slots, &mut self.map, record)?,
                2 => self.restore_control(record)?,
                3 => {
                    graph::validate(&self.vm.heap, &self.map, record)?;
                    if record.kind == RecordKind::Object {
                        let object = *self
                            .map
                            .objects
                            .get(&record.key)
                            .ok_or(HeapError::InvalidHandle)?;
                        if self.vm.heap.info(object)?.kind == ObjectKind::Generator {
                            self.generator_validation =
                                Some(generators::GeneratorValidation::new(&self.vm, object)?);
                        }
                    }
                    self.validate_task(record)?;
                }
                _ => unreachable!(),
            }
            progress.units += 1;
            progress.bytes += record.data.len() as u64;
            self.cursor += 1;
        }
        progress.complete = self.complete;
        Ok(progress)
    }
    pub fn finish(self) -> Result<(Vm, SnapshotMap), SnapshotError> {
        if !self.complete {
            return Err(SnapshotError::Busy("snapshot restoration is incomplete"));
        }
        Ok((self.vm, self.map))
    }
    fn task_mut(&mut self, key: u64) -> Result<&mut TaskState, SnapshotError> {
        if key == 0 {
            Ok(&mut self.vm.active)
        } else {
            self.vm
                .tasks
                .data
                .get_mut((key - 1) as usize)
                .ok_or_else(|| SnapshotError::Invalid("task record identity".into()))
        }
    }
    fn restore_control(&mut self, record: &Record) -> Result<(), SnapshotError> {
        match record.kind {
            RecordKind::Vm => {
                if self.header {
                    return Err(SnapshotError::Invalid("duplicate VM record".into()));
                }
                let header: Header = record.read()?;
                if header.host_types != self.vm.program.checks.host_types.fingerprint {
                    return Err(SnapshotError::Invalid(
                        "trusted host type registry changed".into(),
                    ));
                }
                if header.tasks > self.vm.limits.max_tasks as usize
                    || header.tasks >= self.image.records.len()
                    || header.active_slot.is_some_and(|i| i >= header.tasks)
                {
                    return Err(SnapshotError::Invalid("task count/selection".into()));
                }
                self.vm.tasks = ChargedVec::new(&self.vm.heap.budget_usage(), header.tasks)?;
                self.expected_tasks = header.tasks;
                self.vm.active_slot = header.active_slot;
                self.vm.globals = self.map.range(header.globals)?;
                self.vm.constants = self.map.range(header.constants)?;
                self.vm.next_id = header.next_id;
                self.vm.native_entries = header.native_entries;
                self.vm.native_resumes = header.native_resumes;
                self.vm.native_null_publications = header.native_null_publications;
                self.vm.slices = header.slices;
                self.vm.last_usage = header.last_usage;
                self.vm.heap.restore_reactive_state(
                    header
                        .reactive_capture
                        .map(|value| self.map.value(value))
                        .transpose()?,
                    header.reactive_evaluation,
                )?;
                self.header = true;
            }
            RecordKind::Task => {
                if !self.header || record.key != self.task_headers as u64 {
                    return Err(SnapshotError::Invalid("task header order".into()));
                }
                let saved: SavedTask = record.read()?;
                let task = saved.restore(&mut self.vm, &self.map)?;
                if record.key == 0 {
                    self.vm.active = task;
                } else {
                    if self.vm.tasks.data.len() == self.vm.tasks.data.capacity() {
                        return Err(SnapshotError::Invalid("excess task records".into()));
                    }
                    self.vm.tasks.data.push(task);
                }
                self.task_headers += 1;
            }
            RecordKind::Frame => {
                let mut frame: Frame = record.read()?;
                rebase_frame(&mut frame, &self.map)?;
                self.constant_validation = validate_frame(&frame, &self.vm)?;
                append(&mut self.task_mut(record.key)?.frames, record.offset, frame)?;
            }
            RecordKind::Unwind => {
                let mut unwind: Unwind = record.read()?;
                unwind.value = self.map.range(unwind.value)?;
                rebase_exit(&mut unwind.reason, &self.map)?;
                append(
                    &mut self.task_mut(record.key)?.unwinds,
                    record.offset,
                    unwind,
                )?;
            }
            RecordKind::Caught => {
                let mut caught: CaughtFault = record.read()?;
                rebase_fault(&mut caught.info, &self.map)?;
                append(
                    &mut self.task_mut(record.key)?.caught,
                    record.offset,
                    caught,
                )?;
            }
            RecordKind::Equality => {
                let value: u32 = record.read()?;
                append(
                    &mut self.task_mut(record.key)?.equality_cursors,
                    record.offset,
                    value,
                )?;
            }
            RecordKind::ModelCopy => {
                let copy = models::ModelCopy::restore(record.read()?, &self.vm.heap, &self.map)?;
                self.task_mut(record.key)?.model_copy = Some(copy);
            }
            RecordKind::CopyObject => {
                let task = if record.key == 0 {
                    &mut self.vm.active
                } else {
                    self.vm
                        .tasks
                        .data
                        .get_mut((record.key - 1) as usize)
                        .ok_or_else(|| SnapshotError::Invalid("model copy task".into()))?
                };
                task.model_copy
                    .as_mut()
                    .ok_or_else(|| SnapshotError::Invalid("model copy header".into()))?
                    .restore_object(record.offset, record.read()?, &self.vm.heap, &self.map)?;
            }
            _ => (),
        }
        Ok(())
    }
    fn validate_task(&mut self, record: &Record) -> Result<(), SnapshotError> {
        if record.kind == RecordKind::ModelCopy {
            self.task_mut(record.key)?
                .model_copy
                .as_ref()
                .ok_or_else(|| SnapshotError::Invalid("model copy header".into()))?
                .validate_snapshot(record.read()?)?;
        }
        if record.kind != RecordKind::Task {
            return Ok(());
        }
        let saved: SavedTask = record.read()?;
        let task = self.task_mut(record.key)?;
        if saved.frames != task.frames.data.len()
            || saved.unwinds != task.unwinds.data.len()
            || saved.caught != task.caught.data.len()
            || saved.equality_cursors != task.equality_cursors.data.len()
            || saved.model_copy != task.model_copy.is_some()
        {
            return Err(SnapshotError::Invalid("task record counts".into()));
        }
        if let Some(boundary) = task.cancel_boundary {
            self.callback_validation = Some(CallbackValidation {
                task: record.key,
                boundary,
                next: 0,
            });
        }
        Ok(())
    }
}

/// Locate and validate a saved retirement boundary one frame per restore unit.
/// Its return value is already mapped with the native frame's ordinary roots.
struct CallbackValidation {
    task: u64,
    boundary: CallbackBoundary,
    next: usize,
}
impl CallbackValidation {
    fn step(&mut self, vm: &Vm) -> Result<bool, SnapshotError> {
        let task = if self.task == 0 {
            &vm.active
        } else {
            &vm.tasks.data[self.task as usize - 1]
        };
        let frame = task
            .frames
            .data
            .get(self.next)
            .ok_or_else(|| SnapshotError::Invalid("callback retirement boundary".into()))?;
        self.next += 1;
        if frame.id() != self.boundary.frame() {
            return Ok(false);
        }
        let Frame::Native(native) = frame else {
            return Err(SnapshotError::Invalid(
                "callback retirement native frame".into(),
            ));
        };
        if !native.continuation_active {
            return Err(SnapshotError::Invalid(
                "callback retirement continuation".into(),
            ));
        }
        if matches!(self.boundary, CallbackBoundary::Returned(_)) {
            vm.value(native.roots, 0)?;
        }
        Ok(true)
    }
}
fn append<T>(values: &mut ChargedVec<T>, index: u32, value: T) -> Result<(), SnapshotError> {
    if values.data.len() != index as usize || values.data.len() == values.data.capacity() {
        return Err(SnapshotError::Invalid("control record count/order".into()));
    }
    values.data.push(value);
    Ok(())
}
fn rebase_id(id: &mut Option<FaultId>, map: &SnapshotMap) -> Result<(), SnapshotError> {
    if let Some(id) = id {
        id.0 = map.handle(id.0)?;
    }
    Ok(())
}
fn rebase_fault(fault: &mut FaultSeed, map: &SnapshotMap) -> Result<(), SnapshotError> {
    rebase_id(&mut fault.fault, map)?;
    rebase_id(&mut fault.explicit, map)?;
    if let Some((id, _)) = &mut fault.awaited {
        id.0 = map.handle(id.0)?;
    }
    Ok(())
}
fn rebase_fault_option(
    fault: &mut Option<FaultSeed>,
    map: &SnapshotMap,
) -> Result<(), SnapshotError> {
    if let Some(fault) = fault {
        rebase_fault(fault, map)?;
    }
    Ok(())
}
fn rebase_exit(exit: &mut ExitReason, map: &SnapshotMap) -> Result<(), SnapshotError> {
    if let ExitReason::Throw(fault) = exit {
        rebase_fault(fault, map)?;
    }
    Ok(())
}
fn rebase_frame(frame: &mut Frame, map: &SnapshotMap) -> Result<(), SnapshotError> {
    match frame {
        Frame::Script(frame) => {
            frame.regs = map.range(frame.regs)?;
            frame.env = map.range(frame.env)?;
            frame.args = map.range(frame.args)?;
            frame.bound = map.range(frame.bound)?;
            match &mut frame.phase {
                ScriptPhase::Exit { reason, .. }
                | ScriptPhase::ExitApply { reason, .. }
                | ScriptPhase::Release { reason } => rebase_exit(reason, map)?,
                _ => (),
            }
        }
        Frame::Native(frame) => {
            frame.args = map.range(frame.args)?;
            frame.bound = map.range(frame.bound)?;
            frame.bindings = frame.bindings.map(|r| map.range(r)).transpose()?;
            frame.roots = map.range(frame.roots)?;
            rebase_id(&mut frame.callback_fault, map)?;
            frame.derived_output = frame.derived_output.map(|v| map.value(v)).transpose()?;
            rebase_fault_option(&mut frame.pending_fault, map)?;
            if let NativePhase::Waiting(wait) = &mut frame.phase {
                *wait = map.wait(*wait)?;
            }
        }
    }
    Ok(())
}
fn validate_frame(frame: &Frame, vm: &Vm) -> Result<Option<ConstantValidation>, SnapshotError> {
    match frame {
        Frame::Script(frame) => {
            let code = vm
                .program
                .package()
                .functions
                .get(frame.code.function.index())
                .ok_or_else(|| SnapshotError::Invalid("frame function".into()))?;
            if frame.code.version != vm.program.package().content_version
                || frame.pc.0 as usize > code.code.len()
                || frame.module.0 as usize >= vm.program.package().modules.len()
                || vm.heap.range_descriptor(frame.regs)?.len != code.register_count
            {
                return Err(SnapshotError::Invalid("script frame code/registers".into()));
            }
            generators::validate_generator_frame(vm, *frame)?;
            match frame.phase {
                ScriptPhase::Constant(work) => {
                    let instruction = code
                        .code
                        .get(frame.pc.index())
                        .ok_or_else(|| SnapshotError::Invalid("constant instruction".into()))?;
                    if instruction.opcode != Opcode::LoadConst as u16
                        || work.dest != Reg(instruction.operands[0])
                    {
                        return Err(SnapshotError::Invalid("constant destination".into()));
                    }
                    return validate_constant_cursor(
                        vm,
                        ConstId(instruction.operands[1]),
                        work,
                        frame.env,
                        [1, 2],
                    );
                }
                ScriptPhase::DefaultConstant { parameter, work } => {
                    let signature = &vm.program.package().signatures[code.signature.index()];
                    return validate_default_constant(
                        vm,
                        signature,
                        parameter,
                        work,
                        frame.env,
                        [1, 2],
                    );
                }
                _ => {}
            }
        }
        Frame::Native(frame) => {
            let native = vm
                .program
                .package()
                .natives
                .get(frame.native.index())
                .ok_or_else(|| SnapshotError::Invalid("frame native".into()))?;
            if native.signature != frame.signature
                || frame.continuation_len > vm.limits.max_continuation_values
            {
                return Err(SnapshotError::Invalid("native continuation schema".into()));
            }
            if let NativePhase::DefaultConstant { parameter, work } = frame.phase {
                return validate_default_constant(
                    vm,
                    &vm.program.package().signatures[frame.signature.index()],
                    parameter,
                    work,
                    frame.roots,
                    [0, 1],
                );
            }
        }
    }
    Ok(None)
}

fn validate_default_constant(
    vm: &Vm,
    signature: &Signature,
    parameter: u32,
    work: constants::ConstantWork,
    roots: RootRangeToken,
    slots: [u32; 2],
) -> Result<Option<ConstantValidation>, SnapshotError> {
    let Some(ParameterDefault::Constant(origin)) = signature
        .parameters
        .get(parameter as usize)
        .and_then(|p| p.default)
    else {
        return Err(SnapshotError::Invalid("constant default parameter".into()));
    };
    if work.dest != Reg(parameter) {
        return Err(SnapshotError::Invalid(
            "constant default destination".into(),
        ));
    }
    validate_constant_cursor(vm, origin, work, roots, slots)
}

fn validate_constant_cursor(
    vm: &Vm,
    origin: ConstId,
    work: constants::ConstantWork,
    roots: RootRangeToken,
    slots: [u32; 2],
) -> Result<Option<ConstantValidation>, SnapshotError> {
    let constants = &vm.program.package().constants;
    let Some(Const::Tuple(elements)) = constants.get(work.id.index()) else {
        return Err(SnapshotError::Invalid("constant tuple cursor".into()));
    };
    let builder = vm.value(roots, slots[0])?;
    let parent_value = vm.value(roots, slots[1])?;
    let builder = vm.heap.info(builder)?;
    let parents = vm.heap.info(parent_value)?;
    if !matches!(constants.get(origin.index()), Some(Const::Tuple(_)))
        || work.id.0 > origin.0
        || work.index as usize > elements.len()
        || builder.kind != ObjectKind::Array
        || builder.len != work.index
        || parents.kind != ObjectKind::Array
        || !parents.len.is_multiple_of(3)
    {
        return Err(SnapshotError::Invalid("constant construction state".into()));
    }
    if parents.len == 0 {
        if origin != work.id {
            return Err(SnapshotError::Invalid("constant cursor origin".into()));
        }
        Ok(None)
    } else {
        Ok(Some(ConstantValidation {
            parents: parent_value,
            next: parents.len,
            child: work.id,
            origin,
        }))
    }
}

/// Restore validates one suspended parent per budget unit; execution then uses
/// the same checked internal cursor without repeated defensive checks.
struct ConstantValidation {
    parents: Value,
    next: u32,
    child: ConstId,
    origin: ConstId,
}
impl ConstantValidation {
    fn step(&mut self, vm: &Vm) -> Result<bool, SnapshotError> {
        self.next -= 3;
        let parent = vm.heap.get_field(self.parents, self.next)?;
        let id = vm
            .heap
            .get_field(self.parents, self.next + 1)?
            .as_int()
            .and_then(|id| u32::try_from(id).ok())
            .map(ConstId)
            .ok_or_else(|| SnapshotError::Invalid("constant parent identity".into()))?;
        let index = vm
            .heap
            .get_field(self.parents, self.next + 2)?
            .as_int()
            .and_then(|index| u32::try_from(index).ok())
            .ok_or_else(|| SnapshotError::Invalid("constant parent index".into()))?;
        let Some(Const::Tuple(elements)) = vm.program.package().constants.get(id.index()) else {
            return Err(SnapshotError::Invalid("constant parent tuple".into()));
        };
        let parent = vm.heap.info(parent)?;
        if id.0 <= self.child.0
            || index == 0
            || elements.get(index as usize - 1) != Some(&self.child)
            || parent.kind != ObjectKind::Array
            || parent.len != index - 1
        {
            return Err(SnapshotError::Invalid(
                "constant parent construction".into(),
            ));
        }
        self.child = id;
        if self.next == 0 && self.child != self.origin {
            return Err(SnapshotError::Invalid("constant parent origin".into()));
        }
        Ok(self.next == 0)
    }
}
