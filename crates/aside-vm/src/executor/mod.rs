//! Budgeted execution of the single shared verified-bytecode format.
//!
//! Values in frames, continuations and terminal deliveries are held in FrameSlots.
//! Host entry and continuation-resume tickets are distinct, single-use identities.
use crate::bytecode::*;
use crate::fault::{CodeLease, FaultData};
use crate::payload::{Bytes, Values};
use crate::root_ranges::{ChargedVec, Reservation};
use crate::*;
use std::cmp::Ordering;
use std::rc::Rc;

mod api;
mod arguments;
use arguments::{ArgumentBinding, ArgumentStep, ParameterNames};
mod awaitables;
mod builtins;
mod call_sites;
mod calls;
pub use call_sites::{HostCallSite, HostCallSites};
mod classes;
use calls::NativeSite;
#[cfg(test)]
mod collection_tests;
mod collections;
mod constants;
mod generators;
use generators::GeneratorAction;
#[cfg(test)]
mod generator_tests;
mod iterators;
mod member_hints;
mod models;
mod native_bindings;
mod native_roots;
mod reactive;
pub(crate) mod snapshot;
mod text;
use crate::type_checks as types;
use builtins::Builtin;
use collections::{TableAction, TableWork};
#[cfg(test)]
mod callback_tests;
#[cfg(test)]
mod cancellation_tests;
#[cfg(test)]
mod exit_tests;
mod faults;
mod parameters;
#[cfg(test)]
mod review_tests;
pub use api::*;

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct FaultSeed {
    kind: FaultKind,
    location: Location,
    fault: Option<FaultId>,
    explicit: Option<FaultId>,
    author_value: bool,
    awaited: Option<(FaultId, u64)>,
}
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
enum BuildPhase {
    ErrorValue,
    Envelope,
    Stack(u32),
    Cause,
    Awaited,
    Complete,
}
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct FaultBuild {
    frame: FrameId,
    seed: FaultSeed,
    phase: BuildPhase,
    implicit: Option<FaultId>,
    event: u64,
}
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
enum ReturnTo {
    Generator {
        frame: FrameId,
        call: Pc,
        action: GeneratorAction,
    },
    Sort {
        frame: FrameId,
        call: Pc,
    },
    Class {
        frame: FrameId,
        call: Pc,
    },
    Delivery,
    Register {
        frame: FrameId,
        reg: Reg,
        call: Pc,
    },
    Default {
        frame: FrameId,
        parameter: u32,
        call: Pc,
    },
    Native {
        frame: FrameId,
    },
    NativeDefault {
        frame: FrameId,
        parameter: u32,
    },
    ReactiveRead {
        frame: FrameId,
        call: Pc,
    },
}
impl ReturnTo {
    fn caller(self) -> Option<(FrameId, Option<Pc>)> {
        match self {
            Self::Delivery => None,
            Self::Register { frame, call, .. }
            | Self::Default { frame, call, .. }
            | Self::Class { frame, call }
            | Self::Sort { frame, call }
            | Self::ReactiveRead { frame, call } => Some((frame, Some(call))),
            Self::Generator { frame, call, .. } => Some((frame, Some(call))),
            Self::Native { frame } | Self::NativeDefault { frame, .. } => Some((frame, None)),
        }
    }
}
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
enum ArgSource {
    Sort {
        frame: FrameId,
    },
    None,
    Await {
        frame: FrameId,
        value: Reg,
    },
    Caller {
        frame: FrameId,
        descriptor: DescriptorId,
    },
    CheckedCaller {
        frame: FrameId,
        descriptor: DescriptorId,
        contract: TypeId,
    },
    Native {
        frame: FrameId,
    },
    Prefix {
        frame: FrameId,
    },
}
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
enum ScriptPhase {
    GeneratorCreate,
    GeneratorCapture {
        part: u32,
        index: u32,
        created: bool,
    },
    GeneratorStart {
        action: GeneratorAction,
        call: Pc,
    },
    GeneratorRestore {
        part: u32,
        index: u32,
        closing: bool,
    },
    Search(builtins::SearchWork),
    Sort(builtins::SortWork),
    Class(classes::ClassStep),
    Copy {
        source: ArgSource,
        index: u32,
    },
    Rest,
    Bind(ArgumentBinding),
    Defaults {
        parameter: u32,
    },
    DefaultCall {
        parameter: u32,
        function: FunctionId,
    },
    Parameter {
        parameter: u32,
        storage: BindingStorage,
    },
    Install {
        binding: u32,
    },
    Body,
    Constant(constants::ConstantWork),
    DefaultConstant {
        parameter: u32,
        work: constants::ConstantWork,
    },
    Spread(collections::SpreadCapture),
    TableSpread(collections::TableSpread),
    KeyPath {
        index: u32,
    },
    TableValidate,
    TableScan,
    HostMethod {
        object: Reg,
        dest: Reg,
    },
    Builtin(builtins::BuiltinCall),
    Bulk(builtins::BulkWork),
    TextBuiltin(builtins::TextWork),
    Sequence {
        dest: Reg,
        window: RegWindow,
        index: u32,
        tuple: bool,
    },
    TextPush {
        builder: Reg,
        input: Reg,
        offset: u32,
    },
    TextFinish {
        dest: Reg,
        input: Reg,
        offset: u32,
        version: u64,
        utf8: text::Utf8,
    },
    Concat {
        dest: Reg,
        left: Reg,
        right: Reg,
        offset: u32,
    },
    Equality {
        dest: Reg,
        negated: bool,
    },
    EqualityCleanup {
        dest: Reg,
        negated: bool,
        result: Result<bool, FaultKind>,
    },
    Exit {
        origin: Pc,
        reason: ExitReason,
        skip: Option<RegionId>,
    },
    ExitApply {
        origin: Pc,
        reason: ExitReason,
        handler: Option<HandlerTarget>,
    },
    Release {
        reason: ExitReason,
    },
}
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
enum ExitReason {
    Return,
    GeneratorClose { logical_id: u64, activation: u64 },
    Throw(FaultSeed),
    Leave(Pc),
}
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct ScriptFrame {
    id: FrameId,
    code: CodeRef,
    module: ModuleInstanceId,
    pc: Pc,
    #[serde(skip)]
    instruction_end: Pc,
    #[serde(skip)]
    member_symbol: Option<SymbolId>,
    #[serde(skip)]
    instruction: Option<Instruction>,
    #[serde(skip)]
    instruction_pc: Pc,
    regs: RootRangeToken,
    env: RootRangeToken,
    args: RootRangeToken,
    bound: RootRangeToken,
    argument_count: u32,
    closure: bool,
    shared_parameters: bool,
    // Shared prefix Cells belong to the binder that prepared them, while this
    // frame's code, captures, locals and diagnostics retain its lexical module.
    parameter_module: ModuleInstanceId,
    effects: Effects,
    context: ContextFlags,
    owner: RootOwner,
    ret: ReturnTo,
    phase: ScriptPhase,
    table_work: Option<TableWork>,
    generator: bool,
}
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
enum NativePhase {
    PlannedCopy {
        source: ArgSource,
        index: u32,
    },
    Copy {
        source: ArgSource,
        index: u32,
    },
    Rest,
    Bind(ArgumentBinding),
    Defaults {
        parameter: u32,
    },
    Parameter {
        parameter: u32,
        storage: BindingStorage,
    },
    DefaultCall {
        parameter: u32,
        function: FunctionId,
    },
    DefaultConstant {
        parameter: u32,
        work: constants::ConstantWork,
    },
    Project {
        index: u32,
        input: NativeInput,
    },
    Issue(NativeInput),
    Issued {
        serial: u64,
        input: NativeInput,
    },
    Waiting(WaitId),
    Continue,
    Callback {
        count: u32,
    },
    Resume(NativeInput),
    CallbackFaultBuild,
    WaitingFaultBuild,
    Commit(bool),
    Release(bool),
}
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct NativeFrame {
    argument_plan: Option<u32>,
    id: FrameId,
    native: NativeId,
    signature: SignatureId,
    args: RootRangeToken,
    bound: RootRangeToken,
    bindings: Option<RootRangeToken>,
    module: ModuleInstanceId,
    has_defaults: bool,
    roots: RootRangeToken,
    argument_count: u32,
    receiver: bool,
    continuation_len: u32,
    continuation_active: bool,
    continuation_version: u64,
    continuation_stage: u64,
    effects: Effects,
    context: ContextFlags,
    owner: RootOwner,
    location: Location,
    ret: ReturnTo,
    callback_fault: Option<FaultId>,
    derived_output: Option<Value>,
    fault_count: u32,
    pending_fault: Option<FaultSeed>,
    phase: NativePhase,
    await_activation: Option<u64>,
}
impl NativeFrame {
    fn root_count(self) -> u64 {
        3 + u64::from(self.bindings.is_some())
    }
}
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
enum Frame {
    Script(ScriptFrame),
    Native(NativeFrame),
}
impl Frame {
    fn id(self) -> FrameId {
        match self {
            Self::Script(s) => s.id,
            Self::Native(n) => n.id,
        }
    }
}
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct Unwind {
    frame: FrameId,
    origin: Pc,
    reason: ExitReason,
    region: RegionId,
    value: RootRangeToken,
    replacement_event: Option<u64>,
}
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct Terminal {
    id: DeliveryId,
    fault: Option<FaultSeed>,
}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct CaughtFault {
    frame: FrameId,
    region: RegionId,
    info: FaultSeed,
    activation: u64,
}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
enum CallbackBoundary {
    Cancelled(FrameId),
    Returned(FrameId),
}
impl CallbackBoundary {
    fn frame(self) -> FrameId {
        match self {
            Self::Cancelled(frame) | Self::Returned(frame) => frame,
        }
    }
}

