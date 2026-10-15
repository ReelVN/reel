//! Budgeted execution of the single shared verified-bytecode format.
//!
//! Values in frames, continuations and terminal deliveries are held in FrameSlots.
//! Host entry and continuation-resume tickets are distinct, single-use identities.
use crate::bytecode::*;
use crate::fault::{CodeLease, FaultData};
use crate::root_ranges::{ChargedVec, Reservation};
use crate::*;
use std::cmp::Ordering;
use std::rc::Rc;

mod api;
mod arguments;
use arguments::{ArgumentBinding, ArgumentStep, ParameterNames};
mod awaitables;
mod builtins;
mod calls;
mod classes;
use calls::NativeSite;
#[cfg(test)]
mod collection_tests;
mod collections;
mod iterators;
mod models;
mod reactive;
pub(crate) mod snapshot;
mod text;
use crate::type_checks as types;
use builtins::Builtin;
use collections::{TableAction, TableWork};
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
}
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
enum ArgSource {
    None,
    Await {
        frame: FrameId,
        value: Reg,
    },
    Caller {
        frame: FrameId,
        descriptor: DescriptorId,
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
    Spread(collections::SpreadCapture),
    TableSpread(collections::TableSpread),
    KeyPath {
        index: u32,
    },
    TableValidate,
    TableScan,
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
    Throw(FaultSeed),
    Leave(Pc),
}
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct ScriptFrame {
    id: FrameId,
    code: CodeRef,
    module: ModuleInstanceId,
    pc: Pc,
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
}
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
enum NativePhase {
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
/// Task selection swaps bounded state owners; all registered windows remain in
/// the single FrameSlots provider. Field order deallocates before budget refunds.
pub struct Vm {
    program: Rc<CodeLease>,
    slots: FrameSlots,
    active: TaskState,
    tasks: ChargedVec<TaskState>,
    active_slot: Option<usize>,
    scratch: ChargedVec<Value>,
    text_scratch: ChargedVec<u8>,
    native_cells: ChargedVec<bool>,
    native_values: ChargedVec<(FunctionId, u32)>,
    type_checks: Rc<types::TypeChecks>,
    init_symbol: Option<SymbolId>,
    globals: RootRangeToken,
    constants: RootRangeToken,
    heap: Heap,
    limits: VmLimits,
    control_charge: Reservation,
    next_id: u64,
    native_entries: u64,
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
}
impl Vm {
    pub fn load(
        program: VerifiedProgram,
        support: RuntimeSupport,
        limits: VmLimits,
        epoch: SessionEpoch,
    ) -> Result<Self, VmError> {
        Self::load_initial(program, support, limits, epoch, true)
    }
    fn load_initial(
        program: VerifiedProgram,
        mut support: RuntimeSupport,
        limits: VmLimits,
        epoch: SessionEpoch,
        initialize: bool,
    ) -> Result<Self, VmError> {
        support.opcodes = SUPPORTED_OPCODES.to_vec();
        program.require_runtime(&support)?;
        let p = program.package();
        if limits.max_frames == 0
            || limits.max_unwinds == 0
            || limits.max_tasks == 0
            || limits
                .max_arguments
                .checked_add(limits.max_continuation_values)
                .and_then(|n| n.checked_add(3))
                .and_then(|n| n.checked_add(limits.max_native_faults))
                .is_none_or(|n| n > limits.slots.max_window_len)
            || limits
                .max_unwinds
                .checked_mul(2)
                .is_none_or(|n| n > limits.slots.max_window_len)
            || limits.heap.max_root_ranges < 4
            || limits.max_native_faults == 0
        {
            return Err(VmError::Limit);
        }
        // Load rejects unsupported operand semantics as well as unsupported opcodes.
        for sig in &p.signatures {
            if sig.property_bag_tail.is_some()
                || sig.parameters.len() + usize::from(sig.rest.is_some())
                    > limits.max_arguments as usize
            {
                return Err(VmError::Contract(
                    "property bag or excessive signature is not supported",
                ));
            }
        }
        for f in &p.functions {
            if !matches!(f.kind, FunctionKind::Plain | FunctionKind::Native(_)) {
                return Err(VmError::Contract(
                    "async/generator handler is not connected",
                ));
            }
            if f.register_count > limits.slots.max_window_len
                || f.regions.len() as u64 * 2 + 4 > limits.slots.max_window_len as u64
            {
                return Err(VmError::Limit);
            }
        }
        let mut heap = Heap::new(epoch, limits.heap)?;
        let usage = heap.budget_usage();
        let type_checks = types::TypeChecks::new(p, &usage)?;
        let program_charge = Reservation::new(
            &usage,
            program.stats().owned_capacity_bytes as u64
                + (std::mem::size_of::<CodeLease>() - std::mem::size_of::<BytecodePackage>()
                    + std::mem::size_of::<types::TypeChecks>()
                    + 4 * std::mem::size_of::<usize>()) as u64,
        )?;
        let native_count = p
            .functions
            .iter()
            .filter(|f| matches!(f.kind, FunctionKind::Native(_)))
            .count();
        let module_slots = p.modules.iter().fold(
            p.constants
                .len()
                .checked_add(p.symbols.len())
                .and_then(|n| n.checked_add(native_count)),
            |total, module| total.and_then(|n| n.checked_add(module.globals.len())),
        );
        let module_slots =
            u32::try_from(module_slots.ok_or(VmError::Limit)?).map_err(|_| VmError::Limit)?;
        let execution = limits.slots;
        if execution.initial_capacity == 0
            || execution.initial_capacity > execution.max_capacity
            || execution.max_window_len == 0
            || execution.max_window_len > execution.max_capacity
        {
            return Err(VmError::Limit);
        }
        // Module roots are permanent for this package. Reserving their actual
        // size keeps story length from consuming the execution working set.
        let mut slots = FrameSlots::new(
            &mut heap,
            FrameSlotsLimits {
                initial_capacity: execution
                    .initial_capacity
                    .checked_add(module_slots)
                    .ok_or(VmError::Limit)?,
                max_capacity: execution.max_capacity.saturating_add(module_slots),
                max_window_len: execution.max_window_len,
            },
        )?;
        let control_bytes = std::mem::size_of::<Self>()
            - std::mem::size_of::<Heap>()
            - std::mem::size_of::<FrameSlots>();
        let control_charge = Reservation::new(&usage, control_bytes as u64)?;
        // B01's shared-signature query traverses a binary table. Do it once at
        // load, with owned capacity charged, so slice admission is O(1).
        let mut native_cells = ChargedVec::new(&usage, p.natives.len())?;
        for index in 0..p.natives.len() {
            native_cells
                .data
                .push(program.native_parameters_need_cells(NativeId(index as u32))?);
        }
        let mut native_values = ChargedVec::new(&usage, native_count)?;
        let active = TaskState::new(&usage, &mut heap, &mut slots, limits, false)?;
        let mut tasks = ChargedVec::new(&usage, 1)?;
        tasks.data.push(TaskState::new(
            &usage, &mut heap, &mut slots, limits, initialize,
        )?);
        let scratch = ChargedVec::new(
            &usage,
            limits.max_arguments.max(
                p.functions
                    .iter()
                    .map(|f| f.captures.len() as u32 + 3)
                    .max()
                    .unwrap_or(3),
            ) as usize,
        )?;
        let text_scratch = ChargedVec::new(
            &usage,
            (limits.heap.max_mutation_bytes as usize).min(BYTE_CHUNK_LEN),
        )?;
        let constants = slots.open_module_set(
            &mut heap,
            RootOwner(u64::MAX),
            if !initialize {
                0
            } else {
                u32::try_from(
                    p.constants
                        .len()
                        .checked_add(p.symbols.len())
                        .and_then(|n| n.checked_add(native_count))
                        .ok_or(VmError::Limit)?,
                )
                .map_err(|_| VmError::Limit)?
            },
        )?;
        if initialize {
            for (i, c) in p.constants.iter().enumerate() {
                let value = match c {
                    Const::Null => Value::NULL,
                    Const::Bool(v) => Value::bool(*v),
                    Const::Int(v) => Value::int(*v),
                    Const::FloatBits(v) => Value::float(f64::from_bits(*v)),
                    Const::Utf8(v) => heap.alloc_string(v)?,
                    Const::Bytes(v) => heap.alloc_bytes(v)?,
                    Const::Tuple(elements) => {
                        // Verified constant DAG references only earlier constants. The load
                        // path has a strict per-constant limit, like UTF-8/byte
                        // constants. Runtime MakeTuple instead uses resumable steps.
                        if elements.len() > limits.heap.max_mutation_values as usize {
                            return Err(VmError::Limit);
                        }
                        let tuple = heap.alloc_array(&[])?;
                        slots.write(&mut heap, at(constants, i as u32), tuple)?;
                        for element in elements {
                            let value = slots.read(at(constants, element.0))?;
                            heap.array_push(tuple, value)?;
                        }
                        heap.seal_tuple(tuple)?;
                        tuple
                    }
                };
                slots.write(&mut heap, at(constants, i as u32), value)?;
            }
            // Only member operands need a runtime string. Other symbols remain
            // metadata, avoiding an object allocation per unrelated identifier.
            for function in &p.functions {
                for instruction in &function.code {
                    if matches!(
                        instruction_spec(instruction.opcode).unwrap().opcode,
                        Opcode::MemberGet | Opcode::MemberSet
                    ) {
                        let symbol = instruction.operands[2];
                        let position = p.constants.len() as u32 + symbol;
                        if matches!(
                            slots.read(at(constants, position)),
                            Err(HeapError::UninitializedSlot)
                        ) {
                            let value = heap.alloc_string(&p.symbols[symbol as usize])?;
                            slots.write(&mut heap, at(constants, position), value)?;
                        }
                    }
                }
            }
        }
        // Native function values are immutable code constants. Keep one rooted
        // closure per function so reading a native name has stable identity and
        // neither allocates nor adds a synthetic author-visible global.
        for (index, function) in p.functions.iter().enumerate() {
            if matches!(function.kind, FunctionKind::Native(_)) {
                let position =
                    (p.constants.len() + p.symbols.len() + native_values.data.len()) as u32;
                if initialize {
                    let value = heap.alloc_record(
                        ObjectKind::Closure,
                        &[
                            Value::int(index as i64),
                            Value::int(p.content_version as i64),
                            Value::int(function.module.0 as i64),
                        ],
                    )?;
                    slots.write(&mut heap, at(constants, position), value)?;
                }
                native_values
                    .data
                    .push((FunctionId(index as u32), position));
            }
        }
        let globals = slots.open_module_set(
            &mut heap,
            RootOwner(u64::MAX),
            if initialize {
                p.modules.iter().map(|m| m.globals.len() as u32).sum()
            } else {
                0
            },
        )?;
        if initialize {
            let mut offset = 0;
            for (mi, module) in p.modules.iter().enumerate() {
                for global in &module.globals {
                    let ty = match p.types[global.ty.index()] {
                        TypeDef::Builtin(ty) => ty,
                        _ => BuiltinType::Any,
                    };
                    let cell = heap.alloc_language_cell(CellContract {
                        mutable: global.mutable,
                        ty,
                        type_id: Some(global.ty),
                        module: ModuleId(mi as u32),
                        code_version: p.content_version,
                        module_version: p.content_version,
                    })?;
                    slots.write(&mut heap, at(globals, offset), cell)?;
                    if let Some(f) = global.linked_function {
                        let closure =
                            if matches!(p.functions[f.index()].kind, FunctionKind::Native(_)) {
                                let index = native_values
                                    .data
                                    .binary_search_by_key(&f.0, |(function, _)| function.0)
                                    .expect("prepared native callable");
                                slots.read(at(constants, native_values.data[index].1))?
                            } else {
                                heap.alloc_record(
                                    ObjectKind::Closure,
                                    &[
                                        Value::int(f.0 as i64),
                                        Value::int(p.content_version as i64),
                                        Value::int(mi as i64),
                                    ],
                                )?
                            };
                        if !type_checks.accepts(p, &heap, global.ty, closure)? {
                            return Err(HeapError::TypeMismatch.into());
                        }
                        heap.cell_initialize(cell, closure)?;
                    }
                    offset += 1;
                }
            }
        }
        let init_symbol = p
            .symbols
            .iter()
            .position(|name| name == "init")
            .map(|i| SymbolId(i as u32));
        let type_checks = Rc::new(type_checks);
        let program = Rc::new(CodeLease {
            program,
            charge: program_charge,
            checks: type_checks.clone(),
        });
        heap.code = Some(program.clone());
        Ok(Self {
            program,
            slots,
            active,
            tasks,
            active_slot: None,
            scratch,
            text_scratch,
            native_cells,
            native_values,
            type_checks,
            init_symbol,

            globals,
            constants,
            heap,
            limits,
            control_charge,
            next_id: 1,
            native_entries: 0,
            native_resumes: 0,
            native_null_publications: 0,
            slices: 0,
            last_usage: SliceUsage::default(),
            #[cfg(test)]
            work_probe: WorkProbe::default(),
        })
    }
    pub fn program(&self) -> &VerifiedProgram {
        &self.program
    }
    pub fn task_id(&self) -> Option<TaskId> {
        self.active
            .task_id
            .or_else(|| self.tasks.data.iter().find_map(|t| t.task_id))
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
                ReturnTo::Register { frame, call, .. } | ReturnTo::Class { frame, call } => {
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
    fn read(&self, s: ScriptFrame, reg: Reg) -> Result<Value, FaultKind> {
        self.slots.read(at(s.regs, reg.0)).map_err(heap_fault)
    }
    fn write(&mut self, s: ScriptFrame, reg: Reg, v: Value) -> Result<(), FaultKind> {
        self.slots
            .write(&mut self.heap, at(s.regs, reg.0), v)
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
    fn window_push_cost(&self, windows: u64, values: u64) -> u64 {
        let spans = self.slots.stats().free_spans as u64;
        let rollback = windows - 1;
        // Open scans/removes from the current free list, physically fills each
        // Value and clears its bit. A later failure can roll back earlier opens;
        // those releases can grow the free list even when success would not.
        2 * values + windows * (2 * spans + 8) + rollback * (4 * (spans + rollback) + 8)
    }
    fn script_push_cost(&self, function: FunctionId, count: u32) -> u64 {
        let f = &self.program.package().functions[function.index()];
        if let FunctionKind::Native(native) = f.kind {
            // A callable may carry one prebound receiver root.
            return self.native_push_cost(native, count) + 1;
        }
        let values = f.register_count as u64
            + 4
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
        self.window_push_cost(4, values) + 1
    }
    fn native_push_cost(&self, native: NativeId, count: u32) -> u64 {
        let signature = self.program.package().natives[native.index()].signature;
        let sig = &self.program.package().signatures[signature.index()];
        let cells = self.native_cells.data[native.index()];
        let parameters = sig.parameters.len() as u64 + u64::from(sig.rest.is_some());
        let values = 3
            + self.limits.max_native_faults as u64
            + self.limits.max_arguments as u64
            + self.limits.max_continuation_values as u64
            + count as u64 * 2
            + parameters * (1 + u64::from(cells));
        self.window_push_cost(3 + u64::from(cells), values)
            + sig.parameters.len() as u64
            + self.native_layout_work()
            + 3
    }
    // All construction inputs are explicit so failed root publication rolls back
    // as one transaction before a frame becomes visible.
    #[allow(clippy::too_many_arguments)]
    fn push_script(
        &mut self,
        function: FunctionId,
        closure: Option<Value>,
        source: ArgSource,
        count: u32,
        owner: RootOwner,
        context: ContextFlags,
        effects: Effects,
        ret: ReturnTo,
    ) -> Result<(), VmError> {
        if self.active.frames.data.len() == self.limits.max_frames as usize
            || count > self.limits.max_arguments
        {
            return Err(VmError::Limit);
        }
        let f = &self.program.package().functions[function.index()];
        if let FunctionKind::Native(native) = f.kind {
            let site = self.native_site(function, owner, context, effects, ret)?;
            let receiver = closure
                .map(|value| self.callable_receiver(value))
                .transpose()?
                .flatten();
            return self.push_native_bound(native, source, count, site, receiver);
        }
        if !matches!(source, ArgSource::Prefix { .. })
            && let Some(closure) = closure
        {
            self.enter_initializer(closure, function)?;
        }
        let f = &self.program.package().functions[function.index()];
        let registers = f.register_count;
        let env_len = 4 + f.regions.len() as u32 * 2;
        let sig = &self.program.package().signatures[f.signature.index()];
        let n = sig.parameters.len() as u32 + u32::from(sig.rest.is_some());
        let context = inherit(context, f.context);
        let effects = Effects(
            effects.0
                & self.program.package().signatures[f.signature.index()]
                    .effects
                    .0,
        );
        let shared_parameters = matches!(source, ArgSource::Prefix { .. });
        let module = ModuleInstanceId(f.module.0);
        let parameter_module = if let ArgSource::Prefix { frame } = source {
            match self.active.frames.data[self.index(frame)?] {
                Frame::Script(parent) => parent.parameter_module,
                Frame::Native(parent) => parent.module,
            }
        } else {
            module
        };
        let id = FrameId(self.serial()?);
        // Failed construction rolls back published roots and does not push a partial frame.
        let regs = self.open(owner, RootKind::TaskFrame, registers)?;
        let env = match self.open(owner, RootKind::TaskFrame, env_len) {
            Ok(r) => r,
            Err(e) => {
                self.slots.release(&mut self.heap, regs)?;
                return Err(e);
            }
        };
        let args = match self.open(owner, RootKind::Arguments, count * 2) {
            Ok(r) => r,
            Err(e) => {
                self.slots.release(&mut self.heap, env)?;
                self.slots.release(&mut self.heap, regs)?;
                return Err(e);
            }
        };
        let bound = match self.open(owner, RootKind::Arguments, n) {
            Ok(r) => r,
            Err(e) => {
                self.slots.release(&mut self.heap, args)?;
                self.slots.release(&mut self.heap, env)?;
                self.slots.release(&mut self.heap, regs)?;
                return Err(e);
            }
        };
        self.put(env, 0, closure.unwrap_or(Value::NULL))?;
        if !shared_parameters
            && let Some(receiver) = closure
                .map(|v| self.script_receiver(v))
                .transpose()?
                .flatten()
        {
            self.put(bound, 0, receiver)?;
        }
        let s = ScriptFrame {
            id,
            code: CodeRef {
                function,
                version: self.program.package().content_version,
            },
            module,
            pc: Pc(0),
            regs,
            env,
            args,
            bound,
            argument_count: count,
            closure: closure.is_some(),
            shared_parameters,
            parameter_module,
            effects,
            context,
            owner,
            ret,
            phase: ScriptPhase::Copy { source, index: 0 },
            table_work: None,
        };
        self.active.frames.data.push(Frame::Script(s));
        Ok(())
    }
    pub fn start(
        &mut self,
        function: FunctionId,
        args: &[Value],
        owner: RootOwner,
        context: Context,
    ) -> Result<TaskId, VmError> {
        self.start_impl(function, None, args, owner, context)
    }
    pub fn start_closure(
        &mut self,
        closure: Value,
        args: &[Value],
        owner: RootOwner,
        context: Context,
    ) -> Result<TaskId, VmError> {
        let function = self.closure_function(closure)?;
        self.start_impl(function, Some(closure), args, owner, context)
    }
    fn start_impl(
        &mut self,
        function: FunctionId,
        closure: Option<Value>,
        args: &[Value],
        owner: RootOwner,
        context: Context,
    ) -> Result<TaskId, VmError> {
        if function.index() >= self.program.package().functions.len()
            || args.len() > self.limits.max_arguments as usize
            || owner == RootOwner(u64::MAX)
        {
            return Err(VmError::Limit);
        }
        let f = &self.program.package().functions[function.index()];
        if !f.captures.is_empty() && closure.is_none() {
            return Err(VmError::Contract("entry requires closure environment"));
        }
        for &v in args {
            self.heap.root_insertion_barrier(v)?;
        }
        let serial = self.serial()?;
        let task = TaskId {
            heap: self.heap.id(),
            epoch: self.heap.epoch(),
            serial,
        };
        self.restore_active();
        let slot = match self.tasks.data.iter().position(|s| s.task_id.is_none()) {
            Some(slot) => slot,
            None => self.grow_task_pool()?,
        };
        self.select_slot(slot);
        self.active.task_id = Some(task);
        self.active.task_owner = Some(owner);
        self.active.root_owner = RootOwner(u64::MAX.checked_sub(serial + 1).ok_or(VmError::Limit)?);
        if let Err(e) = self.push_script(
            function,
            closure,
            ArgSource::None,
            args.len() as u32,
            owner,
            ContextFlags::from(context),
            Effects::ALL,
            ReturnTo::Delivery,
        ) {
            self.active.task_id = None;
            self.active.task_owner = None;
            return Err(e);
        }
        let mut frame = *self.active.frames.data.last().unwrap();
        let arguments = match frame {
            Frame::Script(s) => s.args,
            Frame::Native(n) => n.args,
        };
        for (i, &v) in args.iter().enumerate() {
            self.put(arguments, i as u32 * 2, Value::int(-1))?;
            self.put(arguments, i as u32 * 2 + 1, v)?;
        }
        let signature = self.program.package().functions[function.index()].signature;
        let rest = self.program.package().signatures[signature.index()]
            .rest
            .is_some();
        match &mut frame {
            Frame::Script(s) => {
                s.phase = if rest {
                    ScriptPhase::Rest
                } else {
                    ScriptPhase::Bind(ArgumentBinding::new(self.receiver_prefix(*s)?))
                }
            }
            Frame::Native(n) => {
                n.phase = if rest {
                    NativePhase::Rest
                } else {
                    NativePhase::Bind(ArgumentBinding::new(u32::from(n.receiver)))
                }
            }
        };
        *self.active.frames.data.last_mut().unwrap() = frame;
        self.active.task_owner = Some(owner);
        self.active.task_id = Some(task);
        Ok(task)
    }
    fn grow_task_pool(&mut self) -> Result<usize, VmError> {
        let slot = self.tasks.data.len();
        if slot >= self.limits.max_tasks as usize {
            return Err(VmError::Limit);
        }
        let usage = self.heap.budget_usage();
        if slot == self.tasks.data.capacity() {
            let capacity = slot.saturating_mul(2).min(self.limits.max_tasks as usize);
            let mut next = ChargedVec::new(&usage, capacity)?;
            next.data.append(&mut self.tasks.data);
            self.tasks = next;
        }
        let roots = self
            .limits
            .max_unwinds
            .checked_mul(2)
            .and_then(|n| n.checked_add(2))
            .ok_or(VmError::Limit)?;
        self.slots.reserve_task(&mut self.heap, roots)?;
        let task = TaskState::new(&usage, &mut self.heap, &mut self.slots, self.limits, true)?;
        self.tasks.data.push(task);
        Ok(slot)
    }

    /// Register a trusted callable host object. Its first native parameter is
    /// the object itself; argument binding and effects share ordinary calls.
    pub fn bind_callable(&mut self, object: Value, function: FunctionId) -> Result<(), VmError> {
        self.validate_native_receiver(object, function)?;
        self.heap.set_host_call(object, function)?;
        Ok(())
    }

    fn callable_receiver(&self, value: Value) -> Result<Option<Value>, VmError> {
        match self.heap.info(value)?.kind {
            ObjectKind::BoundNative => Ok(Some(self.heap.get_field(value, 3)?)),
            ObjectKind::Host => {
                self.heap.host_call(value)?;
                Ok(Some(value))
            }
            _ => Ok(None),
        }
    }

    /// Bind the receiver of an admitted native callable. The immutable value
    /// traces its receiver and shares ordinary native argument preparation.
    pub fn bind_native(&mut self, receiver: Value, function: FunctionId) -> Result<Value, VmError> {
        self.validate_native_receiver(receiver, function)?;
        let p = self.program.package();
        let f = &p.functions[function.index()];

        Ok(self.heap.alloc_record(
            ObjectKind::BoundNative,
            &[
                Value::int(function.0 as i64),
                Value::int(p.content_version as i64),
                Value::int(f.module.0 as i64),
                receiver,
            ],
        )?)
    }

    fn validate_native_receiver(
        &self,
        receiver: Value,
        function: FunctionId,
    ) -> Result<(), VmError> {
        let p = self.program.package();
        let f = p
            .functions
            .get(function.index())
            .ok_or(VmError::Contract("unknown native callable"))?;
        if !matches!(f.kind, FunctionKind::Native(_)) {
            return Err(VmError::Contract(
                "receiver binding requires a native callable",
            ));
        }
        let parameter = p.signatures[f.signature.index()]
            .parameters
            .first()
            .filter(|p| p.default.is_none())
            .ok_or(VmError::Contract(
                "native receiver requires a first parameter",
            ))?;
        if !self
            .type_checks
            .accepts(p, &self.heap, parameter.ty, receiver)?
        {
            return Err(HeapError::TypeMismatch.into());
        }
        Ok(())
    }

    fn closure_function(&self, closure: Value) -> Result<FunctionId, VmError> {
        let closure = self.script_closure(closure)?;
        let info = self.heap.info(closure)?;
        if info.kind == ObjectKind::Host {
            return Ok(self.heap.host_call(closure)?);
        }
        let bound = info.kind == ObjectKind::BoundNative;
        if info.kind != ObjectKind::Closure && !bound {
            return Err(VmError::Contract("callee is not a closure"));
        }
        let fi = self
            .heap
            .get_field(closure, 0)?
            .as_int()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or(VmError::Contract("invalid closure code"))?;
        let version = self
            .heap
            .get_field(closure, 1)?
            .as_int()
            .ok_or(VmError::Contract("invalid closure version"))? as u64;
        let f = self
            .program
            .package()
            .functions
            .get(fi as usize)
            .ok_or(VmError::Contract("foreign closure code"))?;
        if version != self.program.package().content_version
            || self.heap.get_field(closure, 2)?.as_int() != Some(f.module.0 as i64)
            || info.len != f.captures.len() as u32 + 3 + u32::from(bound)
            || (bound && !matches!(f.kind, FunctionKind::Native(_)))
        {
            return Err(VmError::Contract("foreign closure environment/version"));
        }
        Ok(FunctionId(fi))
    }
    fn argument(&self, source: ArgSource, index: u32) -> Result<(Value, Value), VmError> {
        match source {
            ArgSource::None => Err(VmError::Contract("external arguments already copied")),
            ArgSource::Await { frame, value } => {
                let Frame::Script(s) = self.active.frames.data[self.index(frame)?] else {
                    return Err(VmError::Contract("await argument source"));
                };
                Ok((Value::int(-1), self.slots.read(at(s.regs, value.0))?))
            }
            ArgSource::Caller { frame, descriptor } => {
                let Frame::Script(s) = self.active.frames.data[self.index(frame)?] else {
                    return Err(VmError::Contract("argument source"));
                };
                let Descriptor::Call(desc) = &self.program.package().functions
                    [s.code.function.index()]
                .descriptors[descriptor.index()] else {
                    unreachable!()
                };
                match desc.arguments {
                    Arguments::Window(w) => {
                        // B01 verifies that names occupy the ordered window
                        // tail; copying each argument needs no name-list scan.
                        let positional = w.len as usize - desc.names.len();
                        let name = (index as usize)
                            .checked_sub(positional)
                            .map_or(-1, |i| i64::from(desc.names[i].name.0));
                        Ok((
                            Value::int(name),
                            self.slots.read(at(s.regs, w.start.0 + index))?,
                        ))
                    }
                    Arguments::Packed(r) => {
                        let packed = self.slots.read(at(s.regs, r.0))?;
                        Ok((
                            self.heap.get_field(packed, index * 2)?,
                            self.heap.get_field(packed, index * 2 + 1)?,
                        ))
                    }
                }
            }
            ArgSource::Native { frame, .. } => {
                let Frame::Native(n) = self.active.frames.data[self.index(frame)?] else {
                    unreachable!()
                };
                Ok((
                    Value::int(-1),
                    self.value(n.roots, 2 + self.limits.max_continuation_values + index)?,
                ))
            }
            ArgSource::Prefix { frame, .. } => {
                let range = match self.active.frames.data[self.index(frame)?] {
                    Frame::Script(s) => s.bound,
                    Frame::Native(n) => n.bindings.unwrap_or(n.bound),
                };
                Ok((Value::int(-1), self.value(range, index)?))
            }
        }
    }
    fn push_native(
        &mut self,
        native: NativeId,
        source: ArgSource,
        count: u32,
        site: NativeSite,
    ) -> Result<(), VmError> {
        self.push_native_bound(native, source, count, site, None)
    }
    fn push_native_bound(
        &mut self,
        native: NativeId,
        source: ArgSource,
        count: u32,
        site: NativeSite,
        receiver: Option<Value>,
    ) -> Result<(), VmError> {
        let signature = self.program.package().natives[native.index()].signature;
        let declared = self.program.package().signatures[signature.index()].effects;
        let pure = declared.0 & (Effects::HOST | Effects::WRITE | Effects::WAIT) == 0;
        // Calling a pure native is not itself an external side effect. Derived
        // callbacks retain their allocation mask and cannot admit an impure native.
        if (site.context.derived && !pure)
            || (site.effects.0 & Effects::HOST == 0 && (!pure || declared.0 & !site.effects.0 != 0))
        {
            return Err(VmError::Effect);
        }
        if self.active.frames.data.len() == self.limits.max_frames as usize
            || count > self.limits.max_arguments
        {
            return Err(VmError::Limit);
        }
        let sig = &self.program.package().signatures[signature.index()];
        let n = sig.parameters.len() as u32 + u32::from(sig.rest.is_some());
        let has_defaults = sig.parameters.iter().any(|p| p.default.is_some());
        let needs_cells = self.native_cells.data[native.index()];
        let id = FrameId(self.serial()?);
        let args = self.open(site.owner, RootKind::Arguments, count * 2)?;
        let bound = match self.open(site.owner, RootKind::Arguments, n) {
            Ok(r) => r,
            Err(e) => {
                self.slots.release(&mut self.heap, args)?;
                return Err(e);
            }
        };
        if let Some(value) = receiver
            && let Err(error) = self.put(bound, 0, value)
        {
            self.slots.release(&mut self.heap, bound)?;
            self.slots.release(&mut self.heap, args)?;
            return Err(error);
        }
        let bindings = if needs_cells {
            match self.open(site.owner, RootKind::Arguments, n) {
                Ok(r) => Some(r),
                Err(e) => {
                    self.slots.release(&mut self.heap, bound)?;
                    self.slots.release(&mut self.heap, args)?;
                    return Err(e);
                }
            }
        } else {
            None
        };
        let roots = match self.open(
            site.owner,
            RootKind::NativeContinuation,
            3 + self.limits.max_continuation_values
                + self.limits.max_arguments
                + self.limits.max_native_faults,
        ) {
            Ok(r) => r,
            Err(e) => {
                if let Some(r) = bindings {
                    self.slots.release(&mut self.heap, r)?;
                }
                self.slots.release(&mut self.heap, bound)?;
                self.slots.release(&mut self.heap, args)?;
                return Err(e);
            }
        };
        self.active.frames.data.push(Frame::Native(NativeFrame {
            id,
            await_activation: matches!(source, ArgSource::Await { .. }).then_some(id.0),
            native,
            signature,
            args,
            bound,
            bindings,
            module: site.module,
            has_defaults,
            roots,
            argument_count: count,
            receiver: receiver.is_some(),
            continuation_len: 0,
            continuation_active: false,
            continuation_version: 0,
            continuation_stage: 0,
            effects: Effects(
                site.effects.0
                    & self.program.package().signatures[signature.index()]
                        .effects
                        .0,
            ),
            context: site.context,
            owner: site.owner,
            location: site.location,
            ret: site.ret,
            callback_fault: None,
            derived_output: None,
            fault_count: 0,
            pending_fault: None,
            phase: NativePhase::Copy { source, index: 0 },
        }));
        Ok(())
    }
    pub fn host_arguments(&self, ticket: HostTicket) -> Result<&[Value], VmError> {
        let n = self.ticket(ticket)?;
        Ok(self.slots.borrow_initialized(n.bound)?)
    }
    pub fn host_continuation(&self, ticket: HostTicket) -> Result<&[Value], VmError> {
        let n = self.ticket(ticket)?;
        // Open already published physical NULLs with clear initialized bits.
        // Only values actually written by the adapter belong to this view.
        self.initialized_prefix(n.roots, 2, n.continuation_len)
    }
    /// Original value of the current callback or continued-wait resume input.
    pub fn host_resume_value(&self, ticket: HostTicket) -> Result<Value, VmError> {
        let n = self.ticket(ticket)?;
        if !matches!(
            ticket.input,
            NativeInput::CallbackReturned
                | NativeInput::CallbackFault
                | NativeInput::WaitingReturned
                | NativeInput::WaitingFault
        ) {
            return Err(VmError::InvalidTicket);
        }
        self.value(n.roots, 0)
    }
    /// Compatibility alias for the unified resume-input getter.
    pub fn host_callback_value(&self, ticket: HostTicket) -> Result<Value, VmError> {
        self.host_resume_value(ticket)
    }
    pub fn host_callback_fault(&self, ticket: HostTicket) -> Result<Option<FaultInfo>, VmError> {
        let n = self.ticket(ticket)?;
        n.callback_fault
            .map(|id| self.heap.fault_info(id).map_err(VmError::from))
            .transpose()
    }
    pub fn host_faults(&self, ticket: HostTicket) -> Result<&[Value], VmError> {
        let n = self.ticket(ticket)?;
        self.initialized_prefix(n.roots, self.native_fault_base(), n.fault_count)
    }
    pub fn host_callback_cause(&self, ticket: HostTicket) -> Result<Option<Value>, VmError> {
        let Some(info) = self.host_callback_fault(ticket)? else {
            return Ok(None);
        };
        Ok(self
            .heap
            .fault_causes(info.id)?
            .next(&self.heap)?
            .map(|c| c.value))
    }
    fn make_ticket(
        &self,
        n: NativeFrame,
        task: TaskId,
        serial: u64,
        input: NativeInput,
    ) -> HostTicket {
        HostTicket {
            heap: self.heap.id(),
            epoch: self.heap.epoch(),
            serial,
            frame: n.id,
            task,
            native: n.native,
            signature: n.signature,
            abi_version: self.program.package().natives[n.native.index()].abi_version,
            owner: n.owner,
            location: n.location,
            input,
            context: n.context,
            effects: n.effects,
            await_activation: n.await_activation,
            continuation: if input == NativeInput::Entry {
                None
            } else {
                Some(ContinuationState {
                    version: n.continuation_version,
                    stage: n.continuation_stage,
                })
            },
        }
    }
    fn ticket(&self, ticket: HostTicket) -> Result<NativeFrame, VmError> {
        let task = self.task(ticket.task).map_err(|_| VmError::InvalidTicket)?;
        if ticket.heap != self.heap.id() || ticket.epoch != self.heap.epoch() || task.cancelling {
            return Err(VmError::InvalidTicket);
        }
        match task.frames.data.last() {
            Some(Frame::Native(n)) if matches!(n.phase,NativePhase::Issued {serial,input} if self.make_ticket(*n,ticket.task,serial,input)==ticket) => {
                Ok(*n)
            }
            _ => Err(VmError::InvalidTicket),
        }
    }
    /// Acceptance is bounded by max_arguments/max_continuation_values; no callback
    /// is executed here. All borrowed input values are rooted before ticket consumption.
    pub fn respond(
        &mut self,
        ticket: HostTicket,
        response: HostResponse<'_>,
    ) -> Result<(), VmError> {
        self.select_task(ticket.task)
            .map_err(|_| VmError::InvalidTicket)?;
        let mut n = self.ticket(ticket)?;
        match response {
            HostResponse::Ready(v) => {
                self.put(n.roots, 0, v)?;
                n.pending_fault = None;
                n.phase = NativePhase::Commit(false);
            }
            HostResponse::Error(new) => {
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
                n.phase = NativePhase::Commit(true);
            }
            HostResponse::ForwardFault(id) => {
                if !self.native_holds(n, id)? {
                    return Err(VmError::InvalidFault);
                };
                let value = self.heap.fault_value(id)?;
                self.put(n.roots, 0, value)?;
                self.put(n.roots, self.native_pending_slot(), id.as_value())?;
                n.pending_fault = Some(self.forwarded_fault(n, id, true)?);
                n.phase = NativePhase::Commit(true);
            }
            HostResponse::Pending(wait) => {
                if wait != ticket.wait_id() {
                    return Err(VmError::InvalidWait);
                }
                // NoWait constrains actual waiting, not the native's signature mask.
                if no_wait(n.context) || n.effects.0 & Effects::WAIT == 0 {
                    self.native_fault(&mut n, FaultKind::Effect)?;
                } else {
                    n.phase = NativePhase::Waiting(wait);
                }
            }
            HostResponse::Continue(c) => {
                self.save_continuation(&mut n, c)?;
                n.phase = NativePhase::Continue;
            }
            HostResponse::CallScript {
                closure,
                arguments,
                continuation,
            } => {
                if arguments.len() > self.limits.max_arguments as usize {
                    return Err(VmError::Limit);
                }
                self.closure_function(closure)?;
                for &v in arguments {
                    self.heap.root_insertion_barrier(v)?;
                }
                self.save_continuation(&mut n, continuation)?;
                self.put(n.roots, 1, closure)?;
                for (i, &v) in arguments.iter().enumerate() {
                    self.put(
                        n.roots,
                        2 + self.limits.max_continuation_values + i as u32,
                        v,
                    )?;
                }
                n.phase = NativePhase::Callback {
                    count: arguments.len() as u32,
                };
            }
        }
        *self.active.frames.data.last_mut().unwrap() = Frame::Native(n);
        Ok(())
    }
    pub fn respond_language_error(
        &mut self,
        ticket: HostTicket,
        kind: FaultKind,
    ) -> Result<(), VmError> {
        self.select_task(ticket.task)
            .map_err(|_| VmError::InvalidTicket)?;
        let mut n = self.ticket(ticket)?;
        self.native_fault(&mut n, kind)?;
        *self.active.frames.data.last_mut().unwrap() = Frame::Native(n);
        Ok(())
    }
    fn save_continuation(
        &mut self,
        n: &mut NativeFrame,
        c: Continuation<'_>,
    ) -> Result<(), VmError> {
        if c.values.len() > self.limits.max_continuation_values as usize {
            return Err(VmError::Limit);
        }
        if let FaultRetention::Replace(ids) = c.faults {
            if ids.len() > self.limits.max_native_faults as usize {
                return Err(VmError::Limit);
            };
            for &id in ids {
                self.heap.fault_info(id)?;
                self.heap.root_insertion_barrier(id.as_value())?;
            }
        }
        for &v in c.values {
            self.heap.root_insertion_barrier(v)?;
        }
        for (i, &v) in c.values.iter().enumerate() {
            self.put(n.roots, 2 + i as u32, v)?;
        }
        for i in c.values.len() as u32..n.continuation_len {
            self.put(n.roots, 2 + i, Value::NULL)?;
        }
        self.retain_native_faults(n, c.faults)?;
        n.continuation_active = true;
        n.continuation_len = c.values.len() as u32;
        n.continuation_version = c.version;
        n.continuation_stage = c.stage;
        Ok(())
    }
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
    fn delivery_check(&self, id: DeliveryId) -> Result<Terminal, VmError> {
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
        self.active.task_id = None;
        Ok(())
    }
    /// Invalidate this task's tickets immediately and schedule budgeted frame cleanup.
    /// Independently retained host roots remain the host's responsibility.
    pub fn cancel_task(&mut self, task: TaskId) -> Result<(), VmError> {
        self.select_task(task)?;
        self.cancel_active()
    }
    fn cancel_active(&mut self) -> Result<(), VmError> {
        if self.active.cancelling {
            return Ok(());
        }
        self.active.cancelling = true;
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
    fn restore_active(&mut self) {
        if let Some(slot) = self.active_slot.take() {
            std::mem::swap(&mut self.active, &mut self.tasks.data[slot]);
        }
    }
    fn select_slot(&mut self, slot: usize) {
        self.restore_active();
        std::mem::swap(&mut self.active, &mut self.tasks.data[slot]);
        self.active_slot = Some(slot);
    }
    fn task(&self, id: TaskId) -> Result<&TaskState, VmError> {
        if id.heap != self.heap.id() || id.epoch != self.heap.epoch() {
            return Err(VmError::UnknownTask);
        }
        if self.active.task_id == Some(id) {
            return Ok(&self.active);
        }
        self.tasks
            .data
            .iter()
            .find(|t| t.task_id == Some(id))
            .ok_or(VmError::UnknownTask)
    }
    fn select_task(&mut self, id: TaskId) -> Result<(), VmError> {
        if self.active.task_id == Some(id) {
            return Ok(());
        }
        if id.heap != self.heap.id() || id.epoch != self.heap.epoch() {
            return Err(VmError::UnknownTask);
        }
        let slot = self
            .tasks
            .data
            .iter()
            .position(|t| t.task_id == Some(id))
            .ok_or(VmError::UnknownTask)?;
        self.select_slot(slot);
        Ok(())
    }
    pub fn pooled_root_ranges(&self) -> u32 {
        2 + 2 * self.tasks.data.len() as u32
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
        };
        let result = self.run(&mut meter, &mut now);
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
            if m.budget.deadline_ns.is_some_and(|d| now() >= d) {
                return Ok(RunOutcome::BudgetYield(m.usage));
            }
            if self.active.cancelling {
                if !self.active.caught.data.is_empty() {
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
                if let Some(u) = self.active.unwinds.data.last().copied() {
                    if !m.take(0, self.release_cost(1), 0) {
                        return Ok(RunOutcome::BudgetYield(m.usage));
                    }
                    self.slots.release(&mut self.heap, u.value)?;
                    self.active.unwinds.data.pop();
                    continue;
                }
                if let Some(f) = self.active.frames.data.last().copied() {
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
                self.active.task_id = None;
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
                    2 * self.active.frames.data.len() as u64 + self.native_layout_work() + 3,
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
        self.clear(n.roots, self.native_pending_slot())?;
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
    fn script_step(&mut self, mut s: ScriptFrame, m: &mut Meter) -> Result<bool, VmError> {
        let top = self.active.frames.data.len() - 1;
        let signature = self.program.package().functions[s.code.function.index()].signature;
        match s.phase {
            ScriptPhase::Class(step) => {
                if !self.class_step(&mut s, step, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::Copy { source, index } => {
                if !m.take(0, 1, 0) {
                    return Ok(false);
                }
                if index < s.argument_count {
                    match self.argument(source, index) {
                        Ok((name, v)) => {
                            self.put(s.args, index * 2, name)?;
                            self.put(s.args, index * 2 + 1, v)?;
                            s.phase = ScriptPhase::Copy {
                                source,
                                index: index + 1,
                            };
                        }
                        Err(e) => {
                            let kind = language_fault(e)?;
                            self.preparation_fault(&mut s, kind)?;
                        }
                    }
                } else {
                    s.phase = if self.program.package().signatures[signature.index()]
                        .rest
                        .is_some()
                    {
                        ScriptPhase::Rest
                    } else {
                        ScriptPhase::Bind(ArgumentBinding::new(self.receiver_prefix(s)?))
                    };
                }
            }
            ScriptPhase::Rest => {
                if !m.take(0, 1, 0) {
                    return Ok(false);
                }
                let count = self.program.package().signatures[signature.index()]
                    .parameters
                    .len() as u32;
                match self.heap.alloc_array(&[]) {
                    Ok(rest) => {
                        self.put(s.bound, count, rest)?;
                        s.phase = ScriptPhase::Bind(ArgumentBinding::new(self.receiver_prefix(s)?));
                    }
                    Err(error) => self.preparation_fault(&mut s, heap_fault(error))?,
                }
            }
            ScriptPhase::Bind(mut binding) => {
                let result = if binding.index == s.argument_count {
                    if !m.take(0, 1, 0) {
                        return Ok(false);
                    }
                    s.phase = ScriptPhase::Defaults { parameter: 0 };
                    None
                } else if s.shared_parameters {
                    if !m.take(0, 1, 0) {
                        return Ok(false);
                    }
                    let value = self.value(s.args, binding.index * 2 + 1)?;
                    self.put(s.bound, binding.index, value)?;
                    binding.advance(Value::int(-1));
                    Some(ArgumentStep::Ready(Ok(())))
                } else {
                    Some(self.bind_argument_step(signature, s.bound, s.args, &mut binding, m)?)
                };
                if let Some(result) = result {
                    s.phase = ScriptPhase::Bind(binding);
                    match result {
                        ArgumentStep::Blocked => {
                            self.active.frames.data[top] = Frame::Script(s);
                            return Ok(false);
                        }
                        ArgumentStep::Ready(Err(kind)) => self.preparation_fault(&mut s, kind)?,
                        _ => {}
                    }
                }
            }
            ScriptPhase::Defaults { parameter } => {
                let f = &self.program.package().functions[s.code.function.index()];
                if !m.take(0, f.bindings.len() as u64 + 2, 0) {
                    return Ok(false);
                }
                let sig = &self.program.package().signatures[signature.index()];
                if parameter as usize == sig.parameters.len() + usize::from(sig.rest.is_some()) {
                    s.phase = ScriptPhase::Install { binding: 0 };
                } else if self.slots.read(at(s.bound, parameter)).is_ok() {
                    let storage = f
                        .bindings
                        .iter()
                        .find(|b| b.initialization == BindingInitialization::Parameter(parameter))
                        .map_or(BindingStorage::Value(Reg(parameter)), |b| b.storage);
                    s.phase = ScriptPhase::Parameter { parameter, storage };
                } else if let Some(default) = sig.parameters[parameter as usize].default {
                    s.phase = ScriptPhase::DefaultCall {
                        parameter,
                        function: default,
                    };
                } else {
                    self.preparation_fault(&mut s, FaultKind::MissingArgument)?;
                }
            }
            ScriptPhase::DefaultCall {
                parameter,
                function: default,
            } => {
                if !m.take(0, self.script_push_cost(default, parameter) + 1, 0) {
                    return Ok(false);
                }
                let closure = if s.closure {
                    Some(self.value(s.env, 0)?)
                } else {
                    None
                };
                self.active.frames.data[top] = Frame::Script(s);
                let prepared = self.push_script(
                    default,
                    closure,
                    ArgSource::Prefix { frame: s.id },
                    parameter,
                    s.owner,
                    inherit(s.context, Context::NoWait),
                    s.effects,
                    ReturnTo::Default {
                        frame: s.id,
                        parameter,
                        call: s.pc,
                    },
                );
                match prepared {
                    Ok(()) => return Ok(true),
                    Err(e) => {
                        let kind = language_fault(e)?;
                        self.preparation_fault(&mut s, kind)?;
                    }
                }
            }
            ScriptPhase::Parameter { parameter, storage } => {
                let bytes = if matches!(storage, BindingStorage::Cell(_)) && !s.shared_parameters {
                    std::mem::size_of::<crate::cell::CellState>() as u64
                } else {
                    0
                };
                let sig = &self.program.package().signatures[signature.index()];
                let p = sig
                    .parameters
                    .get(parameter as usize)
                    .or(sig.rest.as_ref())
                    .unwrap();
                if !m.take(0, 4 + self.type_checks.extra(p.ty), bytes) {
                    return Ok(false);
                }
                let input = self.value(s.bound, parameter)?;
                let contract = self.contract(ModuleId(s.parameter_module.0), p.ty, p.mutable);
                let prepared = self.parameter_value(contract, input, storage, s.shared_parameters);
                match prepared {
                    Ok(value) => {
                        self.put(s.bound, parameter, value)?;
                        self.put(s.regs, storage.reg().0, value)?;
                        s.phase = ScriptPhase::Defaults {
                            parameter: parameter + 1,
                        };
                    }
                    Err(kind) => self.preparation_fault(&mut s, kind)?,
                }
            }
            ScriptPhase::Install { binding } => {
                if !m.take(0, 1, 0) {
                    return Ok(false);
                }
                let f = &self.program.package().functions[s.code.function.index()];
                if (binding as usize) < f.bindings.len() {
                    let b = f.bindings[binding as usize].clone();
                    match b.initialization {
                        BindingInitialization::Explicit => {
                            // Value binding bitmap starts uninitialized even if a temporary used it.
                            if matches!(b.storage, BindingStorage::Value(_)) {
                                self.clear(s.regs, b.storage.reg().0)?;
                            }
                        }
                        // Prepared in parameter order before subsequent defaults.
                        BindingInitialization::Parameter(_) => {}
                    }
                    s.phase = ScriptPhase::Install {
                        binding: binding + 1,
                    };
                } else {
                    s.phase = ScriptPhase::Body;
                }
            }
            ScriptPhase::Body => {
                let instruction =
                    self.program.package().functions[s.code.function.index()].code[s.pc.index()];
                let op = instruction_spec(instruction.opcode)
                    .expect("verified spec")
                    .opcode;
                let [a, b, c] = instruction.operands;
                let (work, allocation) = match self.instruction_cost(s, op, a, b, c) {
                    Ok(cost) => cost,
                    Err(error) => {
                        if !m.take(1, 1, 0) {
                            return Ok(false);
                        }
                        self.put(s.env, 1, Value::NULL)?;
                        self.clear(s.env, 2)?;
                        s.phase = ScriptPhase::Exit {
                            origin: s.pc,
                            reason: ExitReason::Throw(FaultSeed {
                                fault: None,
                                explicit: None,
                                author_value: false,
                                awaited: None,

                                kind: vm_fault(error),
                                location: self.loc(s, s.pc),
                            }),
                            skip: None,
                        };
                        self.active.frames.data[top] = Frame::Script(s);
                        return Ok(true);
                    }
                };
                if !m.take(1, work, allocation) {
                    return Ok(false);
                }
                let step = self.execute(&mut s, op, a, b, c);
                match step {
                    Ok(true) => {
                        self.active.frames.data[top] = Frame::Script(s);
                        return Ok(true);
                    }
                    Ok(false) => {}
                    Err(kind) => {
                        self.abort_class(s)?;
                        self.put(s.env, 1, Value::NULL)?;
                        self.clear(s.env, 2)?;
                        s.phase = ScriptPhase::Exit {
                            origin: s.pc,
                            reason: ExitReason::Throw(FaultSeed {
                                fault: None,
                                explicit: None,
                                author_value: false,
                                awaited: None,

                                kind,
                                location: self.loc(s, s.pc),
                            }),
                            skip: None,
                        };
                    }
                }
            }
            ScriptPhase::Spread(work) => {
                if !self.spread_step(&mut s, work, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::TableSpread(work) => {
                if !self.table_spread_step(&mut s, work, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::KeyPath { index } => {
                if !self.key_path_step(&mut s, index, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::TableValidate | ScriptPhase::TableScan => {
                if !self.table_step(&mut s, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::Sequence {
                dest,
                window,
                index,
                tuple,
            } => {
                if !self.sequence_step(&mut s, dest, window, index, tuple, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::TextFinish { .. } => {
                if !self.text_finish_step(&mut s, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::TextPush {
                builder,
                input,
                offset,
            } => {
                if !self.text_push_step(&mut s, builder, input, offset, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::Builtin(_) => {
                if !self.builtin_step(&mut s, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::Bulk(work) => {
                if !self.bulk_step(&mut s, work, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::TextBuiltin(work) => {
                if !self.text_builtin_step(&mut s, work, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::Concat {
                dest,
                left,
                right,
                offset,
            } => {
                if !self.concat_step(&mut s, dest, left, right, offset, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::Equality { dest, negated } => {
                if !m.take(0, 1, 0) {
                    return Ok(false);
                }
                self.equality_step(&mut s, dest, negated)?;
            }
            ScriptPhase::EqualityCleanup {
                dest,
                negated,
                result,
            } => {
                if !m.take(0, 1, 0) {
                    return Ok(false);
                }
                if !self.active.equality_cursors.data.is_empty() {
                    let depth = self.active.equality_cursors.data.len() as u32 - 1;
                    self.clear(self.active.equality_roots.unwrap(), depth * 2)?;
                    self.clear(self.active.equality_roots.unwrap(), depth * 2 + 1)?;
                    self.active.equality_cursors.data.pop();
                } else {
                    if s.table_work.is_some() {
                        self.table_comparison_done(&mut s, result)?;
                    } else {
                        match result {
                            Ok(equal) => {
                                self.put(s.regs, dest.0, Value::bool(equal != negated))?;
                                s.pc.0 += 1;
                                s.phase = ScriptPhase::Body;
                            }
                            Err(kind) => {
                                self.put(s.env, 1, Value::NULL)?;
                                self.clear(s.env, 2)?;
                                s.phase = ScriptPhase::Exit {
                                    origin: s.pc,
                                    reason: ExitReason::Throw(FaultSeed {
                                        fault: None,
                                        explicit: None,
                                        author_value: false,
                                        awaited: None,

                                        kind,
                                        location: self.loc(s, s.pc),
                                    }),
                                    skip: None,
                                };
                            }
                        }
                    }
                }
            }
            ScriptPhase::Exit {
                origin,
                reason,
                skip,
            } => {
                let regions = self.program.package().functions[s.code.function.index()]
                    .regions
                    .len() as u64;
                // The shared selector visits every region. A consumed-region
                // check can compare up to three parent blocks with three child
                // blocks. Admit this bound before making any selector call.
                let work = regions * (if skip.is_some() { 32 } else { 8 }) + 1;
                if !m.take(0, work, 0) {
                    return Ok(false);
                }
                let handler = self.select_exit_handler(s, origin, reason, skip)?;
                s.phase = ScriptPhase::ExitApply {
                    origin,
                    reason,
                    handler,
                };
            }
            ScriptPhase::ExitApply {
                origin,
                reason,
                handler,
            } => {
                let superseded = self.superseded_unwind(s, origin, reason, handler);
                let replacing_fault =
                    superseded.is_some() && matches!(reason, ExitReason::Throw(_));
                let roots = if superseded.is_some() {
                    self.release_cost(1)
                } else if handler.is_some_and(|h| h.kind == HandlerKind::Finally) {
                    self.window_push_cost(1, 2)
                } else {
                    0
                };
                let work = 2 * self.active.caught.data.len() as u64
                    + 12
                    + roots
                    + if replacing_fault {
                        self.native_layout_work()
                    } else {
                        0
                    };
                let alloc = if replacing_fault {
                    crate::fault::FAULT_NODE_BYTES
                } else {
                    0
                };
                if !m.take(0, work, alloc) {
                    return Ok(false);
                }
                if !self.exit_step(&mut s, origin, reason, handler, superseded)? {
                    return Ok(false);
                }
            }
            ScriptPhase::Release { reason } => {
                let caught = self.active.caught.data.len() as u64;
                let next = (caught + 2).max(self.release_cost(4) + self.commit_work(s.ret));
                // The scan is not a saved stage. Charge it with its successor
                // so the reported retry grant can complete this bounded action.
                if !m.take(0, caught + next, 0) {
                    return Ok(false);
                }
                let index = self.active.caught.data.iter().position(|c| {
                    #[cfg(test)]
                    self.work_probe
                        .release_catch_checks
                        .set(self.work_probe.release_catch_checks.get() + 1);
                    c.frame == s.id
                });
                if let Some(index) = index {
                    let record = self.active.caught.data.remove(index);
                    self.clear(s.env, 4 + record.region.0 * 2)?;
                    self.clear(s.env, 5 + record.region.0 * 2)?;
                    self.active.frames.data[top] = Frame::Script(s);
                    return Ok(true);
                }
                let value = self.value(s.env, 1)?;
                let cause = match reason {
                    ExitReason::Throw(info) if info.fault.is_some() => Some(self.value(s.env, 2)?),
                    _ => None,
                };
                if !self.commit(
                    s.ret,
                    value,
                    cause,
                    match reason {
                        ExitReason::Throw(info) => Some(info),
                        _ => None,
                    },
                )? {
                    self.active.frames.data[top] = Frame::Script(s);
                    return Ok(true);
                }
                self.release_frame(Frame::Script(s))?;
                self.active.frames.data.remove(top);
                return Ok(true);
            }
        }
        self.active.frames.data[top] = Frame::Script(s);
        Ok(true)
    }
    fn instruction_cost(
        &self,
        s: ScriptFrame,
        op: Opcode,
        _a: u32,
        b: u32,
        c: u32,
    ) -> Result<(u64, u64), VmError> {
        let f = &self.program.package().functions[s.code.function.index()];
        Ok(match op {
            Opcode::BindInit | Opcode::BindWrite => {
                (1 + self.type_checks.extra(f.bindings[_a as usize].ty), 0)
            }
            Opcode::CellWrite => {
                let contract = self
                    .heap
                    .cell_info(self.slots.read(at(s.regs, _a))?)?
                    .contract;
                (
                    1 + contract.type_id.map_or(0, |ty| self.type_checks.extra(ty)),
                    0,
                )
            }
            Opcode::Is => (1 + self.type_checks.extra(TypeId(c)), 0),
            Opcode::Return => (
                1 + self
                    .type_checks
                    .extra(self.program.package().signatures[f.signature.index()].return_type),
                0,
            ),
            Opcode::Call | Opcode::CallNoWait => {
                let Descriptor::Call(d) = &f.descriptors[b as usize] else {
                    unreachable!()
                };
                let count = match d.arguments {
                    Arguments::Window(w) => w.len,
                    Arguments::Packed(r) => {
                        self.heap
                            .packed_array_len(self.slots.read(at(s.regs, r.0))?)?
                            / 2
                    }
                };
                let mut allocation = 0;
                let cost = match d.target {
                    CallTarget::Function(id) => self.script_push_cost(id, count),
                    CallTarget::Native(id) => self.native_push_cost(id, count),
                    CallTarget::Register(r) => {
                        let target = self.slots.read(at(s.regs, r.0))?;
                        if self.is_text(target, ObjectKind::Class) {
                            allocation = VALUE_CHUNK_BYTES * 3;
                            24
                        } else {
                            let cost = self
                                .closure_function(target)
                                .map(|id| self.script_push_cost(id, count))
                                .unwrap_or(1);
                            if self.is_text(target, ObjectKind::BoundScript) {
                                allocation = VALUE_CHUNK_BYTES;
                                cost + self.initializer_cost(target)?
                            } else {
                                cost
                            }
                        }
                    }
                };
                (cost + self.active.unwinds.data.len() as u64 + 1, allocation)
            }
            Opcode::Await => {
                let cost = self
                    .heap
                    .host_await(self.slots.read(at(s.regs, b))?)
                    .map(|id| self.native_push_cost(id, 1))
                    .unwrap_or(1);
                (cost + self.active.unwinds.data.len() as u64 + 1, 0)
            }
            Opcode::MakeClosure => {
                let Descriptor::Closure { function, captures } = &f.descriptors[b as usize] else {
                    unreachable!()
                };
                if matches!(
                    self.program.package().functions[function.index()].kind,
                    FunctionKind::Native(_)
                ) {
                    (self.native_values.data.len().ilog2() as u64 + 2, 0)
                } else {
                    (
                        captures.len() as u64 + 1,
                        ((captures.len() + 3) as u64).div_ceil(VALUE_CHUNK_LEN as u64)
                            * VALUE_CHUNK_BYTES,
                    )
                }
            }
            Opcode::MakeTuple | Opcode::MakeArray => (1, 0),
            Opcode::Throw | Opcode::Rethrow => (
                f.regions.len() as u64 + self.active.caught.data.len() as u64 + 3,
                0,
            ),
            Opcode::EndFinally => (self.release_cost(1), 0),
            Opcode::GlobalRead | Opcode::GlobalInit | Opcode::GlobalWrite => {
                let Descriptor::Global { module, global } = f.descriptors[b as usize] else {
                    unreachable!()
                };
                let binding = self.program.global_binding(module, global)?;
                let extra = if op == Opcode::GlobalRead || binding.imported {
                    0
                } else {
                    self.type_checks.extra(binding.ty)
                };
                (1 + extra, 0)
            }
            Opcode::MakeClass => (24, VALUE_CHUNK_BYTES * 6),
            Opcode::SuperRef => (4, VALUE_CHUNK_BYTES),
            Opcode::PropertyRef => {
                let object = self.slots.read(at(s.regs, b))?;
                if object.tag() == ValueTag::Object
                    && matches!(
                        self.heap.info(object)?.kind,
                        ObjectKind::Instance | ObjectKind::Super
                    )
                {
                    let Descriptor::Property { key, .. } = f.descriptors[c as usize] else {
                        unreachable!()
                    };
                    let key = self.slots.read(at(s.regs, key.0))?;
                    let bytes = if key.tag() == ValueTag::Object {
                        self.heap.info(key)?.len
                    } else {
                        0
                    };
                    (
                        self.type_checks.symbol_work(bytes) + self.class_member_cost(object)?,
                        VALUE_CHUNK_BYTES,
                    )
                } else {
                    (1, VALUE_CHUNK_BYTES)
                }
            }
            Opcode::MemberGet | Opcode::MemberSet => {
                let object = self
                    .slots
                    .read(at(s.regs, if op == Opcode::MemberGet { b } else { _a }))?;
                let kind = self.heap.info(object)?.kind;
                if matches!(
                    kind,
                    ObjectKind::Class | ObjectKind::Instance | ObjectKind::Super
                ) {
                    let extra = if op == Opcode::MemberSet {
                        self.class_write_cost(object, SymbolId(c))?
                    } else {
                        0
                    };
                    return Ok((self.class_member_cost(object)? + extra, VALUE_CHUNK_BYTES));
                }
                (
                    1,
                    if matches!(kind, ObjectKind::Array | ObjectKind::Tuple) {
                        VALUE_CHUNK_BYTES
                    } else {
                        0
                    },
                )
            }
            Opcode::IterAdvance => (5, 0),
            Opcode::ArgsNew => (1, 0),
            Opcode::IterInit
            | Opcode::IterNext
            | Opcode::MakeRange
            | Opcode::MakeRangeInclusive => (4, VALUE_CHUNK_BYTES),
            Opcode::ChildrenPush | Opcode::ArrayPush => {
                let array = self.slots.read(at(s.regs, _a))?;
                let value = self.slots.read(at(s.regs, b))?;
                let len = self.heap.info(array)?.len;
                (
                    1,
                    if (op == Opcode::ArrayPush || !value.same_bits(Value::NULL))
                        && (len as usize).is_multiple_of(VALUE_CHUNK_LEN)
                    {
                        VALUE_CHUNK_BYTES
                    } else {
                        0
                    },
                )
            }
            Opcode::ArgsPush | Opcode::ArgsNamed => {
                let packed = self.slots.read(at(s.regs, _a))?;
                let len = self.heap.packed_array_len(packed)?;
                (
                    2,
                    if (len as usize).is_multiple_of(VALUE_CHUNK_LEN) {
                        VALUE_CHUNK_BYTES
                    } else {
                        0
                    },
                )
            }
            _ => (1, 0),
        })
    }
    /// All opcode identities and operand positions come from bytecode::instruction_spec.
    fn execute(
        &mut self,
        s: &mut ScriptFrame,
        op: Opcode,
        a: u32,
        b: u32,
        c: u32,
    ) -> Result<bool, FaultKind> {
        let r = |n| Reg(n);
        let f = &self.program.package().functions[s.code.function.index()];
        if op == Opcode::MakeClosure {
            let Descriptor::Closure { function, .. } = &f.descriptors[b as usize] else {
                unreachable!()
            };
            if matches!(
                self.program.package().functions[function.index()].kind,
                FunctionKind::Native(_)
            ) {
                let index = self
                    .native_values
                    .data
                    .binary_search_by_key(function, |(id, _)| *id)
                    .unwrap();
                let value = self
                    .slots
                    .read(at(self.constants, self.native_values.data[index].1))
                    .map_err(heap_fault)?;
                self.write(*s, r(a), value)?;
                s.pc.0 += 1;
                return Ok(true);
            }
        }
        if matches!(
            op,
            Opcode::CellWrite
                | Opcode::BindWrite
                | Opcode::GlobalWrite
                | Opcode::GlobalInit
                | Opcode::IndexSet
                | Opcode::MemberSet
        ) {
            let allowed = if s.context.derived {
                match op {
                    Opcode::BindWrite => match f.bindings[a as usize].storage {
                        BindingStorage::Value(_) => true,
                        BindingStorage::Cell(reg) => {
                            self.heap.derived_write_allowed(self.read(*s, reg)?)
                        }
                    },
                    Opcode::CellWrite | Opcode::IndexSet | Opcode::MemberSet => {
                        self.heap.derived_write_allowed(self.read(*s, r(a))?)
                    }
                    _ => false,
                }
            } else {
                s.effects.0 & Effects::WRITE != 0
            };
            if !allowed {
                return Err(FaultKind::Effect);
            }
        }
        if matches!(
            op,
            Opcode::PropertyRef
                | Opcode::MakeClass
                | Opcode::SuperRef
                | Opcode::CellNew
                | Opcode::MakeClosure
                | Opcode::MakeTuple
                | Opcode::MakeArray
                | Opcode::MakeTable
                | Opcode::ArgsNew
                | Opcode::ArgsPush
                | Opcode::ArgsNamed
                | Opcode::ArgsSpread
                | Opcode::ArraySpread
                | Opcode::TableSpread
                | Opcode::ArrayPush
                | Opcode::ChildrenPush
                | Opcode::TextNew
                | Opcode::TextPush
                | Opcode::MakeRange
                | Opcode::MakeRangeInclusive
                | Opcode::IterInit
                | Opcode::IterNext
        ) && s.effects.0 & Effects::ALLOCATE == 0
        {
            return Err(FaultKind::Effect);
        }
        match op {
            Opcode::MakeClass => {
                self.start_class(s, r(a), DescriptorId(b))?;
                return Ok(false);
            }
            Opcode::SuperRef => {
                let value = self.super_ref(*s)?;
                self.write(*s, r(a), value)?;
            }
            Opcode::TextNew => {
                let value = self.heap.alloc_byte_buffer(&[]).map_err(heap_fault)?;
                self.write(*s, r(a), value)?;
            }
            Opcode::TextPush => {
                let builder = self.read(*s, r(a))?;
                if self.heap.info(builder).map_err(heap_fault)?.kind != ObjectKind::ByteBuffer {
                    return Err(FaultKind::Type);
                }
                s.phase = ScriptPhase::TextPush {
                    builder: r(a),
                    input: r(b),
                    offset: 0,
                };
                return Ok(false);
            }
            Opcode::TextFinish => {
                let value = self.read(*s, r(b))?;
                let info = self.heap.info(value).map_err(heap_fault)?;
                if info.kind != ObjectKind::ByteBuffer {
                    return Err(FaultKind::Type);
                }
                s.phase = ScriptPhase::TextFinish {
                    dest: r(a),
                    input: r(b),
                    offset: 0,
                    version: info.content_version,
                    utf8: Default::default(),
                };
                return Ok(false);
            }
            Opcode::ChildrenPush => {
                let array = self.read(*s, r(a))?;
                let child = self.read(*s, r(b))?;
                if !child.same_bits(Value::NULL) {
                    if !self.heap.is_composable_host(child).map_err(heap_fault)? {
                        return Err(FaultKind::Type);
                    }
                    self.heap.array_push(array, child).map_err(heap_fault)?;
                }
            }
            Opcode::ArrayPush => {
                let array = self.read(*s, r(a))?;
                let value = self.read(*s, r(b))?;
                self.heap.array_push(array, value).map_err(heap_fault)?;
            }
            Opcode::TableSpread => {
                let table = self.read(*s, r(a))?;
                self.start_table_spread(s, table, r(b), collections::TableFinish::Instruction)?;
                return Ok(false);
            }
            Opcode::MakeRange | Opcode::MakeRangeInclusive => {
                let start = self.read(*s, r(b))?.as_int().ok_or(FaultKind::Type)?;
                let end = self.read(*s, r(c))?.as_int().ok_or(FaultKind::Type)?;
                let value = self
                    .heap
                    .alloc_record(
                        ObjectKind::Range,
                        &[
                            Value::int(start),
                            Value::int(end),
                            Value::bool(op == Opcode::MakeRangeInclusive),
                        ],
                    )
                    .map_err(heap_fault)?;
                self.write(*s, r(a), value)?;
            }
            Opcode::IterInit => {
                let source = self.read(*s, r(b))?;
                let iterator = self.iterator_new(source).map_err(heap_fault)?;
                self.write(*s, r(a), iterator)?;
            }
            Opcode::IterNext => {
                let iterator = self.read(*s, r(b))?;
                let value = self.iterator_next(iterator).map_err(heap_fault)?;
                self.write(*s, r(a), value)?;
            }
            Opcode::IterAdvance => {
                let iterator = self.read(*s, r(b))?;
                let step = self.iterator_peek(iterator).map_err(heap_fault)?;
                self.write(*s, r(a), Value::bool(step.done))?;
                self.write(*s, r(c), step.value)?;
                self.iterator_commit(iterator, step).map_err(heap_fault)?;
            }
            Opcode::IterClose => {
                let iterator = self.read(*s, r(a))?;
                if self.heap.info(iterator).map_err(heap_fault)?.kind != ObjectKind::Iterator {
                    return Err(FaultKind::Type);
                }
                self.heap
                    .set_field(iterator, 0, Value::NULL)
                    .map_err(heap_fault)?;
            }
            Opcode::MakeTuple | Opcode::MakeArray => {
                let Descriptor::Sequence(window) = f.descriptors[b as usize] else {
                    unreachable!()
                };
                let builder = self.heap.alloc_array(&[]).map_err(heap_fault)?;
                self.put(s.env, 1, builder).map_err(vm_fault)?;
                s.phase = ScriptPhase::Sequence {
                    dest: r(a),
                    window,
                    index: 0,
                    tuple: op == Opcode::MakeTuple,
                };
                return Ok(false);
            }
            Opcode::MakeTable => {
                let table = self.heap.alloc_table().map_err(heap_fault)?;
                self.put(s.env, 1, table).map_err(vm_fault)?;
                self.next_table_field(s, r(a), DescriptorId(b), 0)
                    .map_err(vm_fault)?;
                return Ok(false);
            }
            Opcode::PropertyRef => {
                let host = self.read(*s, r(b))?;
                let Descriptor::Property { key, writable } = self.program.package().functions
                    [s.code.function.index()]
                .descriptors[c as usize] else {
                    unreachable!()
                };
                let key = self.read(*s, key)?;
                if matches!(
                    self.heap.info(host).map_err(heap_fault)?.kind,
                    ObjectKind::Instance | ObjectKind::Super
                ) {
                    let reference = self.class_property_ref(host, key, writable)?;
                    self.write(*s, r(a), reference)?;
                    s.pc.0 += 1;
                    return Ok(true);
                }
                let fields = self.heap.property_fields(host).map_err(heap_fault)?;
                self.start_table(
                    s,
                    fields,
                    key,
                    TableAction::PropertyRef {
                        dest: r(a),
                        object: r(b),
                        writable,
                    },
                )?;
                return Ok(false);
            }
            Opcode::MemberGet | Opcode::MemberSet => {
                let object = self.read(*s, r(if op == Opcode::MemberGet { b } else { a }))?;
                if matches!(
                    self.heap.info(object).map_err(heap_fault)?.kind,
                    ObjectKind::Class | ObjectKind::Instance | ObjectKind::Super
                ) {
                    let value = if op == Opcode::MemberSet {
                        Some(self.read(*s, r(b))?)
                    } else {
                        None
                    };
                    let result = self.class_member(object, SymbolId(c), value, s.effects)?;
                    if op == Opcode::MemberGet {
                        self.write(*s, r(a), result)?;
                    }
                    s.pc.0 += 1;
                    return Ok(true);
                }
                if op == Opcode::MemberGet && self.heap.is_property_ref(object) {
                    if self.program.package().symbols[c as usize] != "value" {
                        return Err(FaultKind::MissingKey);
                    }
                    let (property, _) = self.heap.property_ref(object).map_err(heap_fault)?;
                    self.heap
                        .observe_read(property.host, Some(property.field))
                        .map_err(heap_fault)?;
                    let value = self.heap.host_property(property).map_err(heap_fault)?;
                    self.write(*s, r(a), value)?;
                    s.pc.0 += 1;
                    return Ok(true);
                }
                if op == Opcode::MemberGet {
                    let kind = self.heap.info(object).map_err(heap_fault)?.kind;
                    if matches!(
                        kind,
                        ObjectKind::Array | ObjectKind::Tuple | ObjectKind::String
                    ) {
                        let method =
                            Builtin::lookup(kind, &self.program.package().symbols[c as usize])
                                .ok_or(FaultKind::MissingKey)?;
                        let value = self.bound_builtin(object, method, s.effects)?;
                        self.write(*s, r(a), value)?;
                        s.pc.0 += 1;
                        return Ok(false);
                    }
                }
                let host = self.is_text(object, ObjectKind::Host);
                let object = if host {
                    self.heap.host_fields(object).map_err(heap_fault)?
                } else {
                    object
                };
                let key = self
                    .value(
                        self.constants,
                        self.program.package().constants.len() as u32 + c,
                    )
                    .map_err(vm_fault)?;
                let action = if op == Opcode::MemberGet {
                    if host {
                        TableAction::Get(r(a))
                    } else {
                        TableAction::MemberGet {
                            dest: r(a),
                            symbol: SymbolId(c),
                        }
                    }
                } else if host {
                    TableAction::SetHost {
                        object: r(a),
                        value: r(b),
                    }
                } else {
                    TableAction::Set(r(b))
                };
                self.start_table(s, object, key, action)?;
                return Ok(false);
            }
            Opcode::IndexGet => {
                let object = self.read(*s, r(b))?;
                let index = self.read(*s, r(c))?;
                let object = if self.is_text(object, ObjectKind::Host) {
                    self.heap.host_fields(object).map_err(heap_fault)?
                } else {
                    object
                };
                if self.is_text(object, ObjectKind::Table) {
                    self.start_table(s, object, index, TableAction::Get(r(a)))?;
                    return Ok(false);
                }
                let value = self.sequence_get(object, index)?;
                self.write(*s, r(a), value)?;
            }
            Opcode::IndexSet => {
                let object = self.read(*s, r(a))?;
                let index = self.read(*s, r(b))?;
                if self.is_text(object, ObjectKind::Host) {
                    let fields = self.heap.host_fields(object).map_err(heap_fault)?;
                    self.start_table(
                        s,
                        fields,
                        index,
                        TableAction::SetHost {
                            object: r(a),
                            value: r(c),
                        },
                    )?;
                    return Ok(false);
                }
                if self.is_text(object, ObjectKind::Table) {
                    self.start_table(s, object, index, TableAction::Set(r(c)))?;
                    return Ok(false);
                }
                let value = self.read(*s, r(c))?;
                self.sequence_set(object, index, value)?;
            }
            Opcode::LoadConst => {
                let v = self.slots.read(at(self.constants, b)).map_err(heap_fault)?;
                self.write(*s, r(a), v)?;
            }
            Opcode::Move => {
                let v = self.read(*s, r(b))?;
                self.write(*s, r(a), v)?;
            }
            Opcode::Clear => self
                .slots
                .clear(&mut self.heap, at(s.regs, a))
                .map_err(heap_fault)?,
            Opcode::CellNew => {
                let binding = f.bindings[a as usize].clone();
                let cell = self
                    .heap
                    .alloc_language_cell(self.contract(
                        ModuleId(s.module.0),
                        binding.ty,
                        binding.mutable,
                    ))
                    .map_err(heap_fault)?;
                self.write(*s, binding.storage.reg(), cell)?;
            }
            Opcode::CellRead => {
                let cell = self.read(*s, r(b))?;
                let v = self.heap.cell_read(cell).map_err(heap_fault)?;
                self.write(*s, r(a), v)?;
            }
            Opcode::CellWrite => {
                let cell = self.read(*s, r(a))?;
                let v = self.read(*s, r(b))?;
                let contract = self.heap.cell_info(cell).map_err(heap_fault)?.contract;
                self.check_cell_type(contract, v)?;
                self.heap.cell_write(cell, v).map_err(heap_fault)?;
            }
            Opcode::BindInit | Opcode::BindWrite => {
                let binding = f.bindings[a as usize].clone();
                let v = self.read(*s, r(b))?;
                self.check_type(binding.ty, v)?;
                match binding.storage {
                    BindingStorage::Cell(reg) => {
                        let cell = self.read(*s, reg)?;
                        if op == Opcode::BindInit {
                            self.heap.cell_initialize(cell, v)
                        } else {
                            self.heap.cell_write(cell, v)
                        }
                        .map_err(heap_fault)?;
                    }
                    BindingStorage::Value(reg) => {
                        let old = self.slots.read(at(s.regs, reg.0));
                        if op == Opcode::BindInit {
                            if old.is_ok() {
                                return Err(FaultKind::InvalidState);
                            }
                        } else {
                            old.map_err(heap_fault)?;
                            if !binding.mutable {
                                return Err(FaultKind::ReadOnly);
                            }
                        }
                        self.write(*s, reg, v)?;
                    }
                }
            }
            Opcode::BindRead => {
                let binding = f.bindings[b as usize].clone();
                let mut v = self.read(*s, binding.storage.reg())?;
                if matches!(binding.storage, BindingStorage::Cell(_)) {
                    v = self.heap.cell_read(v).map_err(heap_fault)?;
                }
                self.write(*s, r(a), v)?;
            }
            Opcode::CaptureRead => {
                if !s.closure {
                    return Err(FaultKind::InvalidState);
                }
                let closure = self
                    .script_closure(self.slots.read(at(s.env, 0)).map_err(heap_fault)?)
                    .map_err(vm_fault)?;
                let v = self.heap.get_field(closure, b + 3).map_err(heap_fault)?;
                self.write(*s, r(a), v)?;
            }
            Opcode::GlobalRead | Opcode::GlobalInit | Opcode::GlobalWrite => {
                let Descriptor::Global { module, global } = f.descriptors[b as usize] else {
                    unreachable!()
                };
                let binding = self
                    .program
                    .global_binding(module, global)
                    .expect("verified module binding");
                let cell = self
                    .slots
                    .read(at(self.globals, binding.offset))
                    .map_err(heap_fault)?;
                if op == Opcode::GlobalRead {
                    let v = self.heap.cell_read(cell).map_err(|error| {
                        if s.context.init && matches!(error, HeapError::UninitializedCell) {
                            FaultKind::ModuleInitializationCycle
                        } else {
                            heap_fault(error)
                        }
                    })?;
                    self.write(*s, r(a), v)?;
                } else {
                    let v = self.read(*s, r(a))?;
                    if binding.imported {
                        return Err(FaultKind::ReadOnly);
                    }
                    self.check_type(binding.ty, v)?;
                    if op == Opcode::GlobalInit {
                        self.heap.cell_initialize(cell, v)
                    } else {
                        self.heap.cell_write(cell, v)
                    }
                    .map_err(heap_fault)?;
                }
            }
            Opcode::Jump => {
                s.pc = Pc(a);
                return Ok(false);
            }
            Opcode::BranchBool => {
                let condition = self.read(*s, r(a))?.as_bool().ok_or(FaultKind::Type)?;
                s.pc = Pc(if condition { b } else { c });
                return Ok(false);
            }
            Opcode::BranchNull => {
                s.pc = Pc(if self.read(*s, r(a))?.tag() == ValueTag::Null {
                    b
                } else {
                    c
                });
                return Ok(false);
            }
            Opcode::Neg | Opcode::Pos | Opcode::Not | Opcode::BitNot => {
                let v = unary(op, self.read(*s, r(b))?)?;
                self.write(*s, r(a), v)?;
            }
            Opcode::Is => {
                let v = self.read(*s, r(b))?;
                let result = self.check_type(TypeId(c), v).is_ok();
                self.write(*s, r(a), Value::bool(result))?;
            }
            Opcode::Add
            | Opcode::Sub
            | Opcode::Mul
            | Opcode::Div
            | Opcode::Rem
            | Opcode::BitAnd
            | Opcode::BitOr
            | Opcode::BitXor
            | Opcode::Shl
            | Opcode::Shr
            | Opcode::Eq
            | Opcode::Ne
            | Opcode::Lt
            | Opcode::Le
            | Opcode::Gt
            | Opcode::Ge => {
                let left = self.read(*s, r(b))?;
                let right = self.read(*s, r(c))?;
                if matches!(op, Opcode::Eq | Opcode::Ne)
                    && left.tag() == ValueTag::Object
                    && right.tag() == ValueTag::Object
                    && matches!(
                        self.heap.info(left).map_err(heap_fault)?.kind,
                        ObjectKind::Tuple | ObjectKind::String | ObjectKind::Bytes
                    )
                {
                    self.active.equality_cursors.data.clear();
                    self.put(self.active.equality_roots.unwrap(), 0, left)
                        .map_err(vm_fault)?;
                    self.put(self.active.equality_roots.unwrap(), 1, right)
                        .map_err(vm_fault)?;
                    self.active.equality_cursors.data.push(0);
                    s.phase = ScriptPhase::Equality {
                        dest: r(a),
                        negated: op == Opcode::Ne,
                    };
                    return Ok(false);
                }
                if op == Opcode::Add
                    && self.is_text(left, ObjectKind::String)
                    && self.is_text(right, ObjectKind::String)
                {
                    if s.effects.0 & Effects::ALLOCATE == 0 {
                        return Err(FaultKind::Effect);
                    }
                    self.heap
                        .info(left)
                        .map_err(heap_fault)?
                        .len
                        .checked_add(self.heap.info(right).map_err(heap_fault)?.len)
                        .ok_or(FaultKind::Resource)?;
                    let builder = self.heap.alloc_byte_buffer(&[]).map_err(heap_fault)?;
                    self.put(s.env, 1, builder).map_err(vm_fault)?;
                    s.phase = ScriptPhase::Concat {
                        dest: r(a),
                        left: r(b),
                        right: r(c),
                        offset: 0,
                    };
                    return Ok(false);
                }
                let v = binary(op, left, right)?;
                self.write(*s, r(a), v)?;
            }
            Opcode::MakeClosure => {
                let Descriptor::Closure { function, captures } = &f.descriptors[b as usize] else {
                    unreachable!()
                };
                let function = *function;
                self.scratch.data.clear();
                self.scratch.data.extend([
                    Value::int(function.0 as i64),
                    Value::int(s.code.version as i64),
                    Value::int(self.program.package().functions[function.index()].module.0 as i64),
                ]);
                for source in captures {
                    let value = match *source {
                        CaptureSource::Binding(id) => {
                            let binding = &f.bindings[id.index()];
                            self.read(*s, binding.storage.reg())?
                        }
                        CaptureSource::Capture(id) => {
                            let closure = self
                                .script_closure(self.slots.read(at(s.env, 0)).map_err(heap_fault)?)
                                .map_err(vm_fault)?;
                            self.heap.get_field(closure, id.0 + 3).map_err(heap_fault)?
                        }
                    };
                    self.scratch.data.push(value);
                }
                let closure = self
                    .heap
                    .alloc_record(ObjectKind::Closure, &self.scratch.data)
                    .map_err(heap_fault)?;
                self.scratch.data.clear();
                self.write(*s, r(a), closure)?;
            }
            Opcode::ArgsNew => {
                let packed = self.heap.alloc_array(&[]).map_err(heap_fault)?;
                self.write(*s, r(a), packed)?;
            }
            Opcode::ArgsPush | Opcode::ArgsNamed => {
                let packed = self.read(*s, r(a))?;
                let v = self.read(*s, r(b))?;
                let len = self.heap.packed_array_len(packed).map_err(heap_fault)?;
                if len / 2 >= self.limits.max_arguments {
                    return Err(FaultKind::Resource);
                }
                self.heap
                    .packed_pair_push(
                        packed,
                        Value::int(if op == Opcode::ArgsNamed {
                            c as i64
                        } else {
                            -1
                        }),
                        v,
                    )
                    .map_err(heap_fault)?;
            }
            Opcode::ArgsSpread | Opcode::ArraySpread => {
                let packed = self.read(*s, r(a))?;
                let source = self.read(*s, r(b))?;
                let arguments = op == Opcode::ArgsSpread;
                let named = arguments && self.is_text(source, ObjectKind::Table);
                if !named
                    && !self.is_text(source, ObjectKind::Array)
                    && !self.is_text(source, ObjectKind::Tuple)
                {
                    return Err(FaultKind::Type);
                }
                let info = self.heap.info(source).map_err(heap_fault)?;
                if !self.is_text(packed, ObjectKind::Array) {
                    return Err(FaultKind::Type);
                }
                let destination_version =
                    self.heap.info(packed).map_err(heap_fault)?.content_version;
                if arguments {
                    let count = self.heap.packed_array_len(packed).map_err(heap_fault)? / 2;
                    if info.len > self.limits.max_arguments.saturating_sub(count) {
                        return Err(FaultKind::Resource);
                    }
                }
                self.put(s.env, 1, packed).map_err(vm_fault)?;
                self.put(s.env, 2, source).map_err(vm_fault)?;
                s.phase = ScriptPhase::Spread(collections::SpreadCapture {
                    kind: if named {
                        collections::CaptureKind::NamedArguments
                    } else if arguments {
                        collections::CaptureKind::PositionalArguments
                    } else {
                        collections::CaptureKind::Elements
                    },
                    index: 0,
                    len: info.len,
                    version: info.content_version,
                    destination_version,
                });
                return Ok(false);
            }
            Opcode::Call | Opcode::CallNoWait => {
                let Descriptor::Call(desc) = &f.descriptors[b as usize] else {
                    unreachable!()
                };
                let target = desc.target;
                let count = match desc.arguments {
                    Arguments::Window(w) => w.len,
                    Arguments::Packed(r) => {
                        let v = self.read(*s, r)?;
                        self.heap.packed_array_len(v).map_err(heap_fault)? / 2
                    }
                };
                let source = ArgSource::Caller {
                    frame: s.id,
                    descriptor: DescriptorId(b),
                };
                let mut caller = *s;
                if op == Opcode::CallNoWait
                    || self.active.unwinds.data.iter().any(|u| u.frame == s.id)
                {
                    caller.context = inherit(caller.context, Context::NoWait);
                }
                let dynamic = if let CallTarget::Register(reg) = target {
                    let closure = self.read(*s, reg)?;
                    if self.is_text(closure, ObjectKind::Class) {
                        self.start_construct(s, closure, r(a), DescriptorId(b), count)?;
                        return Ok(false);
                    }
                    if self.is_text(closure, ObjectKind::BoundBuiltin) {
                        self.start_builtin(s, closure, DescriptorId(b), r(a), count)?;
                        return Ok(false);
                    }
                    Some((
                        closure,
                        self.closure_function(closure).map_err(|error| {
                            if matches!(error, VmError::Heap(HeapError::ConstructionFailed)) {
                                FaultKind::ConstructionFailed
                            } else {
                                FaultKind::Type
                            }
                        })?,
                    ))
                } else {
                    None
                };
                s.pc.0 += 1;
                let caller_index = self.index(s.id).map_err(|_| FaultKind::InvalidState)?;
                self.active.frames.data[caller_index] = Frame::Script(*s);
                match target {
                    CallTarget::Native(native) => {
                        let site = NativeSite {
                            owner: caller.owner,
                            module: caller.module,
                            context: caller.context,
                            effects: caller.effects,
                            location: self.loc(caller, caller.pc),
                            ret: ReturnTo::Register {
                                frame: caller.id,
                                reg: r(a),
                                call: caller.pc,
                            },
                        };
                        if let Err(e) = self.push_native(native, source, count, site) {
                            s.pc = caller.pc;
                            return Err(vm_fault(e));
                        }
                    }
                    CallTarget::Function(function) => {
                        if let Err(e) = self.push_script(
                            function,
                            None,
                            source,
                            count,
                            s.owner,
                            caller.context,
                            s.effects,
                            ReturnTo::Register {
                                frame: s.id,
                                reg: r(a),
                                call: caller.pc,
                            },
                        ) {
                            s.pc = caller.pc;
                            return Err(vm_fault(e));
                        }
                    }
                    CallTarget::Register(_) => {
                        let (closure, function) = dynamic.unwrap();
                        if let Err(e) = self.push_script(
                            function,
                            Some(closure),
                            source,
                            count,
                            s.owner,
                            caller.context,
                            s.effects,
                            ReturnTo::Register {
                                frame: s.id,
                                reg: r(a),
                                call: caller.pc,
                            },
                        ) {
                            s.pc = caller.pc;
                            return Err(vm_fault(e));
                        }
                    }
                }
                return Ok(true);
            }
            Opcode::Await => {
                self.start_await(s, r(a), r(b))?;
                return Ok(true);
            }
            Opcode::Return => {
                let v = self.read(*s, r(a))?;
                let signature = self.program.package().functions[s.code.function.index()].signature;
                self.check_type(
                    self.program.package().signatures[signature.index()].return_type,
                    v,
                )?;
                self.slots
                    .write(&mut self.heap, at(s.env, 1), v)
                    .map_err(heap_fault)?;
                s.phase = ScriptPhase::Exit {
                    origin: s.pc,
                    reason: ExitReason::Return,
                    skip: None,
                };
                return Ok(false);
            }
            Opcode::MatchFail => return Err(FaultKind::MatchError),
            Opcode::Throw => {
                let v = self.read(*s, r(a))?;
                self.put(s.env, 1, v).map_err(vm_fault)?;
                self.clear(s.env, 2).map_err(vm_fault)?;
                s.phase = ScriptPhase::Exit {
                    origin: s.pc,
                    reason: ExitReason::Throw(FaultSeed {
                        kind: FaultKind::Thrown,
                        location: self.loc(*s, s.pc),
                        fault: None,
                        explicit: None,
                        author_value: true,
                        awaited: None,
                    }),
                    skip: None,
                };
                return Ok(false);
            }
            Opcode::Rethrow => {
                let region = f
                    .regions
                    .iter()
                    .enumerate()
                    .filter_map(|(id, r)| {
                        r.catch
                            .as_ref()
                            .filter(|c| c.range.contains(s.pc))
                            .map(|c| (RegionId(id as u32), c.range.end.0 - c.range.start.0))
                    })
                    .min_by_key(|(_, width)| *width)
                    .map(|(id, _)| id)
                    .ok_or(FaultKind::InvalidState)?;
                let seed = self
                    .active
                    .caught
                    .data
                    .iter()
                    .find(|c| c.frame == s.id && c.region == region)
                    .map(|c| c.info)
                    .ok_or(FaultKind::InvalidState)?;
                let id = seed.fault.ok_or(FaultKind::InvalidState)?;
                let value = self.heap.fault_value(id).map_err(heap_fault)?;
                self.put(s.env, 1, value).map_err(vm_fault)?;
                self.put(s.env, 2, id.as_value()).map_err(vm_fault)?;
                s.phase = ScriptPhase::Exit {
                    origin: s.pc,
                    reason: ExitReason::Throw(seed),
                    skip: None,
                };
                return Ok(false);
            }
            Opcode::Leave => {
                self.slots
                    .write(&mut self.heap, at(s.env, 1), Value::NULL)
                    .map_err(heap_fault)?;
                s.phase = ScriptPhase::Exit {
                    origin: s.pc,
                    reason: ExitReason::Leave(Pc(a)),
                    skip: None,
                };
                return Ok(false);
            }
            Opcode::EndFinally => {
                let Some(u) = self
                    .active
                    .unwinds
                    .data
                    .last()
                    .copied()
                    .filter(|u| u.frame == s.id && u.region == RegionId(a))
                else {
                    return Err(FaultKind::InvalidState);
                };
                let value = self.slots.read(at(u.value, 0)).map_err(heap_fault)?;
                self.slots
                    .write(&mut self.heap, at(s.env, 1), value)
                    .map_err(heap_fault)?;
                if matches!(u.reason,ExitReason::Throw(info) if info.fault.is_some()) {
                    let cause = self.value(u.value, 1).map_err(vm_fault)?;
                    self.put(s.env, 2, cause).map_err(vm_fault)?;
                } else {
                    self.clear(s.env, 2).map_err(vm_fault)?;
                }
                self.slots
                    .release(&mut self.heap, u.value)
                    .map_err(heap_fault)?;
                self.active.unwinds.data.pop();
                s.phase = ScriptPhase::Exit {
                    origin: u.origin,
                    reason: u.reason,
                    skip: Some(u.region),
                };
                return Ok(false);
            }
            _ => return Err(FaultKind::InvalidState),
        }
        s.pc.0 += 1;
        Ok(false)
    }
    fn is_text(&self, v: Value, kind: ObjectKind) -> bool {
        v.tag() == ValueTag::Object && self.heap.info(v).is_ok_and(|i| i.kind == kind)
    }
    fn equality_step(
        &mut self,
        s: &mut ScriptFrame,
        dest: Reg,
        negated: bool,
    ) -> Result<(), VmError> {
        if self.table_changed(*s)? {
            s.phase = ScriptPhase::EqualityCleanup {
                dest,
                negated,
                result: Err(FaultKind::ConcurrentModification),
            };
            return Ok(());
        }
        let depth = self.active.equality_cursors.data.len() as u32 - 1;
        let offset = self.active.equality_cursors.data[depth as usize];
        let left = self.value(self.active.equality_roots.unwrap(), depth * 2)?;
        let right = self.value(self.active.equality_roots.unwrap(), depth * 2 + 1)?;
        let result = if left.tag() == ValueTag::Object && right.tag() == ValueTag::Object {
            let l = self.heap.info(left)?;
            let r = self.heap.info(right)?;
            if l.kind != r.kind {
                Some(false)
            } else {
                match l.kind {
                    ObjectKind::String | ObjectKind::Bytes => {
                        if l.len != r.len {
                            Some(false)
                        } else if offset == l.len {
                            Some(true)
                        } else {
                            let mut a = [0];
                            let mut b = [0];
                            self.heap.read_bytes(left, offset, &mut a)?;
                            self.heap.read_bytes(right, offset, &mut b)?;
                            self.active.equality_cursors.data[depth as usize] += 1;
                            if a != b { Some(false) } else { None }
                        }
                    }
                    ObjectKind::Tuple => {
                        if l.len != r.len {
                            Some(false)
                        } else if offset == l.len {
                            Some(true)
                        } else {
                            if self.active.equality_cursors.data.len()
                                == self.limits.max_unwinds as usize
                            {
                                s.phase = ScriptPhase::EqualityCleanup {
                                    dest,
                                    negated,
                                    result: Err(FaultKind::Resource),
                                };
                                return Ok(());
                            }
                            let a = self.heap.get_field(left, offset)?;
                            let b = self.heap.get_field(right, offset)?;
                            self.active.equality_cursors.data[depth as usize] += 1;
                            self.put(self.active.equality_roots.unwrap(), (depth + 1) * 2, a)?;
                            self.put(self.active.equality_roots.unwrap(), (depth + 1) * 2 + 1, b)?;
                            self.active.equality_cursors.data.push(0);
                            None
                        }
                    }
                    _ => Some(left.same_bits(right)),
                }
            }
        } else {
            Some(
                binary(Opcode::Eq, left, right)
                    .map_err(|_| VmError::Contract("equality scalar"))?
                    .as_bool()
                    .unwrap(),
            )
        };
        if let Some(equal) = result {
            if !equal {
                s.phase = ScriptPhase::EqualityCleanup {
                    dest,
                    negated,
                    result: Ok(false),
                };
            } else {
                self.clear(self.active.equality_roots.unwrap(), depth * 2)?;
                self.clear(self.active.equality_roots.unwrap(), depth * 2 + 1)?;
                self.active.equality_cursors.data.pop();
                if self.active.equality_cursors.data.is_empty() {
                    s.phase = ScriptPhase::EqualityCleanup {
                        dest,
                        negated,
                        result: Ok(true),
                    };
                }
            }
        }
        Ok(())
    }
    fn select_exit_handler(
        &self,
        s: ScriptFrame,
        origin: Pc,
        reason: ExitReason,
        skip: Option<RegionId>,
    ) -> Result<Option<HandlerTarget>, VmError> {
        #[cfg(test)]
        self.work_probe
            .exit_handler_queries
            .set(self.work_probe.exit_handler_queries.get() + 1);
        Ok(match reason {
            ExitReason::Throw(_) => {
                self.program
                    .exception_handler(s.code.function, origin, skip)?
            }
            ExitReason::Return => self.program.cleanup_handler(
                s.code.function,
                origin,
                CleanupDestination::Return,
                skip,
            )?,
            ExitReason::Leave(target) => self.program.cleanup_handler(
                s.code.function,
                origin,
                CleanupDestination::Leave(target),
                skip,
            )?,
        })
    }
    fn superseded_unwind(
        &self,
        s: ScriptFrame,
        origin: Pc,
        reason: ExitReason,
        handler: Option<HandlerTarget>,
    ) -> Option<Unwind> {
        let u = self
            .active
            .unwinds
            .data
            .last()
            .copied()
            .filter(|u| u.frame == s.id)?;
        let range = self.program.package().functions[s.code.function.index()].regions
            [u.region.index()]
        .finally
        .unwrap();
        (range.contains(origin)
            && !handler.is_some_and(|h| range.contains(h.pc))
            && !matches!(reason, ExitReason::Leave(target) if range.contains(target)))
        .then_some(u)
    }
    fn exit_step(
        &mut self,
        s: &mut ScriptFrame,
        origin: Pc,
        reason: ExitReason,
        handler: Option<HandlerTarget>,
        superseded: Option<Unwind>,
    ) -> Result<bool, VmError> {
        // A new error/return inside an active finally supersedes that exit. Nested
        // ordinary Leave stays within the outer cleanup and preserves its record.
        if let Some(u) = superseded {
            if let ExitReason::Throw(info) = reason {
                let id = info
                    .fault
                    .ok_or(VmError::Contract("incomplete unwind fault"))?;
                let (kind, old_fault, old_value) = match u.reason {
                    ExitReason::Throw(old) => (
                        CauseKind::ReplacedExit(ReplacedExit::Throw(
                            old.fault
                                .ok_or(VmError::Contract("incomplete pending fault"))?,
                        )),
                        old.fault,
                        Value::NULL,
                    ),
                    ExitReason::Return => (
                        CauseKind::ReplacedExit(ReplacedExit::Return),
                        None,
                        self.value(u.value, 0)?,
                    ),
                    ExitReason::Leave(target) => (
                        CauseKind::ReplacedExit(ReplacedExit::Leave {
                            code: s.code,
                            target,
                        }),
                        None,
                        Value::NULL,
                    ),
                };
                let event = if let Some(e) = u.replacement_event {
                    e
                } else {
                    let e = self.serial()?;
                    self.active
                        .unwinds
                        .data
                        .last_mut()
                        .unwrap()
                        .replacement_event = Some(e);
                    e
                };
                if let Err(e) = self.heap.append_fault_cause(
                    id,
                    CauseInfo {
                        event,
                        kind,
                        location: self.loc(*s, origin),
                        fault: old_fault,
                        value: old_value,
                    },
                    self.program.clone(),
                ) {
                    self.block_heap(e)?;
                    return Ok(true);
                }
            }
            self.slots.release(&mut self.heap, u.value)?;
            self.active.unwinds.data.pop();
            return Ok(true);
        }
        let target = handler.map(|h| h.pc).or(match reason {
            ExitReason::Leave(target) => Some(target),
            _ => None,
        });
        if self.release_exited_catch(*s, target)? {
            return Ok(true);
        }
        if let Some(handler) = handler {
            let value = self.value(s.env, 1)?;
            match handler.kind {
                HandlerKind::Catch => {
                    let ExitReason::Throw(info) = reason else {
                        return Err(VmError::Contract("catch on normal exit"));
                    };
                    let base = 4 + handler.region.0 * 2;
                    self.put(s.env, base, value)?;
                    if info.fault.is_some() {
                        let cause = self.value(s.env, 2)?;
                        self.put(s.env, base + 1, cause)?;
                    } else {
                        self.clear(s.env, base + 1)?;
                    }
                    if let Some(index) = self
                        .active
                        .caught
                        .data
                        .iter()
                        .position(|c| c.frame == s.id && c.region == handler.region)
                    {
                        self.active.caught.data[index].info = info;
                    } else {
                        if self.active.caught.data.len() == self.limits.max_unwinds as usize {
                            self.block(
                                BlockedResource::CatchRecords,
                                self.active.caught.data.len() as u64,
                                self.active.caught.data.len() as u64 + 1,
                            );
                            return Ok(true);
                        }
                        let activation = self.serial()?;
                        self.active.caught.data.push(CaughtFault {
                            frame: s.id,
                            region: handler.region,
                            info,
                            activation,
                        });
                    }
                    self.put(s.regs, handler.exception.unwrap().0, value)?;
                    self.clear(s.env, 1)?;
                    self.clear(s.env, 2)?;
                    s.pc = handler.pc;
                    s.phase = ScriptPhase::Body;
                }
                HandlerKind::Finally => {
                    if self.active.unwinds.data.len() == self.limits.max_unwinds as usize {
                        self.block(
                            BlockedResource::UnwindRecords,
                            self.active.unwinds.data.len() as u64,
                            self.active.unwinds.data.len() as u64 + 1,
                        );
                        return Ok(true);
                    }
                    let root = match self.open(s.owner, RootKind::Exception, 2) {
                        Ok(r) => r,
                        Err(e) => {
                            match e {
                                VmError::Heap(e) => self.block_heap(e)?,
                                other => return Err(other),
                            };
                            return Ok(true);
                        }
                    };
                    self.put(root, 0, value)?;
                    if matches!(reason,ExitReason::Throw(info) if info.fault.is_some()) {
                        let cause = self.value(s.env, 2)?;
                        self.put(root, 1, cause)?;
                    }
                    self.active.unwinds.data.push(Unwind {
                        frame: s.id,
                        origin,
                        reason,
                        region: handler.region,
                        value: root,
                        replacement_event: None,
                    });
                    self.clear(s.env, 1)?;
                    self.clear(s.env, 2)?;
                    s.pc = handler.pc;
                    s.phase = ScriptPhase::Body;
                }
            }
        } else {
            match reason {
                ExitReason::Leave(target) => {
                    s.pc = target;
                    s.phase = ScriptPhase::Body;
                }
                _ => s.phase = ScriptPhase::Release { reason },
            }
        }
        Ok(true)
    }
    fn commit(
        &mut self,
        ret: ReturnTo,
        value: Value,
        cause: Option<Value>,
        error: Option<FaultSeed>,
    ) -> Result<bool, VmError> {
        if let (ReturnTo::Native { frame }, Some(seed)) = (ret, error)
            && let Some(id) = seed.fault
        {
            let Frame::Native(n) = self.active.frames.data[self.index(frame)?] else {
                return Err(VmError::Contract("callback target"));
            };
            if !self.native_fault_slot_holds(n, id)?
                && n.fault_count == self.limits.max_native_faults
            {
                self.block(
                    BlockedResource::NativeFaultSlots { frame },
                    n.fault_count as u64,
                    n.fault_count as u64 + 1,
                );
                return Ok(false);
            }
        }
        match ret {
            ReturnTo::Delivery => {
                self.put(self.active.delivery.unwrap(), 0, value)?;
                if let Some(c) = cause {
                    self.put(self.active.delivery.unwrap(), 1, c)?;
                } else {
                    self.clear(self.active.delivery.unwrap(), 1)?;
                }
                let serial = self.serial()?;
                self.active.terminal = Some(Terminal {
                    id: DeliveryId {
                        task: self.active.task_id.unwrap(),
                        heap: self.heap.id(),
                        epoch: self.heap.epoch(),
                        serial,
                    },
                    fault: error,
                });
            }
            ReturnTo::Register { frame, call, .. }
            | ReturnTo::Default { frame, call, .. }
            | ReturnTo::Class { frame, call } => {
                let i = self.index(frame)?;
                let Frame::Script(mut parent) = self.active.frames.data[i] else {
                    return Err(VmError::Contract("return target"));
                };
                if let Some(info) = error {
                    self.abort_class(parent)?;
                    self.put(parent.env, 1, value)?;
                    if let Some(c) = cause {
                        self.put(parent.env, 2, c)?;
                    } else {
                        self.clear(parent.env, 2)?;
                    }
                    parent.phase = match ret {
                        ReturnTo::Default { .. } => ScriptPhase::Release {
                            reason: ExitReason::Throw(info),
                        },
                        _ => ScriptPhase::Exit {
                            origin: call,
                            reason: ExitReason::Throw(info),
                            skip: None,
                        },
                    };
                } else {
                    match ret {
                        ReturnTo::Register { reg, .. } => self.put(parent.regs, reg.0, value)?,
                        ReturnTo::Class { .. } => self.put(parent.env, 2, value)?,
                        ReturnTo::Default { parameter, .. } => {
                            self.put(parent.bound, parameter, value)?;
                            parent.phase = ScriptPhase::Defaults { parameter };
                        }
                        _ => unreachable!(),
                    }
                }
                self.active.frames.data[i] = Frame::Script(parent);
            }
            ReturnTo::Native { frame } => {
                let i = self.index(frame)?;
                let Frame::Native(mut n) = self.active.frames.data[i] else {
                    return Err(VmError::Contract("native callback return target"));
                };
                self.put(n.roots, 0, value)?;
                let cause_slot =
                    2 + self.limits.max_continuation_values + self.limits.max_arguments;
                self.put(n.roots, cause_slot, cause.unwrap_or(Value::NULL))?;
                n.callback_fault = error.and_then(|s| s.fault);
                if let Some(id) = n.callback_fault {
                    self.push_native_fault(&mut n, id)?;
                }
                self.clear(n.roots, self.native_pending_slot())?;
                n.phase = NativePhase::Resume(if error.is_some() {
                    NativeInput::CallbackFault
                } else {
                    NativeInput::CallbackReturned
                });
                self.active.frames.data[i] = Frame::Native(n);
            }
            ReturnTo::NativeDefault { frame, parameter } => {
                let i = self.index(frame)?;
                let Frame::Native(mut n) = self.active.frames.data[i] else {
                    return Err(VmError::Contract("native default target"));
                };
                if let Some(seed) = error {
                    self.put(n.roots, 0, value)?;
                    self.put(
                        n.roots,
                        self.native_pending_slot(),
                        cause.unwrap_or(Value::NULL),
                    )?;
                    n.pending_fault = Some(seed);
                    n.phase = NativePhase::Commit(true);
                } else {
                    self.put(n.bound, parameter, value)?;
                    n.phase = NativePhase::Defaults { parameter };
                }
                self.active.frames.data[i] = Frame::Native(n);
            }
        }
        Ok(true)
    }
    fn native_step(
        &mut self,
        mut n: NativeFrame,
        m: &mut Meter,
    ) -> Result<Option<RunOutcome>, VmError> {
        let top = self.active.frames.data.len() - 1;
        match n.phase {
            NativePhase::Copy { source, index } => {
                if !m.take(0, 1, 0) {
                    return Ok(Some(RunOutcome::BudgetYield(m.usage)));
                }
                if index < n.argument_count {
                    match self.argument(source, index) {
                        Ok((name, value)) => {
                            self.put(n.args, index * 2, name)?;
                            self.put(n.args, index * 2 + 1, value)?;
                            n.phase = NativePhase::Copy {
                                source,
                                index: index + 1,
                            };
                        }
                        Err(e) => {
                            let kind = language_fault(e)?;
                            self.native_fault(&mut n, kind)?;
                        }
                    }
                } else {
                    // FrameSlots::open initialized the complete physical range
                    // and its bitmap under native_push_cost. Re-publishing each
                    // unused NULL only satisfied the former whole-window borrow.
                    n.phase = if self.program.package().signatures[n.signature.index()]
                        .rest
                        .is_some()
                    {
                        NativePhase::Rest
                    } else {
                        NativePhase::Bind(ArgumentBinding::new(u32::from(n.receiver)))
                    };
                }
            }
            NativePhase::Bind(mut binding) => {
                if binding.index < n.argument_count {
                    let result =
                        self.bind_argument_step(n.signature, n.bound, n.args, &mut binding, m)?;
                    n.phase = NativePhase::Bind(binding);
                    match result {
                        ArgumentStep::Blocked => {
                            self.active.frames.data[top] = Frame::Native(n);
                            return Ok(Some(RunOutcome::BudgetYield(m.usage)));
                        }
                        ArgumentStep::Ready(Err(kind)) => self.native_fault(&mut n, kind)?,
                        _ => {}
                    }
                } else {
                    let sig = &self.program.package().signatures[n.signature.index()];
                    // The no-default path checks the whole initialized window.
                    if !m.take(0, sig.parameters.len() as u64 + 3, 0) {
                        return Ok(Some(RunOutcome::BudgetYield(m.usage)));
                    }
                    if n.has_defaults || sig.rest.is_some() {
                        n.phase = NativePhase::Defaults { parameter: 0 };
                    } else if self.slots.borrow_initialized(n.bound).is_err() {
                        self.native_fault(&mut n, FaultKind::MissingArgument)?;
                    } else {
                        n.phase = NativePhase::Resume(NativeInput::Entry);
                    }
                }
            }
            NativePhase::Rest
            | NativePhase::Defaults { .. }
            | NativePhase::Parameter { .. }
            | NativePhase::DefaultCall { .. }
            | NativePhase::Project { .. } => {
                if !self.native_parameter_step(&mut n, m)? {
                    return Ok(Some(RunOutcome::BudgetYield(m.usage)));
                }
            }
            NativePhase::Resume(input) | NativePhase::Issue(input) => {
                if !m.take(0, 1, 0) {
                    return Ok(Some(RunOutcome::BudgetYield(m.usage)));
                }
                if matches!(n.phase, NativePhase::Resume(_)) && n.bindings.is_some() {
                    n.phase = NativePhase::Project { index: 0, input };
                    self.active.frames.data[top] = Frame::Native(n);
                    return Ok(None);
                }
                let serial = self.serial()?;
                let ticket = self.make_ticket(n, self.active.task_id.unwrap(), serial, input);
                if input == NativeInput::Entry {
                    self.native_entries += 1;
                } else {
                    self.native_resumes += 1;
                }
                n.phase = NativePhase::Issued { serial, input };
                self.active.frames.data[top] = Frame::Native(n);
                return Ok(Some(RunOutcome::HostCall(ticket)));
            }
            NativePhase::Issued { serial, input } => {
                return Ok(Some(RunOutcome::HostCall(self.make_ticket(
                    n,
                    self.active.task_id.unwrap(),
                    serial,
                    input,
                ))));
            }
            NativePhase::Waiting(wait) => return Ok(Some(RunOutcome::Waiting(wait))),
            NativePhase::Continue => {
                if !m.take(0, 1, 0) {
                    return Ok(Some(RunOutcome::BudgetYield(m.usage)));
                }
                n.phase = NativePhase::Resume(NativeInput::Continue {
                    version: n.continuation_version,
                    stage: n.continuation_stage,
                });
            }
            NativePhase::Callback { count } => {
                let closure = self.value(n.roots, 1)?;
                let function = self.closure_function(closure)?;
                let cost = self.script_push_cost(function, count);
                if !m.take(0, cost, 0) {
                    return Ok(Some(RunOutcome::BudgetYield(m.usage)));
                }
                self.active.frames.data[top] = Frame::Native(n);
                let prepared = self.push_script(
                    function,
                    Some(closure),
                    ArgSource::Native { frame: n.id },
                    count,
                    n.owner,
                    n.context,
                    n.effects,
                    ReturnTo::Native { frame: n.id },
                );
                match prepared {
                    Ok(()) => return Ok(None),
                    Err(e) => {
                        let kind = language_fault(e)?;
                        self.put(n.roots, 0, Value::NULL)?;
                        n.pending_fault = Some(FaultSeed {
                            fault: None,
                            explicit: None,
                            author_value: false,
                            awaited: None,
                            kind,
                            location: n.location,
                        });
                        n.phase = NativePhase::CallbackFaultBuild;
                    }
                }
            }
            NativePhase::CallbackFaultBuild | NativePhase::WaitingFaultBuild => {
                if !m.take(
                    0,
                    2 * (n.fault_count as u64 + n.continuation_len as u64) + 8,
                    0,
                ) {
                    return Ok(Some(RunOutcome::BudgetYield(m.usage)));
                }
                let id = n
                    .pending_fault
                    .and_then(|f| f.fault)
                    .ok_or(VmError::Contract("unbuilt callback fault"))?;
                if !self.native_fault_slot_holds(n, id)?
                    && n.fault_count == self.limits.max_native_faults
                {
                    self.block(
                        BlockedResource::NativeFaultSlots { frame: n.id },
                        n.fault_count as u64,
                        n.fault_count as u64 + 1,
                    );
                    return Ok(None);
                }
                self.push_native_fault(&mut n, id)?;
                n.callback_fault = Some(id);
                n.pending_fault = None;
                self.clear(n.roots, self.native_pending_slot())?;
                n.phase =
                    NativePhase::Resume(if matches!(n.phase, NativePhase::WaitingFaultBuild) {
                        NativeInput::WaitingFault
                    } else {
                        NativeInput::CallbackFault
                    });
            }
            NativePhase::Commit(error) => {
                let extra = if error {
                    0
                } else {
                    self.type_checks
                        .extra(self.program.package().signatures[n.signature.index()].return_type)
                };
                if !m.take(0, 1 + extra, 0) {
                    return Ok(Some(RunOutcome::BudgetYield(m.usage)));
                }
                let value = self.value(n.roots, 0)?;
                if !error
                    && self
                        .check_type(
                            self.program.package().signatures[n.signature.index()].return_type,
                            value,
                        )
                        .is_err()
                {
                    self.put(n.roots, 0, Value::NULL)?;
                    self.clear(n.roots, self.native_pending_slot())?;
                    n.pending_fault = Some(FaultSeed {
                        fault: None,
                        explicit: None,
                        author_value: false,
                        awaited: None,
                        kind: FaultKind::Type,
                        location: n.location,
                    });
                    n.phase = NativePhase::Release(true);
                } else {
                    n.phase = NativePhase::Release(error);
                }
            }
            NativePhase::Release(error) => {
                if !m.take(
                    0,
                    self.release_cost(n.root_count()) + self.commit_work(n.ret),
                    0,
                ) {
                    return Ok(Some(RunOutcome::BudgetYield(m.usage)));
                }
                let value = self.value(n.roots, 0)?;
                let info = if error {
                    Some(n.pending_fault.unwrap_or(FaultSeed {
                        fault: None,
                        explicit: None,
                        author_value: false,
                        awaited: None,
                        kind: FaultKind::Thrown,
                        location: n.location,
                    }))
                } else {
                    None
                };
                let cause = info.and_then(|seed| seed.fault).map(FaultId::as_value);
                if !self.commit(n.ret, value, cause, info)? {
                    self.active.frames.data[top] = Frame::Native(n);
                    return Ok(None);
                }
                self.release_frame(Frame::Native(n))?;
                self.active.frames.data.remove(top);
                return Ok(None);
            }
        }
        self.active.frames.data[top] = Frame::Native(n);
        Ok(None)
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
}
impl Meter {
    fn take(&mut self, instructions: u32, work: u64, allocation: u64) -> bool {
        self.usage.required_work = work;
        self.usage.required_allocation = allocation;
        if instructions > self.budget.instructions - self.usage.instructions
            || work > self.budget.work - self.usage.work
            || allocation > self.budget.allocation_bytes - self.usage.allocation_bytes
        {
            return false;
        }
        self.usage.instructions += instructions;
        self.usage.work += work;
        self.usage.allocation_bytes += allocation;
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