/// One shared heap/module set, with independently owned task state machines.
struct TaskState {
    frames: ChargedVec<Frame>,
    unwinds: ChargedVec<Unwind>,
    caught: ChargedVec<CaughtFault>,
    equality_cursors: ChargedVec<u32>,
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
    model_copy: Option<models::ModelCopy>,
}
impl TaskState {
    fn new(
        usage: &HeapBudgetUsage,
        heap: &mut Heap,
        slots: &mut FrameSlots,
        limits: VmLimits,
        pooled: bool,
    ) -> Result<Self, VmError> {
        let cap = if pooled {
            limits.max_frames as usize
        } else {
            0
        };
        let regions = if pooled {
            limits.max_unwinds as usize
        } else {
            0
        };
        let frames = ChargedVec::new(usage, cap)?;
        let unwinds = ChargedVec::new(usage, regions)?;
        let caught = ChargedVec::new(usage, regions)?;
        let equality_cursors = ChargedVec::new(usage, regions)?;
        let equality_roots = if pooled {
            Some(slots.open(
                heap,
                RootOwner(u64::MAX),
                RootKind::Work,
                limits.max_unwinds.checked_mul(2).ok_or(VmError::Limit)?,
            )?)
        } else {
            None
        };
        let delivery = if pooled {
            match slots.open(heap, RootOwner(u64::MAX), RootKind::Delivery, 2) {
                Ok(range) => Some(range),
                Err(error) => {
                    slots.release(heap, equality_roots.unwrap())?;
                    return Err(error.into());
                }
            }
        } else {
            None
        };
        Ok(Self {
            frames,
            unwinds,
            caught,
            equality_cursors,
            equality_roots,
            delivery,
            terminal: None,
            cancelling: false,
            cancel_boundary: None,
            task_owner: None,
            task_id: None,
            root_owner: RootOwner(u64::MAX),
            fault_build: None,
            blocked: None,
            cooperative_cancel: false,
            abort_equality: false,
            model_copy: None,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NativeParameters {
    needs_cells: bool,
    has_defaults: bool,
}

/// Task selection swaps bounded state owners; all registered windows remain in
/// the single FrameSlots provider. Field order deallocates before budget refunds.
pub struct Vm {
    program: Rc<CodeLease>,
    code_window: crate::bytecode::CodeWindow,
    slots: FrameSlots,
    active: TaskState,
    tasks: ChargedVec<TaskState>,
    // Serial order makes identity queries independent of the pooled task layout.
    task_index: ChargedVec<(u64, usize)>,
    active_slot: Option<usize>,
    wait_revision: u64,
    scratch: ChargedVec<Value>,
    text_scratch: ChargedVec<u8>,
    native_parameters: ChargedVec<NativeParameters>,
    native_values: ChargedVec<(FunctionId, u32)>,
    type_checks: Rc<types::TypeChecks>,
    init_symbol: Option<SymbolId>,
    globals: RootRangeToken,
    constants: RootRangeToken,
    constant_cache: ChargedVec<Value>,
    member_hints: member_hints::MemberHints,
    native_bindings: native_bindings::NativeBindings,
    heap: Heap,
    limits: VmLimits,
    control_charge: Reservation,
    next_id: u64,
    native_entries: u64,
    native_binding_hits: u64,
    native_binding_misses: u64,
    native_resumes: u64,
    native_null_publications: u64,
    slices: u64,
    last_usage: SliceUsage,
    #[cfg(test)]
    work_probe: WorkProbe,
}
#[cfg(test)]
#[derive(Default)]
struct WorkProbe {
    release_catch_checks: std::cell::Cell<u64>,
    native_layout_queries: std::cell::Cell<u64>,
    exit_handler_queries: std::cell::Cell<u64>,
    member_hint_hits: std::cell::Cell<u64>,
}
mod completion;
mod instructions;
mod loading;
mod native_calls;
mod script_frames;
mod script_step;
mod task_control;

impl Vm {
    pub fn program(&self) -> &VerifiedProgram {
        &self.program
    }
    pub fn limits(&self) -> VmLimits {
        self.limits
    }
    /// Slice count and quota usage recorded by the most recent `run_slice` call.
    /// Reads stored counters in constant time without traversing tasks.
    #[inline]
    pub fn last_slice_usage(&self) -> (u64, SliceUsage) {
        (self.slices, self.last_usage)
    }
    pub fn task_id(&self) -> Option<TaskId> {
        self.active
            .task_id
            .or_else(|| self.tasks.data.iter().find_map(|t| t.task_id))
    }
    /// Current instruction or native call site, available without allocating a
    /// fault record. A task without execution frames has no current location.
    pub fn task_location(&self, task: TaskId) -> Result<Option<Location>, VmError> {
        Ok(self
            .task(task)?
            .frames
            .data
            .last()
            .map(|frame| match frame {
                Frame::Script(script) => self.loc(*script, script.pc),
                Frame::Native(native) => native.location,
            }))
    }
    pub fn is_idle(&self) -> bool {
        self.active.task_id.is_none() && self.tasks.data.iter().all(|t| t.task_id.is_none())
    }
    pub fn heap(&self) -> &Heap {
        &self.heap
    }
    /// Allocation/mutation access for host results. Collection must use Vm::gc_step.
    pub fn heap_mut(&mut self) -> &mut Heap {
        &mut self.heap
    }
    pub fn begin_collection(&mut self) -> Result<(), HeapError> {
        self.heap.begin_collection()
    }
    pub fn gc_step(&mut self, budget: GcBudget) -> Result<GcProgress, HeapError> {
        self.heap.gc_step_with_ranges(budget, self.slots.roots())
    }
    pub fn stats(&self) -> VmStats {
        VmStats {
            frames: (self.active.frames.data.len()
                + self
                    .tasks
                    .data
                    .iter()
                    .map(|t| t.frames.data.len())
                    .sum::<usize>()) as u32,
            unwinds: (self.active.unwinds.data.len()
                + self
                    .tasks
                    .data
                    .iter()
                    .map(|t| t.unwinds.data.len())
                    .sum::<usize>()) as u32,
            comparison_depth: (self.active.equality_cursors.data.len()
                + self
                    .tasks
                    .data
                    .iter()
                    .map(|t| t.equality_cursors.data.len())
                    .sum::<usize>()) as u32,
            native_entries: self.native_entries,
            native_binding_hits: self.native_binding_hits,
            native_binding_misses: self.native_binding_misses,
            native_resumes: self.native_resumes,
            native_null_publications: self.native_null_publications,
            slices: self.slices,
            usage: self.last_usage,
            heap: self.heap.stats(),
            slots: self.slots.stats(),
            program_bytes: self.program.charge.bytes(),
            control_bytes: self.control_charge.bytes(),
        }
    }
    fn serial(&mut self) -> Result<u64, VmError> {
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).ok_or(VmError::Limit)?;
        Ok(id)
    }
    fn index(&self, id: FrameId) -> Result<usize, VmError> {
        // Explicit stack return/argument targets are the current frame or its
        // immediate caller. No per-register or per-call map/linear search.
        let len = self.active.frames.data.len();
        if len > 0 && self.active.frames.data[len - 1].id() == id {
            return Ok(len - 1);
        }
        if len > 1 && self.active.frames.data[len - 2].id() == id {
            return Ok(len - 2);
        }
        Err(VmError::Contract("missing immediate caller"))
    }
    fn loc(&self, s: ScriptFrame, pc: Pc) -> Location {
        Location {
            code: s.code,
            pc,
            source: self.program.source_at(s.code.function, pc),
        }
    }
    fn binding_location(&self, s: ScriptFrame) -> Location {
        let mut current = s;
        let mut index = self.index(s.id).unwrap_or(0);
        loop {
            match current.ret {
                ReturnTo::Default { frame, .. } => {
                    if index > 0
                        && let Frame::Script(parent) = self.active.frames.data[index - 1]
                        && parent.id == frame
                    {
                        current = parent;
                        index -= 1;
                    } else {
                        return self.loc(current, current.pc);
                    }
                }
                ReturnTo::Register { frame, call, .. }
                | ReturnTo::Class { frame, call }
                | ReturnTo::Sort { frame, call }
                | ReturnTo::ReactiveRead { frame, call } => {
                    return match index.checked_sub(1).map(|i| self.active.frames.data[i]) {
                        Some(Frame::Script(parent)) if parent.id == frame => self.loc(parent, call),
                        _ => self.loc(current, current.pc),
                    };
                }
                ReturnTo::Generator { frame, call, .. } => {
                    return match index.checked_sub(1).map(|i| self.active.frames.data[i]) {
                        Some(Frame::Script(parent)) if parent.id == frame => self.loc(parent, call),
                        _ => self.loc(current, current.pc),
                    };
                }
                ReturnTo::Native { frame } | ReturnTo::NativeDefault { frame, .. } => {
                    return match index.checked_sub(1).map(|i| self.active.frames.data[i]) {
                        Some(Frame::Native(n)) if n.id == frame => n.location,
                        _ => self.loc(current, current.pc),
                    };
                }
                ReturnTo::Delivery => return self.loc(current, current.pc),
            }
        }
    }
    fn contract(&self, module: ModuleId, ty: TypeId, mutable: bool) -> CellContract {
        let kind = match self.program.package().types[ty.index()] {
            TypeDef::Builtin(kind) => kind,
            _ => BuiltinType::Any,
        };
        CellContract {
            mutable,
            ty: kind,
            type_id: Some(ty),
            module,
            code_version: self.program.package().content_version,
            module_version: self.program.package().content_version,
        }
    }
    fn check_type(&self, ty: TypeId, value: Value) -> Result<(), FaultKind> {
        if self
            .type_checks
            .accepts(self.program.package(), &self.heap, ty, value)
            .map_err(heap_fault)?
        {
            Ok(())
        } else {
            Err(FaultKind::Type)
        }
    }
    fn read(&self, regs: RootRangeToken, reg: Reg) -> Result<Value, FaultKind> {
        self.slots.read(at(regs, reg.0)).map_err(heap_fault)
    }
    fn write(&mut self, regs: RootRangeToken, reg: Reg, v: Value) -> Result<(), FaultKind> {
        self.slots
            .write(&mut self.heap, at(regs, reg.0), v)
            .map_err(heap_fault)
    }
    fn value(&self, r: RootRangeToken, i: u32) -> Result<Value, VmError> {
        Ok(self.slots.read(at(r, i))?)
    }
    fn put(&mut self, r: RootRangeToken, i: u32, v: Value) -> Result<(), VmError> {
        Ok(self.slots.write(&mut self.heap, at(r, i), v)?)
    }
    fn clear(&mut self, r: RootRangeToken, i: u32) -> Result<(), VmError> {
        Ok(self.slots.clear(&mut self.heap, at(r, i))?)
    }
    fn open(
        &mut self,
        _owner: RootOwner,
        kind: RootKind,
        len: u32,
    ) -> Result<RootRangeToken, VmError> {
        Ok(self
            .slots
            .open(&mut self.heap, self.active.root_owner, kind, len)?)
    }
    fn release_cost(&self, count: u64) -> u64 {
        // Each release can add one span before binary search and up to three
        // Vec shifts/coalesces. Include growth across this entire transaction.
        count * (4 * (self.slots.stats().free_spans as u64 + count) + 8)
    }
    fn window_push_cost(&self, windows: u64, values: u64) -> (u64, u64) {
        let spans = self.slots.stats().free_spans as u64;
        let rollback = windows - 1;
        let admission = self
            .slots
            .window_admission(&self.heap, windows as usize, values);
        // Open scans/removes from the current free list, fills each Value and
        // clears bitmap words with masked boundaries. A later failure can roll
        // back earlier opens; those releases can grow the free list even when
        // success would not.
        (
            values
                + values.div_ceil(64)
                + 8 * windows
                + windows * (3 * spans + 8)
                + rollback * (4 * (spans + rollback) + 8)
                + admission.work,
            admission.allocation_bytes,
        )
    }
    fn script_push_cost(&self, function: FunctionId, count: u32) -> (u64, u64) {
        let f = &self.program.package().functions[function.index()];
        if let FunctionKind::Native(native) = f.kind {
            // A callable may carry one prebound receiver root.
            let (work, bytes) = self.native_push_cost(native, count);
            return (work + 1, bytes);
        }
        let values = f.register_count as u64
            + 4
            + u64::from(f.kind == FunctionKind::Generator)
            + 2 * f.regions.len() as u64
            + count as u64 * 2
            + self.program.package().signatures[f.signature.index()]
                .parameters
                .len() as u64
            + u64::from(
                self.program.package().signatures[f.signature.index()]
                    .rest
                    .is_some(),
            );
        let (work, bytes) = self.window_push_cost(4, values);
        (work + 1, bytes)
    }
    fn native_push_cost(&self, native: NativeId, count: u32) -> (u64, u64) {
        let signature = self.program.package().natives[native.index()].signature;
        let sig = &self.program.package().signatures[signature.index()];
        let cells = self.native_parameters.data[native.index()].needs_cells;
        let parameters = sig.parameters.len() as u64 + u64::from(sig.rest.is_some());
        let values = 3 + count as u64 * 2 + parameters * (1 + u64::from(cells));
        let (work, bytes) = self.window_push_cost(3 + u64::from(cells), values);
        (work + 3, bytes)
    }

    pub fn run_slice(
        &mut self,
        task: TaskId,
        budget: SliceBudget,
        mut now: impl FnMut() -> u64,
    ) -> Result<RunOutcome, VmError> {
        self.select_task(task)?;
        self.slices += 1;
        let mut meter = Meter {
            budget,
            usage: SliceUsage::default(),
            clock_steps: 0,
        };
        let result = self.run(&mut meter, &mut now);
        self.code_window = crate::bytecode::CodeWindow::default();
        self.last_usage = meter.usage;
        result
    }
    fn run(&mut self, m: &mut Meter, now: &mut impl FnMut() -> u64) -> Result<RunOutcome, VmError> {
        self.active.blocked = None;
        loop {
            if let Some(blocked) = self.active.blocked {
                return Ok(RunOutcome::ResourceBlocked(blocked));
            }
            if let Some(t) = self.active.terminal {
                return Ok(if t.fault.is_some() {
                    RunOutcome::Fault(t.id)
                } else {
                    RunOutcome::Returned(t.id)
                });
            }
            if m.deadline_reached(now) {
                return Ok(RunOutcome::BudgetYield(m.usage));
            }
            if self.active.cancelling {
                let boundary = self.active.cancel_boundary.map(CallbackBoundary::frame);
                if self.active.caught.data.last().is_some_and(|caught| {
                    boundary.is_none_or(|boundary| caught.frame.0 > boundary.0)
                }) {
                    if !m.take(0, 1, 0) {
                        return Ok(RunOutcome::BudgetYield(m.usage));
                    }
                    self.active.caught.data.pop();
                    continue;
                }
                if !self.active.equality_cursors.data.is_empty() {
                    if !m.take(0, 1, 0) {
                        return Ok(RunOutcome::BudgetYield(m.usage));
                    }
                    let depth = self.active.equality_cursors.data.len() as u32 - 1;
                    self.clear(self.active.equality_roots.unwrap(), depth * 2)?;
                    self.clear(self.active.equality_roots.unwrap(), depth * 2 + 1)?;
                    self.active.equality_cursors.data.pop();
                    continue;
                }
                if let Some(u) = self.active.unwinds.data.last().copied()
                    && boundary.is_none_or(|boundary| u.frame.0 > boundary.0)
                {
                    if !m.take(0, self.release_cost(1), 0) {
                        return Ok(RunOutcome::BudgetYield(m.usage));
                    }
                    self.slots.release(&mut self.heap, u.value)?;
                    self.active.unwinds.data.pop();
                    continue;
                }
                if let Some(f) = self.active.frames.data.last().copied() {
                    if boundary == Some(f.id()) {
                        if !m.take(0, 4, 0) {
                            return Ok(RunOutcome::BudgetYield(m.usage));
                        }
                        let Frame::Native(mut native) = f else {
                            unreachable!("callback boundary is a native frame")
                        };
                        let input = match self.active.cancel_boundary.unwrap() {
                            CallbackBoundary::Returned(_) => NativeInput::CallbackReturned,
                            CallbackBoundary::Cancelled(_) => {
                                self.put(native.roots, 0, Value::NULL)?;
                                NativeInput::CallbackCancelled
                            }
                        };
                        self.clear_native_pending(native.roots)?;
                        native.callback_fault = None;
                        native.pending_fault = None;
                        native.phase = NativePhase::Resume(input);
                        *self.active.frames.data.last_mut().unwrap() = Frame::Native(native);
                        self.active.cancelling = false;
                        self.active.cancel_boundary = None;
                        continue;
                    }
                    let count = match f {
                        Frame::Script(_) => 4,
                        Frame::Native(n) => n.root_count(),
                    };
                    if !m.take(0, self.release_cost(count), 0) {
                        return Ok(RunOutcome::BudgetYield(m.usage));
                    }
                    self.release_frame(f)?;
                    self.active.frames.data.pop();
                    continue;
                }
                self.active.cancelling = false;
                self.active.task_owner = None;
                self.forget_active_task();
                return Err(VmError::Idle);
            }
            if self.active.fault_build.is_some() {
                if !self.fault_build_step(m)? {
                    return Ok(RunOutcome::BudgetYield(m.usage));
                }
                continue;
            }
            if self.active.abort_equality {
                if !m.take(0, 3, 0) {
                    return Ok(RunOutcome::BudgetYield(m.usage));
                }
                if !self.active.equality_cursors.data.is_empty() {
                    let depth = self.active.equality_cursors.data.len() as u32 - 1;
                    self.clear(self.active.equality_roots.unwrap(), depth * 2)?;
                    self.clear(self.active.equality_roots.unwrap(), depth * 2 + 1)?;
                    self.active.equality_cursors.data.pop();
                } else {
                    self.active.abort_equality = false;
                }
                continue;
            }
            if self.active.cooperative_cancel {
                if !m.take(
                    0,
                    2 * self.active.frames.data.len() as u64 + self.program_lookup_work() + 3,
                    0,
                ) {
                    return Ok(RunOutcome::BudgetYield(m.usage));
                }
                self.seed_cancellation()?;
                self.active.cooperative_cancel = false;
                continue;
            }
            if let Some((frame, seed)) = self.unbuilt_fault() {
                if !m.take(0, self.active.caught.data.len() as u64 + 1, 0) {
                    return Ok(RunOutcome::BudgetYield(m.usage));
                }
                let implicit = if seed.explicit.is_none() {
                    self.active
                        .caught
                        .data
                        .iter()
                        .max_by_key(|c| c.activation)
                        .and_then(|c| c.info.fault)
                } else {
                    None
                };
                self.active.fault_build = Some(FaultBuild {
                    frame,
                    seed,
                    phase: BuildPhase::ErrorValue,
                    implicit,
                    event: self.serial()?,
                });
                continue;
            }
            let Some(f) = self.active.frames.data.last().copied() else {
                return Err(VmError::Idle);
            };
            match f {
                Frame::Script(s) => {
                    if !self.script_step(s, m)? {
                        return Ok(RunOutcome::BudgetYield(m.usage));
                    }
                }
                Frame::Native(n) => {
                    if let Some(out) = self.native_step(n, m)? {
                        return Ok(out);
                    }
                }
            }
        }
    }
    fn commit_work(&self, ret: ReturnTo) -> u64 {
        match ret {
            ReturnTo::Native { frame } => {
                match self.index(frame).ok().map(|i| self.active.frames.data[i]) {
                    Some(Frame::Native(n)) => {
                        2 * (n.fault_count as u64 + n.continuation_len as u64) + 8
                    }
                    _ => 8,
                }
            }
            _ => 2,
        }
    }
    fn release_frame(&mut self, f: Frame) -> Result<(), VmError> {
        match f {
            Frame::Script(s) => {
                if s.generator {
                    self.generator_abandon(s)?;
                }
                self.abort_class(s)?;
                for r in [s.regs, s.env, s.args, s.bound] {
                    self.slots.release(&mut self.heap, r)?;
                }
            }
            Frame::Native(n) => {
                if let Some(output) = n.derived_output {
                    self.heap.end_derived(output)?;
                }
                if self
                    .active
                    .model_copy
                    .as_ref()
                    .is_some_and(|copy| copy.frame == n.id)
                {
                    self.active.model_copy = None;
                }
                if let Some(r) = n.bindings {
                    self.slots.release(&mut self.heap, r)?;
                }
                for r in [n.args, n.bound, n.roots] {
                    self.slots.release(&mut self.heap, r)?;
                }
            }
        }
        Ok(())
    }
    fn preparation_fault(&mut self, s: &mut ScriptFrame, kind: FaultKind) -> Result<(), VmError> {
        self.put(s.env, 1, Value::NULL)?;
        self.clear(s.env, 2)?;
        s.phase = ScriptPhase::Release {
            reason: ExitReason::Throw(FaultSeed {
                fault: None,
                explicit: None,
                author_value: false,
                awaited: None,
                kind,
                location: self.binding_location(*s),
            }),
        };
        Ok(())
    }
    fn native_fault(&mut self, n: &mut NativeFrame, kind: FaultKind) -> Result<(), VmError> {
        self.put(n.roots, 0, Value::NULL)?;
        self.clear_native_pending(n.roots)?;
        n.pending_fault = Some(FaultSeed {
            fault: None,
            explicit: None,
            author_value: false,
            awaited: None,
            kind,
            location: n.location,
        });
        n.phase = NativePhase::Commit(true);
        Ok(())
    }
}
fn at(range: RootRangeToken, index: u32) -> RangePosition {
    RangePosition { range, index }
}
fn inherit(parent: ContextFlags, child: Context) -> ContextFlags {
    let c = ContextFlags::from(child);
    ContextFlags {
        no_wait: parent.no_wait || c.no_wait,
        init: parent.init || c.init,
        derived: parent.derived || c.derived,
    }
}
fn no_wait(context: ContextFlags) -> bool {
    context.no_wait || context.init || context.derived
}
fn heap_fault(e: HeapError) -> FaultKind {
    match e {
        HeapError::UninitializedSlot | HeapError::UninitializedCell => FaultKind::Uninitialized,
        HeapError::UninitializedField => FaultKind::UninitializedField,
        HeapError::ConstructionFailed => FaultKind::ConstructionFailed,
        HeapError::ReadOnly => FaultKind::ReadOnly,
        HeapError::TypeMismatch | HeapError::WrongKind => FaultKind::Type,
        HeapError::AlreadyInitialized => FaultKind::AlreadyInitialized,
        HeapError::InvalidHandle | HeapError::RetiredHost | HeapError::BindingCycle => {
            FaultKind::InvalidState
        }
        _ => FaultKind::Resource,
    }
}
fn language_fault(e: VmError) -> Result<FaultKind, VmError> {
    match e {
        VmError::Effect => Ok(FaultKind::Effect),
        VmError::Limit => Ok(FaultKind::Resource),
        VmError::Contract("invalid argument name") => Ok(FaultKind::Type),
        VmError::Heap(
            h @ (HeapError::UninitializedSlot
            | HeapError::UninitializedField
            | HeapError::ConstructionFailed
            | HeapError::UninitializedCell
            | HeapError::AlreadyInitialized
            | HeapError::ReadOnly
            | HeapError::TypeMismatch
            | HeapError::WrongKind
            | HeapError::OutOfBounds
            | HeapError::ObjectCapacity
            | HeapError::RootCapacity
            | HeapError::RootRangeCapacity
            | HeapError::WindowCapacity
            | HeapError::AllocationFailure
            | HeapError::OperationLimit { .. }
            | HeapError::ResourceLimit { .. }),
        ) => Ok(heap_fault(h)),
        other => Err(other),
    }
}
fn vm_fault(e: VmError) -> FaultKind {
    match e {
        VmError::Effect => FaultKind::Effect,
        VmError::Heap(e) => heap_fault(e),
        _ => FaultKind::Resource,
    }
}
struct Meter {
    budget: SliceBudget,
    usage: SliceUsage,
    clock_steps: u8,
}
impl Meter {
    fn deadline_reached(&mut self, now: &mut impl FnMut() -> u64) -> bool {
        let Some(deadline) = self.budget.deadline_ns else {
            return false;
        };
        if self.clock_steps != 0 {
            self.clock_steps -= 1;
            return false;
        }
        self.clock_steps = 31;
        now() >= deadline
    }
    fn admits(&mut self, instructions: u32, work: u64, allocation: u64) -> bool {
        self.usage.required_work = work;
        self.usage.required_allocation = allocation;
        instructions <= self.budget.instructions - self.usage.instructions
            && work <= self.budget.work - self.usage.work
            && allocation <= self.budget.allocation_bytes - self.usage.allocation_bytes
    }
    fn take(&mut self, instructions: u32, work: u64, allocation: u64) -> bool {
        if !self.admits(instructions, work, allocation) {
            return false;
        }
        self.usage.instructions += instructions;
        self.usage.work += work;
        self.usage.allocation_bytes += allocation;
        // Allocations and bulk work end the current clock batch immediately.
        if allocation != 0 || work >= 32 {
            self.clock_steps = 0;
        }
        self.usage.required_work = 0;
        self.usage.required_allocation = 0;
        true
    }
}
fn unary(op: Opcode, v: Value) -> Result<Value, FaultKind> {
    Ok(match op {
        Opcode::Not => Value::bool(!v.as_bool().ok_or(FaultKind::Type)?),
        Opcode::BitNot => Value::int(!v.as_int().ok_or(FaultKind::Type)?),
        Opcode::Pos if matches!(v.tag(), ValueTag::Int | ValueTag::Float) => v,
        Opcode::Neg => {
            if let Some(i) = v.as_int() {
                Value::int(i.checked_neg().ok_or(FaultKind::Overflow)?)
            } else {
                Value::float(-v.as_float().ok_or(FaultKind::Type)?)
            }
        }
        _ => return Err(FaultKind::Type),
    })
}
// Compare int/float without first rounding the int to float. The cast only runs
// in the finite i64 domain; a fractional tail is compared after the integer part.
fn int_float(i: i64, f: f64) -> Option<Ordering> {
    if f.is_nan() {
        return None;
    }
    if f >= 9223372036854775808.0 {
        return Some(Ordering::Less);
    }
    if f < -9223372036854775808.0 {
        return Some(Ordering::Greater);
    }
    let trunc = f as i64;
    match i.cmp(&trunc) {
        Ordering::Equal => (i as f64).partial_cmp(&f),
        other => Some(other),
    }
}
fn compare(a: Value, b: Value) -> Option<Ordering> {
    match (a.as_int(), b.as_int(), a.as_float(), b.as_float()) {
        (Some(x), Some(y), _, _) => Some(x.cmp(&y)),
        (Some(x), _, _, Some(y)) => int_float(x, y),
        (_, Some(y), Some(x), _) => int_float(y, x).map(Ordering::reverse),
        (_, _, Some(x), Some(y)) => x.partial_cmp(&y),
        _ => None,
    }
}
fn binary(op: Opcode, a: Value, b: Value) -> Result<Value, FaultKind> {
    use Opcode::*;
    if matches!(op, Eq | Ne) {
        let equal = if matches!(a.tag(), ValueTag::Int | ValueTag::Float)
            && matches!(b.tag(), ValueTag::Int | ValueTag::Float)
        {
            compare(a, b) == Some(Ordering::Equal)
        } else {
            a.same_bits(b)
        };
        return Ok(Value::bool(if op == Eq { equal } else { !equal }));
    }
    if matches!(op, Lt | Le | Gt | Ge) {
        if !matches!(a.tag(), ValueTag::Int | ValueTag::Float)
            || !matches!(b.tag(), ValueTag::Int | ValueTag::Float)
        {
            return Err(FaultKind::Type);
        }
        let order = compare(a, b);
        return Ok(Value::bool(match op {
            Lt => order == Some(Ordering::Less),
            Le => matches!(order, Some(Ordering::Less | Ordering::Equal)),
            Gt => order == Some(Ordering::Greater),
            Ge => matches!(order, Some(Ordering::Greater | Ordering::Equal)),
            _ => unreachable!(),
        }));
    }
    if let (Some(x), Some(y)) = (a.as_int(), b.as_int()) {
        return Ok(Value::int(match op {
            Add => x.checked_add(y).ok_or(FaultKind::Overflow)?,
            Sub => x.checked_sub(y).ok_or(FaultKind::Overflow)?,
            Mul => x.checked_mul(y).ok_or(FaultKind::Overflow)?,
            Div | Rem => {
                if y == 0 {
                    return Err(FaultKind::DivisionByZero);
                }
                if x == i64::MIN && y == -1 {
                    return Err(FaultKind::Overflow);
                }
                if op == Div { x / y } else { x % y }
            }
            BitAnd => x & y,
            BitOr => x | y,
            BitXor => x ^ y,
            Shl | Shr => {
                if !(0..64).contains(&y) {
                    return Err(FaultKind::Type);
                }
                if op == Shl {
                    x.wrapping_shl(y as u32)
                } else {
                    x >> y
                }
            }
            _ => return Err(FaultKind::Type),
        }));
    }
    let numeric = |v: Value| {
        v.as_float()
            .or_else(|| v.as_int().map(|n| n as f64))
            .ok_or(FaultKind::Type)
    };
    let (x, y) = (numeric(a)?, numeric(b)?);
    Ok(Value::float(match op {
        Add => x + y,
        Sub => x - y,
        Mul => x * y,
        Div | Rem => {
            if y == 0.0 {
                return Err(FaultKind::DivisionByZero);
            }
            if op == Div { x / y } else { x % y }
        }
        _ => return Err(FaultKind::Type),
    }))
}

#[cfg(test)]
fn test_pc(vm: &Vm, function: u32, ordinal: u32) -> Pc {
    let code = &vm.program.package().functions[function as usize].code;
    Pc(code.indexed_iter().nth(ordinal as usize).map_or_else(
        || {
            assert_eq!(ordinal as usize, code.len());
            code.end_pc()
        },
        |item| item.unwrap().0,
    ) as u32)
}

#[cfg(test)]
mod native_plan_tests;
