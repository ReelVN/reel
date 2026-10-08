//! Budgeted execution of the single shared verified-bytecode format.
//!
//! Values in frames, continuations and terminal deliveries are held in FrameSlots.
//! Host entry and continuation-resume tickets are distinct, single-use identities.
use crate::bytecode::*;
use crate::root_ranges::{ChargedVec, Reservation};
use crate::*;
use std::cmp::Ordering;

mod api;
pub use api::*;

#[derive(Clone, Copy)]
enum ReturnTo {
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
}
#[derive(Clone, Copy)]
enum ArgSource {
    None,
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
#[derive(Clone, Copy)]
enum ScriptPhase {
    Copy {
        source: ArgSource,
        index: u32,
    },
    Bind {
        index: u32,
        positional: u32,
    },
    Defaults {
        parameter: u32,
    },
    Install {
        binding: u32,
    },
    Body,
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
    Release {
        reason: ExitReason,
    },
}
#[derive(Clone, Copy)]
enum ExitReason {
    Return,
    Throw(FaultInfo),
    Leave(Pc),
}
#[derive(Clone, Copy)]
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
    effects: Effects,
    context: ContextFlags,
    owner: RootOwner,
    ret: ReturnTo,
    phase: ScriptPhase,
}
#[derive(Clone, Copy)]
enum NativePhase {
    Copy { source: ArgSource, index: u32 },
    Bind { index: u32, positional: u32 },
    Issued { serial: u64, input: NativeInput },
    Waiting(WaitId),
    Continue,
    Callback { count: u32 },
    Resume(NativeInput),
    Commit(bool),
    Release(bool),
}
#[derive(Clone, Copy)]
struct NativeFrame {
    id: FrameId,
    native: NativeId,
    signature: SignatureId,
    args: RootRangeToken,
    bound: RootRangeToken,
    roots: RootRangeToken,
    argument_count: u32,
    continuation_len: u32,
    continuation_version: u64,
    continuation_stage: u64,
    effects: Effects,
    context: ContextFlags,
    owner: RootOwner,
    location: Location,
    ret: ReturnTo,
    callback_fault: Option<FaultInfo>,
    pending_fault: Option<FaultInfo>,
    phase: NativePhase,
}
#[derive(Clone, Copy)]
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
#[derive(Clone, Copy)]
struct Unwind {
    frame: FrameId,
    origin: Pc,
    reason: ExitReason,
    region: RegionId,
    value: RootRangeToken,
}
#[derive(Clone, Copy)]
struct Terminal {
    id: DeliveryId,
    fault: Option<FaultInfo>,
}

#[derive(Clone, Copy)]
struct CaughtFault {
    frame: FrameId,
    region: RegionId,
    info: FaultInfo,
}
/// One shared heap/module set, with bounded independent task state machines.
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
    cancel_roots: u32,
    release_external: bool,
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
            Some(slots.open(heap, RootOwner(u64::MAX), RootKind::Delivery, 2)?)
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
            cancel_roots: 0,
            release_external: false,
        })
    }
}
/// Task selection swaps bounded state owners; all registered windows remain in
/// the single FrameSlots provider. Field order deallocates before budget refunds.
pub struct Vm {
    program: VerifiedProgram,
    program_charge: Reservation,
    slots: FrameSlots,
    active: TaskState,
    tasks: ChargedVec<TaskState>,
    active_slot: Option<usize>,
    scratch: ChargedVec<Value>,
    text_scratch: ChargedVec<u8>,
    globals: RootRangeToken,
    constants: RootRangeToken,
    heap: Heap,
    limits: VmLimits,
    control_charge: Reservation,
    next_id: u64,
    native_entries: u64,
    native_resumes: u64,
    slices: u64,
    last_usage: SliceUsage,
}
impl Vm {
    pub fn load(
        program: VerifiedProgram,
        mut support: RuntimeSupport,
        limits: VmLimits,
        epoch: SessionEpoch,
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
                .is_none_or(|n| n > limits.slots.max_window_len)
            || limits
                .max_unwinds
                .checked_mul(2)
                .is_none_or(|n| n > limits.slots.max_window_len)
            || limits
                .max_tasks
                .checked_mul(2)
                .and_then(|n| n.checked_add(2))
                .is_none_or(|n| n > limits.heap.max_root_ranges)
        {
            return Err(VmError::Limit);
        }
        // Load rejects unsupported operand semantics as well as unsupported opcodes.
        for ty in &p.types {
            if !matches!(ty, TypeDef::Builtin(_)) {
                return Err(VmError::Contract(
                    "structural/named type handler is not connected",
                ));
            }
        }
        for c in &p.constants {
            if !matches!(
                c,
                Const::Null
                    | Const::Bool(_)
                    | Const::Int(_)
                    | Const::FloatBits(_)
                    | Const::Utf8(_)
                    | Const::Bytes(_)
            ) {
                return Err(VmError::Contract("tuple constant handler is not connected"));
            }
        }
        for sig in &p.signatures {
            if sig.rest.is_some()
                || sig.property_bag_tail.is_some()
                || sig.parameters.len() > limits.max_arguments as usize
            {
                return Err(VmError::Contract(
                    "rest/property bag or excessive signature is not supported",
                ));
            }
        }
        for native in &p.natives {
            if p.signatures[native.signature.index()]
                .parameters
                .iter()
                .any(|p| p.default.is_some())
            {
                return Err(VmError::Contract(
                    "native script defaults require a module environment adapter",
                ));
            }
        }
        for f in &p.functions {
            if f.kind != FunctionKind::Plain {
                return Err(VmError::Contract(
                    "async/generator handler is not connected",
                ));
            }
            if f.register_count > limits.slots.max_window_len
                || f.regions.len() as u64 * 2 + 3 > limits.slots.max_window_len as u64
            {
                return Err(VmError::Limit);
            }
        }
        let mut heap = Heap::new(epoch, limits.heap)?;
        let usage = heap.budget_usage();
        let program_charge = Reservation::new(&usage, program.stats().owned_capacity_bytes as u64)?;
        let mut slots = FrameSlots::new(&mut heap, limits.slots)?;
        let control_bytes = std::mem::size_of::<Self>()
            - std::mem::size_of::<Heap>()
            - std::mem::size_of::<FrameSlots>()
            - std::mem::size_of::<BytecodePackage>();
        let control_charge = Reservation::new(&usage, control_bytes as u64)?;
        let active = TaskState::new(&usage, &mut heap, &mut slots, limits, false)?;
        let mut tasks = ChargedVec::new(&usage, limits.max_tasks as usize)?;
        for _ in 0..limits.max_tasks {
            tasks
                .data
                .push(TaskState::new(&usage, &mut heap, &mut slots, limits, true)?);
        }
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
        let text_scratch = ChargedVec::new(&usage, limits.heap.max_mutation_bytes as usize)?;
        let constants = slots.open(
            &mut heap,
            RootOwner(u64::MAX),
            RootKind::ModuleSet,
            p.constants.len() as u32,
        )?;
        for (i, c) in p.constants.iter().enumerate() {
            let value = match c {
                Const::Null => Value::NULL,
                Const::Bool(v) => Value::bool(*v),
                Const::Int(v) => Value::int(*v),
                Const::FloatBits(v) => Value::float(f64::from_bits(*v)),
                Const::Utf8(v) => heap.alloc_string(v)?,
                Const::Bytes(v) => heap.alloc_bytes(v)?,
                _ => unreachable!(),
            };
            slots.write(&mut heap, at(constants, i as u32), value)?;
        }
        let globals = slots.open(
            &mut heap,
            RootOwner(u64::MAX),
            RootKind::ModuleSet,
            p.modules.iter().map(|m| m.globals.len() as u32).sum(),
        )?;
        let mut offset = 0;
        for (mi, module) in p.modules.iter().enumerate() {
            for global in &module.globals {
                let TypeDef::Builtin(ty) = p.types[global.ty.index()] else {
                    unreachable!()
                };
                let cell = heap.alloc_language_cell(CellContract {
                    mutable: global.mutable,
                    ty,
                    type_id: global.ty,
                    module: ModuleId(mi as u32),
                    code_version: p.content_version,
                    module_version: p.content_version,
                })?;
                slots.write(&mut heap, at(globals, offset), cell)?;
                if let Some(f) = global.linked_function {
                    let closure = heap.alloc_record(
                        ObjectKind::Closure,
                        &[
                            Value::int(f.0 as i64),
                            Value::int(p.content_version as i64),
                            Value::int(mi as i64),
                        ],
                    )?;
                    heap.cell_initialize(cell, closure)?;
                }
                offset += 1;
            }
        }
        Ok(Self {
            program,
            program_charge,
            slots,
            active,
            tasks,
            active_slot: None,
            scratch,
            text_scratch,

            globals,
            constants,
            heap,
            limits,
            control_charge,
            next_id: 1,
            native_entries: 0,
            native_resumes: 0,
            slices: 0,
            last_usage: SliceUsage::default(),
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
            native_entries: self.native_entries,
            native_resumes: self.native_resumes,
            slices: self.slices,
            usage: self.last_usage,
            heap: self.heap.stats(),
            slots: self.slots.stats(),
            program_bytes: self.program_charge.bytes(),
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
        match s.ret {
            ReturnTo::Register { frame, call, .. } | ReturnTo::Default { frame, call, .. } => {
                match self.index(frame).ok().map(|i| self.active.frames.data[i]) {
                    Some(Frame::Script(parent)) => self.loc(parent, call),
                    _ => self.loc(s, s.pc),
                }
            }
            ReturnTo::Native { frame } => {
                match self.index(frame).ok().map(|i| self.active.frames.data[i]) {
                    Some(Frame::Native(n)) => n.location,
                    _ => self.loc(s, s.pc),
                }
            }
            ReturnTo::Delivery => self.loc(s, s.pc),
        }
    }
    fn contract(&self, module: ModuleId, ty: TypeId, mutable: bool) -> CellContract {
        let TypeDef::Builtin(kind) = self.program.package().types[ty.index()] else {
            unreachable!()
        };
        CellContract {
            mutable,
            ty: kind,
            type_id: ty,
            module,
            code_version: self.program.package().content_version,
            module_version: self.program.package().content_version,
        }
    }
    fn check_type(&self, ty: TypeId, value: Value) -> Result<(), FaultKind> {
        let TypeDef::Builtin(kind) = self.program.package().types[ty.index()] else {
            unreachable!()
        };
        if self.heap.accepts_builtin(kind, value).map_err(heap_fault)? {
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
        count * (4 * self.limits.heap.max_root_ranges as u64 + 8)
    }
    fn script_push_cost(&self, function: FunctionId, count: u32) -> u64 {
        let f = &self.program.package().functions[function.index()];
        f.register_count as u64
            + 3
            + 2 * f.regions.len() as u64
            + count as u64 * 2
            + self.program.package().signatures[f.signature.index()]
                .parameters
                .len() as u64
            + self.release_cost(4)
    }
    fn native_push_cost(&self, native: NativeId, count: u32) -> u64 {
        let signature = self.program.package().natives[native.index()].signature;
        3 + self.limits.max_arguments as u64
            + self.limits.max_continuation_values as u64
            + count as u64 * 2
            + self.program.package().signatures[signature.index()]
                .parameters
                .len() as u64
            + self.release_cost(3)
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
        let registers = f.register_count;
        let env_len = 3 + f.regions.len() as u32 * 2;
        let n = self.program.package().signatures[f.signature.index()]
            .parameters
            .len() as u32;
        let context = inherit(context, f.context);
        let effects = Effects(
            effects.0
                & self.program.package().signatures[f.signature.index()]
                    .effects
                    .0,
        );
        let module = ModuleInstanceId(f.module.0);
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
            effects,
            context,
            owner,
            ret,
            phase: ScriptPhase::Copy { source, index: 0 },
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
        let slot = self
            .tasks
            .data
            .iter()
            .position(|s| s.task_id.is_none())
            .ok_or(VmError::Limit)?;
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
        let Frame::Script(mut s) = *self.active.frames.data.last().unwrap() else {
            unreachable!()
        };
        for (i, &v) in args.iter().enumerate() {
            self.put(s.args, i as u32 * 2, Value::int(-1))?;
            self.put(s.args, i as u32 * 2 + 1, v)?;
        }
        s.phase = ScriptPhase::Bind {
            index: 0,
            positional: 0,
        };
        *self.active.frames.data.last_mut().unwrap() = Frame::Script(s);
        self.active.task_owner = Some(owner);
        self.active.task_id = Some(task);
        Ok(task)
    }
    fn closure_function(&self, closure: Value) -> Result<FunctionId, VmError> {
        if self.heap.info(closure)?.kind != ObjectKind::Closure {
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
            || self.heap.info(closure)?.len != f.captures.len() as u32 + 3
        {
            return Err(VmError::Contract("foreign closure environment/version"));
        }
        Ok(FunctionId(fi))
    }
    fn argument(&self, source: ArgSource, index: u32) -> Result<(i64, Value), VmError> {
        match source {
            ArgSource::None => Err(VmError::Contract("external arguments already copied")),
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
                        let name = desc
                            .names
                            .iter()
                            .find(|a| a.offset == index)
                            .map_or(-1, |a| a.name.0 as i64);
                        Ok((name, self.slots.read(at(s.regs, w.start.0 + index))?))
                    }
                    Arguments::Packed(r) => {
                        let packed = self.slots.read(at(s.regs, r.0))?;
                        Ok((
                            self.heap
                                .get_field(packed, index * 2)?
                                .as_int()
                                .ok_or(VmError::Contract("invalid argument name"))?,
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
                    -1,
                    self.value(n.roots, 2 + self.limits.max_continuation_values + index)?,
                ))
            }
            ArgSource::Prefix { frame, .. } => {
                let Frame::Script(s) = self.active.frames.data[self.index(frame)?] else {
                    unreachable!()
                };
                Ok((-1, self.value(s.bound, index)?))
            }
        }
    }
    fn bound_argument(
        &mut self,
        signature: SignatureId,
        bound: RootRangeToken,
        args: RootRangeToken,
        index: u32,
        positional: &mut u32,
    ) -> Result<(), FaultKind> {
        let name = self
            .slots
            .read(at(args, index * 2))
            .map_err(heap_fault)?
            .as_int()
            .ok_or(FaultKind::InvalidState)?;
        let value = self
            .slots
            .read(at(args, index * 2 + 1))
            .map_err(heap_fault)?;
        let sig = &self.program.package().signatures[signature.index()];
        let parameter = if name == -1 {
            let p = *positional;
            *positional += 1;
            p as usize
        } else {
            sig.parameters
                .iter()
                .position(|p| p.name.0 as i64 == name)
                .ok_or(FaultKind::UnknownArgument)?
        };
        let p = sig
            .parameters
            .get(parameter)
            .ok_or(FaultKind::UnknownArgument)?;
        match self.slots.read(at(bound, parameter as u32)) {
            Ok(_) => return Err(FaultKind::DuplicateArgument),
            Err(HeapError::UninitializedSlot) => {}
            Err(e) => return Err(heap_fault(e)),
        }
        self.check_type(p.ty, value)?;
        self.slots
            .write(&mut self.heap, at(bound, parameter as u32), value)
            .map_err(heap_fault)
    }
    fn push_native(
        &mut self,
        native: NativeId,
        source: ArgSource,
        count: u32,
        s: ScriptFrame,
        dest: Reg,
    ) -> Result<(), VmError> {
        if self.active.frames.data.len() == self.limits.max_frames as usize
            || count > self.limits.max_arguments
        {
            return Err(VmError::Limit);
        }
        let signature = self.program.package().natives[native.index()].signature;
        let n = self.program.package().signatures[signature.index()]
            .parameters
            .len() as u32;
        let id = FrameId(self.serial()?);
        let args = self.open(s.owner, RootKind::Arguments, count * 2)?;
        let bound = match self.open(s.owner, RootKind::Arguments, n) {
            Ok(r) => r,
            Err(e) => {
                self.slots.release(&mut self.heap, args)?;
                return Err(e);
            }
        };
        let roots = match self.open(
            s.owner,
            RootKind::NativeContinuation,
            3 + self.limits.max_continuation_values + self.limits.max_arguments,
        ) {
            Ok(r) => r,
            Err(e) => {
                self.slots.release(&mut self.heap, bound)?;
                self.slots.release(&mut self.heap, args)?;
                return Err(e);
            }
        };
        self.active.frames.data.push(Frame::Native(NativeFrame {
            id,
            native,
            signature,
            args,
            bound,
            roots,
            argument_count: count,
            continuation_len: 0,
            continuation_version: 0,
            continuation_stage: 0,
            effects: Effects(
                s.effects.0
                    & self.program.package().signatures[signature.index()]
                        .effects
                        .0,
            ),
            context: s.context,
            owner: s.owner,
            location: self.loc(s, s.pc),
            ret: ReturnTo::Register {
                frame: s.id,
                reg: dest,
                call: s.pc,
            },
            callback_fault: None,
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
        // Continuation suffix is explicitly initialized, including unused nulls.
        let all = self.slots.borrow_initialized(n.roots)?;
        Ok(&all[2..2 + n.continuation_len as usize])
    }
    pub fn host_callback_value(&self, ticket: HostTicket) -> Result<Value, VmError> {
        let n = self.ticket(ticket)?;
        if !matches!(
            ticket.input,
            NativeInput::CallbackReturned | NativeInput::CallbackFault
        ) {
            return Err(VmError::InvalidTicket);
        }
        self.value(n.roots, 0)
    }
    pub fn host_callback_fault(&self, ticket: HostTicket) -> Result<Option<FaultInfo>, VmError> {
        let n = self.ticket(ticket)?;
        if !matches!(
            ticket.input,
            NativeInput::CallbackReturned | NativeInput::CallbackFault
        ) {
            return Err(VmError::InvalidTicket);
        }
        Ok(n.callback_fault)
    }
    pub fn host_callback_cause(&self, ticket: HostTicket) -> Result<Option<Value>, VmError> {
        let n = self.ticket(ticket)?;
        if !matches!(ticket.input, NativeInput::CallbackFault) {
            return Err(VmError::InvalidTicket);
        }
        if n.callback_fault.is_some_and(|f| f.has_cause) {
            Ok(Some(self.value(
                n.roots,
                2 + self.limits.max_continuation_values + self.limits.max_arguments,
            )?))
        } else {
            Ok(None)
        }
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
            HostResponse::Ready(v) | HostResponse::Error(v) => {
                let error = matches!(response, HostResponse::Error(_));
                n.pending_fault = if error {
                    if ticket.input == NativeInput::CallbackFault
                        && self.value(n.roots, 0)?.same_bits(v)
                    {
                        n.callback_fault
                    } else {
                        Some(FaultInfo {
                            kind: FaultKind::Thrown,
                            location: n.location,
                            has_cause: false,
                        })
                    }
                } else {
                    None
                };
                self.put(n.roots, 0, v)?;
                n.phase = NativePhase::Commit(error);
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
    fn save_continuation(
        &mut self,
        n: &mut NativeFrame,
        c: Continuation<'_>,
    ) -> Result<(), VmError> {
        if c.values.len() > self.limits.max_continuation_values as usize {
            return Err(VmError::Limit);
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
        let (v, error) = match result {
            Completion::Ready(v) => (v, false),
            Completion::Error(v) => (v, true),
        };
        self.put(n.roots, 0, v)?;
        n.pending_fault = error.then_some(FaultInfo {
            kind: FaultKind::Thrown,
            location: n.location,
            has_cause: false,
        });
        n.phase = NativePhase::Commit(error);
        *self.active.frames.data.last_mut().unwrap() = Frame::Native(n);
        Ok(())
    }
    pub fn delivery_value(&self, id: DeliveryId) -> Result<Value, VmError> {
        self.delivery_check(id)?;
        self.value(self.task(id.task)?.delivery.unwrap(), 0)
    }
    pub fn delivery_cause(&self, id: DeliveryId) -> Result<Option<Value>, VmError> {
        let t = self.delivery_check(id)?;
        if t.fault.is_some_and(|f| f.has_cause) {
            Ok(Some(self.value(self.task(id.task)?.delivery.unwrap(), 1)?))
        } else {
            Ok(None)
        }
    }
    pub fn delivery_fault(&self, id: DeliveryId) -> Result<Option<FaultInfo>, VmError> {
        Ok(self.delivery_check(id)?.fault)
    }
    fn delivery_check(&self, id: DeliveryId) -> Result<Terminal, VmError> {
        self.task(id.task)
            .map_err(|_| VmError::InvalidDelivery)?
            .terminal
            .filter(|t| t.id == id)
            .ok_or(VmError::InvalidDelivery)
    }
    pub fn take_delivery_root(&mut self, id: DeliveryId, target: RootToken) -> Result<(), VmError> {
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
    pub fn cancel_task(&mut self, task: TaskId) -> Result<(), VmError> {
        self.select_task(task)?;
        self.cancel_active(false)
    }
    fn cancel_active(&mut self, external: bool) -> Result<(), VmError> {
        if self.active.cancelling {
            self.active.release_external |= external;
            return Ok(());
        }
        self.active.cancelling = true;
        self.active.cancel_roots = 0;
        self.active.release_external = external;
        if self.active.terminal.take().is_some() {
            self.clear(self.active.delivery.unwrap(), 0)?;
            self.clear(self.active.delivery.unwrap(), 1)?;
        }
        Ok(())
    }
    pub fn cancel(&mut self, owner: RootOwner) -> Result<(), VmError> {
        self.restore_active();
        let mut found = false;
        for slot in 0..self.tasks.data.len() {
            if self.tasks.data[slot].task_owner == Some(owner) {
                self.select_slot(slot);
                self.cancel_active(true)?;
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
        2 + 2 * self.limits.max_tasks
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
        loop {
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
                        Frame::Native(_) => 3,
                    };
                    if !m.take(0, self.release_cost(count), 0) {
                        return Ok(RunOutcome::BudgetYield(m.usage));
                    }
                    self.release_frame(f)?;
                    self.active.frames.data.pop();
                    continue;
                }
                if self.active.release_external && self.active.cancel_roots < self.heap.root_slots()
                {
                    if !m.take(0, 1, 0) {
                        return Ok(RunOutcome::BudgetYield(m.usage));
                    }
                    self.heap.release_owner_step(
                        self.active.task_owner.unwrap(),
                        &mut self.active.cancel_roots,
                        1,
                    );
                    continue;
                }
                self.active.cancelling = false;
                self.active.task_owner = None;
                self.active.task_id = None;
                return Err(VmError::Idle);
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
    fn release_frame(&mut self, f: Frame) -> Result<(), VmError> {
        match f {
            Frame::Script(s) => {
                for r in [s.regs, s.env, s.args, s.bound] {
                    self.slots.release(&mut self.heap, r)?;
                }
            }
            Frame::Native(n) => {
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
            reason: ExitReason::Throw(FaultInfo {
                kind,
                location: self.binding_location(*s),
                has_cause: false,
            }),
        };
        Ok(())
    }
    fn native_fault(&mut self, n: &mut NativeFrame, kind: FaultKind) -> Result<(), VmError> {
        self.put(n.roots, 0, Value::NULL)?;
        n.pending_fault = Some(FaultInfo {
            kind,
            location: n.location,
            has_cause: false,
        });
        n.phase = NativePhase::Commit(true);
        Ok(())
    }
    fn script_step(&mut self, mut s: ScriptFrame, m: &mut Meter) -> Result<bool, VmError> {
        let top = self.active.frames.data.len() - 1;
        let signature = self.program.package().functions[s.code.function.index()].signature;
        match s.phase {
            ScriptPhase::Copy { source, index } => {
                if !m.take(0, 1, 0) {
                    return Ok(false);
                }
                if index < s.argument_count {
                    match self.argument(source, index) {
                        Ok((name, v)) => {
                            self.put(s.args, index * 2, Value::int(name))?;
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
                    s.phase = ScriptPhase::Bind {
                        index: 0,
                        positional: 0,
                    };
                }
            }
            ScriptPhase::Bind {
                index,
                mut positional,
            } => {
                let n = self.program.package().signatures[signature.index()]
                    .parameters
                    .len() as u64;
                if !m.take(0, n + 2, 0) {
                    return Ok(false);
                }
                if index < s.argument_count {
                    match self.bound_argument(signature, s.bound, s.args, index, &mut positional) {
                        Ok(()) => {
                            s.phase = ScriptPhase::Bind {
                                index: index + 1,
                                positional,
                            }
                        }
                        Err(kind) => {
                            self.preparation_fault(&mut s, kind)?;
                        }
                    }
                } else {
                    s.phase = ScriptPhase::Defaults { parameter: 0 };
                }
            }
            ScriptPhase::Defaults { parameter } => {
                if !m.take(0, 1, 0) {
                    return Ok(false);
                }
                let sig = &self.program.package().signatures[signature.index()];
                if parameter as usize == sig.parameters.len() {
                    s.phase = ScriptPhase::Install { binding: 0 };
                } else if self.slots.read(at(s.bound, parameter)).is_ok() {
                    s.phase = ScriptPhase::Defaults {
                        parameter: parameter + 1,
                    };
                } else if let Some(default) = sig.parameters[parameter as usize].default {
                    if !m.take(0, self.script_push_cost(default, parameter), 0) {
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
                } else {
                    self.preparation_fault(&mut s, FaultKind::MissingArgument)?;
                }
            }
            ScriptPhase::Install { binding } => {
                if !m.take(0, 1, 0) {
                    return Ok(false);
                }
                let f = &self.program.package().functions[s.code.function.index()];
                // Parameters without explicit binding entries still occupy leading registers.
                let n = self.program.package().signatures[signature.index()]
                    .parameters
                    .len() as u32;
                if binding < n {
                    let v = self.value(s.bound, binding)?;
                    self.put(s.regs, binding, v)?;
                    s.phase = ScriptPhase::Install {
                        binding: binding + 1,
                    };
                } else if ((binding - n) as usize) < f.bindings.len() {
                    let b = f.bindings[(binding - n) as usize].clone();
                    match b.initialization {
                        BindingInitialization::Explicit => {
                            // Value binding bitmap starts uninitialized even if a temporary used it.
                            if matches!(b.storage, BindingStorage::Value(_)) {
                                self.clear(s.regs, b.storage.reg().0)?;
                            }
                        }
                        BindingInitialization::Parameter(parameter) => {
                            let v = self.value(s.bound, parameter)?;
                            match b.storage {
                                BindingStorage::Value(r) => self.put(s.regs, r.0, v)?,
                                BindingStorage::Cell(r) => {
                                    let allocation = self.heap.alloc_language_cell(self.contract(
                                        ModuleId(s.module.0),
                                        b.ty,
                                        b.mutable,
                                    ));
                                    match allocation {
                                        Ok(cell) => {
                                            self.heap.cell_initialize(cell, v)?;
                                            self.put(s.regs, r.0, cell)?;
                                        }
                                        Err(e) => {
                                            let kind = language_fault(VmError::Heap(e))?;
                                            self.preparation_fault(&mut s, kind)?;
                                            self.active.frames.data[top] = Frame::Script(s);
                                            return Ok(true);
                                        }
                                    }
                                }
                            }
                        }
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
                        s.phase = ScriptPhase::Exit {
                            origin: s.pc,
                            reason: ExitReason::Throw(FaultInfo {
                                has_cause: false,
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
                        self.put(s.env, 1, Value::NULL)?;
                        s.phase = ScriptPhase::Exit {
                            origin: s.pc,
                            reason: ExitReason::Throw(FaultInfo {
                                has_cause: false,
                                kind,
                                location: self.loc(s, s.pc),
                            }),
                            skip: None,
                        };
                    }
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
                    match result {
                        Ok(equal) => {
                            self.put(s.regs, dest.0, Value::bool(equal != negated))?;
                            s.pc.0 += 1;
                            s.phase = ScriptPhase::Body;
                        }
                        Err(kind) => {
                            self.put(s.env, 1, Value::NULL)?;
                            s.phase = ScriptPhase::Exit {
                                origin: s.pc,
                                reason: ExitReason::Throw(FaultInfo {
                                    has_cause: false,
                                    kind,
                                    location: self.loc(s, s.pc),
                                }),
                                skip: None,
                            };
                        }
                    }
                }
            }
            ScriptPhase::Exit {
                origin,
                reason,
                skip,
            } => {
                if !m.take(
                    0,
                    self.program.package().functions[s.code.function.index()]
                        .regions
                        .len() as u64
                        + self.active.caught.data.len() as u64
                        + 2
                        + self.release_cost(1),
                    0,
                ) {
                    return Ok(false);
                }
                self.exit_step(&mut s, origin, reason, skip)?;
            }
            ScriptPhase::Release { reason } => {
                if !m.take(0, self.active.caught.data.len() as u64, 0) {
                    return Ok(false);
                }
                if let Some(index) = self.active.caught.data.iter().position(|c| c.frame == s.id) {
                    if !m.take(0, self.active.caught.data.len() as u64 + 2, 0) {
                        return Ok(false);
                    }
                    let record = self.active.caught.data.remove(index);
                    self.clear(s.env, 3 + record.region.0 * 2)?;
                    self.clear(s.env, 4 + record.region.0 * 2)?;
                    self.active.frames.data[top] = Frame::Script(s);
                    return Ok(true);
                }
                if !m.take(0, self.release_cost(4), 0) {
                    return Ok(false);
                }
                let value = self.value(s.env, 1)?;
                let cause = match reason {
                    ExitReason::Throw(info) if info.has_cause => Some(self.value(s.env, 2)?),
                    _ => None,
                };
                self.commit(
                    s.ret,
                    value,
                    cause,
                    match reason {
                        ExitReason::Throw(info) => Some(info),
                        _ => None,
                    },
                )?;
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
        _c: u32,
    ) -> Result<(u64, u64), VmError> {
        let f = &self.program.package().functions[s.code.function.index()];
        Ok(match op {
            Opcode::Add => {
                let left = self.slots.read(at(s.regs, b))?;
                let right = self.slots.read(at(s.regs, _c))?;
                if self.is_text(left, ObjectKind::String) && self.is_text(right, ObjectKind::String)
                {
                    let n = self.heap.info(left)?.len as u64 + self.heap.info(right)?.len as u64;
                    (
                        n * 3 + 1,
                        n.div_ceil(BYTE_CHUNK_LEN as u64) * BYTE_CHUNK_BYTES,
                    )
                } else {
                    (1, 0)
                }
            }
            Opcode::Call => {
                let Descriptor::Call(d) = &f.descriptors[b as usize] else {
                    unreachable!()
                };
                let count = match d.arguments {
                    Arguments::Window(w) => w.len,
                    Arguments::Packed(r) => {
                        self.heap.info(self.slots.read(at(s.regs, r.0))?)?.len / 2
                    }
                };
                let cost = match d.target {
                    CallTarget::Function(id) => self.script_push_cost(id, count),
                    CallTarget::Native(id) => self.native_push_cost(id, count),
                    CallTarget::Register(r) => self
                        .closure_function(self.slots.read(at(s.regs, r.0))?)
                        .map(|id| self.script_push_cost(id, count))
                        .unwrap_or(1),
                };
                (cost + self.active.unwinds.data.len() as u64 + 1, 0)
            }
            Opcode::MakeClosure => {
                let Descriptor::Closure { captures, .. } = &f.descriptors[b as usize] else {
                    unreachable!()
                };
                (
                    captures.len() as u64 + 1,
                    ((captures.len() + 3) as u64).div_ceil(VALUE_CHUNK_LEN as u64)
                        * VALUE_CHUNK_BYTES,
                )
            }
            Opcode::Rethrow => (
                f.regions.len() as u64 + self.active.caught.data.len() as u64 + 3,
                0,
            ),
            Opcode::EndFinally => (self.release_cost(1), 0),
            Opcode::GlobalRead | Opcode::GlobalInit | Opcode::GlobalWrite => {
                (self.program.package().modules.len() as u64 + 1, 0)
            }
            Opcode::ArgsNew => (1, 0),
            Opcode::ArgsPush | Opcode::ArgsNamed => {
                let packed = self.slots.read(at(s.regs, _a))?;
                let len = self.heap.info(packed)?.len;
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
        if matches!(
            op,
            Opcode::CellWrite | Opcode::BindWrite | Opcode::GlobalWrite | Opcode::GlobalInit
        ) && (s.context.derived || s.effects.0 & Effects::WRITE == 0)
        {
            return Err(FaultKind::Effect);
        }
        if matches!(
            op,
            Opcode::CellNew
                | Opcode::MakeClosure
                | Opcode::ArgsNew
                | Opcode::ArgsPush
                | Opcode::ArgsNamed
        ) && s.effects.0 & Effects::ALLOCATE == 0
        {
            return Err(FaultKind::Effect);
        }
        match op {
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
                let closure = self.slots.read(at(s.env, 0)).map_err(heap_fault)?;
                let v = self.heap.get_field(closure, b + 3).map_err(heap_fault)?;
                self.write(*s, r(a), v)?;
            }
            Opcode::GlobalRead | Opcode::GlobalInit | Opcode::GlobalWrite => {
                let Descriptor::Global { module, global } = f.descriptors[b as usize] else {
                    unreachable!()
                };
                let offset = self.program.package().modules[..module.index()]
                    .iter()
                    .map(|m| m.globals.len() as u32)
                    .sum::<u32>()
                    + global.0;
                let cell = self
                    .slots
                    .read(at(self.globals, offset))
                    .map_err(heap_fault)?;
                if op == Opcode::GlobalRead {
                    let v = self.heap.cell_read(cell).map_err(heap_fault)?;
                    self.write(*s, r(a), v)?;
                } else {
                    let v = self.read(*s, r(a))?;
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
                let v = if op == Opcode::Add
                    && self.is_text(left, ObjectKind::String)
                    && self.is_text(right, ObjectKind::String)
                {
                    self.concat_text(left, right)?
                } else {
                    binary(op, left, right)?
                };
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
                            let closure = self.slots.read(at(s.env, 0)).map_err(heap_fault)?;
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
                // Preflight two edges before the first append to keep pair publication atomic.
                let len = self.heap.info(packed).map_err(heap_fault)?.len;
                if len / 2 >= self.limits.max_arguments {
                    return Err(FaultKind::Resource);
                }
                self.heap
                    .budget_usage()
                    .ensure(if (len as usize).is_multiple_of(VALUE_CHUNK_LEN) {
                        VALUE_CHUNK_BYTES
                    } else {
                        0
                    })
                    .map_err(heap_fault)?;
                self.heap
                    .array_push(
                        packed,
                        Value::int(if op == Opcode::ArgsNamed {
                            c as i64
                        } else {
                            -1
                        }),
                    )
                    .map_err(heap_fault)?;
                self.heap.array_push(packed, v).map_err(heap_fault)?;
            }
            Opcode::Call => {
                let Descriptor::Call(desc) = &f.descriptors[b as usize] else {
                    unreachable!()
                };
                let target = desc.target;
                let count = match desc.arguments {
                    Arguments::Window(w) => w.len,
                    Arguments::Packed(r) => {
                        let v = self.read(*s, r)?;
                        let info = self.heap.info(v).map_err(heap_fault)?;
                        if info.kind != ObjectKind::Array || info.len % 2 != 0 {
                            return Err(FaultKind::Type);
                        }
                        info.len / 2
                    }
                };
                let source = ArgSource::Caller {
                    frame: s.id,
                    descriptor: DescriptorId(b),
                };
                let mut caller = *s;
                if self.active.unwinds.data.iter().any(|u| u.frame == s.id) {
                    caller.context = inherit(caller.context, Context::NoWait);
                }
                let dynamic = if let CallTarget::Register(reg) = target {
                    let closure = self.read(*s, reg)?;
                    Some((
                        closure,
                        self.closure_function(closure)
                            .map_err(|_| FaultKind::Type)?,
                    ))
                } else {
                    None
                };
                s.pc.0 += 1;
                let caller_index = self.index(s.id).map_err(|_| FaultKind::InvalidState)?;
                self.active.frames.data[caller_index] = Frame::Script(*s);
                match target {
                    CallTarget::Native(native) => {
                        if s.effects.0 & Effects::HOST == 0 {
                            s.pc = caller.pc;
                            return Err(FaultKind::Effect);
                        }
                        if let Err(e) = self.push_native(native, source, count, caller, r(a)) {
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
            Opcode::Throw => {
                let v = self.read(*s, r(a))?;
                self.slots
                    .write(&mut self.heap, at(s.env, 1), v)
                    .map_err(heap_fault)?;
                s.phase = ScriptPhase::Exit {
                    origin: s.pc,
                    reason: ExitReason::Throw(FaultInfo {
                        has_cause: false,
                        kind: FaultKind::Thrown,
                        location: self.loc(*s, s.pc),
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
                            .map(|c| (id, c.range.end.0 - c.range.start.0))
                    })
                    .min_by_key(|(_, n)| *n)
                    .map(|(id, _)| RegionId(id as u32))
                    .ok_or(FaultKind::InvalidState)?;
                let info = self
                    .active
                    .caught
                    .data
                    .iter()
                    .find(|c| c.frame == s.id && c.region == region)
                    .map(|c| c.info)
                    .ok_or(FaultKind::InvalidState)?;
                let value = self.value(s.env, 3 + region.0 * 2).map_err(vm_fault)?;
                self.put(s.env, 1, value).map_err(vm_fault)?;
                if info.has_cause {
                    let cause = self.value(s.env, 4 + region.0 * 2).map_err(vm_fault)?;
                    self.put(s.env, 2, cause).map_err(vm_fault)?;
                } else {
                    self.clear(s.env, 2).map_err(vm_fault)?;
                }
                s.phase = ScriptPhase::Exit {
                    origin: s.pc,
                    reason: ExitReason::Throw(info),
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
                if matches!(u.reason,ExitReason::Throw(info) if info.has_cause) {
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
    fn concat_text(&mut self, left: Value, right: Value) -> Result<Value, FaultKind> {
        let left_len = self.heap.info(left).map_err(heap_fault)?.len as usize;
        let right_len = self.heap.info(right).map_err(heap_fault)?.len as usize;
        let len = left_len.checked_add(right_len).ok_or(FaultKind::Resource)?;
        if len > self.text_scratch.data.capacity() {
            return Err(FaultKind::Resource);
        }
        self.text_scratch.data.resize(len, 0);
        self.heap
            .read_bytes(left, 0, &mut self.text_scratch.data[..left_len])
            .map_err(heap_fault)?;
        self.heap
            .read_bytes(right, 0, &mut self.text_scratch.data[left_len..])
            .map_err(heap_fault)?;
        let text =
            std::str::from_utf8(&self.text_scratch.data).map_err(|_| FaultKind::InvalidState)?;
        let result = self.heap.alloc_string(text).map_err(heap_fault);
        self.text_scratch.data.clear();
        result
    }
    fn equality_step(
        &mut self,
        s: &mut ScriptFrame,
        dest: Reg,
        negated: bool,
    ) -> Result<(), VmError> {
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
    fn exit_step(
        &mut self,
        s: &mut ScriptFrame,
        origin: Pc,
        reason: ExitReason,
        skip: Option<RegionId>,
    ) -> Result<(), VmError> {
        let f = &self.program.package().functions[s.code.function.index()];
        let handler = match reason {
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
        };
        // A new error/return inside an active finally supersedes that exit. Nested
        // ordinary Leave stays within the outer cleanup and preserves its record.
        if let Some(u) = self
            .active
            .unwinds
            .data
            .last()
            .copied()
            .filter(|u| u.frame == s.id)
        {
            let range = f.regions[u.region.index()].finally.unwrap();
            let handled_inside = handler.is_some_and(|h| range.contains(h.pc));
            if range.contains(origin)
                && !handled_inside
                && !matches!(reason,ExitReason::Leave(target) if range.contains(target))
            {
                if let ExitReason::Throw(mut info) = reason {
                    let previous = self.value(u.value, 0)?;
                    self.put(s.env, 2, previous)?;
                    info.has_cause = true;
                    s.phase = ScriptPhase::Exit {
                        origin,
                        reason: ExitReason::Throw(info),
                        skip,
                    };
                }
                self.slots.release(&mut self.heap, u.value)?;
                self.active.unwinds.data.pop();
                return Ok(());
            }
        }
        if let Some(handler) = handler {
            let value = self.value(s.env, 1)?;
            match handler.kind {
                HandlerKind::Catch => {
                    let ExitReason::Throw(info) = reason else {
                        return Err(VmError::Contract("catch on normal exit"));
                    };
                    let base = 3 + handler.region.0 * 2;
                    self.put(s.env, base, value)?;
                    if info.has_cause {
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
                            return Err(VmError::Limit);
                        }
                        self.active.caught.data.push(CaughtFault {
                            frame: s.id,
                            region: handler.region,
                            info,
                        });
                    }
                    self.put(s.regs, handler.exception.unwrap().0, value)?;
                    s.pc = handler.pc;
                    s.phase = ScriptPhase::Body;
                }
                HandlerKind::Finally => {
                    if self.active.unwinds.data.len() == self.limits.max_unwinds as usize {
                        return Err(VmError::Limit);
                    }
                    let root = self.open(s.owner, RootKind::Exception, 2)?;
                    self.put(root, 0, value)?;
                    if matches!(reason,ExitReason::Throw(info) if info.has_cause) {
                        let cause = self.value(s.env, 2)?;
                        self.put(root, 1, cause)?;
                    }
                    self.active.unwinds.data.push(Unwind {
                        frame: s.id,
                        origin,
                        reason,
                        region: handler.region,
                        value: root,
                    });
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
        Ok(())
    }
    fn commit(
        &mut self,
        ret: ReturnTo,
        value: Value,
        cause: Option<Value>,
        error: Option<FaultInfo>,
    ) -> Result<(), VmError> {
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
            ReturnTo::Register { frame, call, .. } | ReturnTo::Default { frame, call, .. } => {
                let i = self.index(frame)?;
                let Frame::Script(mut parent) = self.active.frames.data[i] else {
                    return Err(VmError::Contract("return target"));
                };
                if let Some(info) = error {
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
                        ReturnTo::Default { parameter, .. } => {
                            self.put(parent.bound, parameter, value)?;
                            parent.phase = ScriptPhase::Defaults {
                                parameter: parameter + 1,
                            };
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
                n.callback_fault = error;
                n.phase = NativePhase::Resume(if error.is_some() {
                    NativeInput::CallbackFault
                } else {
                    NativeInput::CallbackReturned
                });
                self.active.frames.data[i] = Frame::Native(n);
            }
        }
        Ok(())
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
                let root_len = 3 + self.limits.max_continuation_values + self.limits.max_arguments;
                if index < n.argument_count {
                    match self.argument(source, index) {
                        Ok((name, value)) => {
                            self.put(n.args, index * 2, Value::int(name))?;
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
                } else if index < n.argument_count + root_len {
                    self.put(n.roots, index - n.argument_count, Value::NULL)?;
                    n.phase = NativePhase::Copy {
                        source,
                        index: index + 1,
                    };
                } else {
                    n.phase = NativePhase::Bind {
                        index: 0,
                        positional: 0,
                    };
                }
            }
            NativePhase::Bind {
                index,
                mut positional,
            } => {
                let sig = &self.program.package().signatures[n.signature.index()];
                if !m.take(0, sig.parameters.len() as u64 + 2, 0) {
                    return Ok(Some(RunOutcome::BudgetYield(m.usage)));
                }
                if index < n.argument_count {
                    match self.bound_argument(n.signature, n.bound, n.args, index, &mut positional)
                    {
                        Ok(()) => {
                            n.phase = NativePhase::Bind {
                                index: index + 1,
                                positional,
                            }
                        }
                        Err(kind) => {
                            self.native_fault(&mut n, kind)?;
                        }
                    }
                } else if self.slots.borrow_initialized(n.bound).is_err() {
                    self.native_fault(&mut n, FaultKind::MissingArgument)?;
                } else {
                    n.phase = NativePhase::Resume(NativeInput::Entry);
                }
            }
            NativePhase::Resume(input) => {
                if !m.take(0, 1, 0) {
                    return Ok(Some(RunOutcome::BudgetYield(m.usage)));
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
                        n.callback_fault = Some(FaultInfo {
                            kind,
                            location: n.location,
                            has_cause: false,
                        });
                        n.phase = NativePhase::Resume(NativeInput::CallbackFault);
                    }
                }
            }
            NativePhase::Commit(error) => {
                if !m.take(0, 1, 0) {
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
                    n.pending_fault = Some(FaultInfo {
                        kind: FaultKind::Type,
                        location: n.location,
                        has_cause: false,
                    });
                    n.phase = NativePhase::Release(true);
                } else {
                    n.phase = NativePhase::Release(error);
                }
            }
            NativePhase::Release(error) => {
                if !m.take(0, self.release_cost(3), 0) {
                    return Ok(Some(RunOutcome::BudgetYield(m.usage)));
                }
                let value = self.value(n.roots, 0)?;
                let info = if error {
                    Some(n.pending_fault.unwrap_or(FaultInfo {
                        kind: FaultKind::Thrown,
                        location: n.location,
                        has_cause: false,
                    }))
                } else {
                    None
                };
                let cause = if info.is_some_and(|f| f.has_cause) {
                    Some(self.value(
                        n.roots,
                        2 + self.limits.max_continuation_values + self.limits.max_arguments,
                    )?)
                } else {
                    None
                };
                self.commit(n.ret, value, cause, info)?;
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
        HeapError::ReadOnly => FaultKind::ReadOnly,
        HeapError::TypeMismatch | HeapError::WrongKind => FaultKind::Type,
        HeapError::AlreadyInitialized => FaultKind::AlreadyInitialized,
        _ => FaultKind::Resource,
    }
}
fn language_fault(e: VmError) -> Result<FaultKind, VmError> {
    match e {
        VmError::Limit => Ok(FaultKind::Resource),
        VmError::Contract("invalid argument name") => Ok(FaultKind::Type),
        VmError::Heap(
            h @ (HeapError::UninitializedSlot
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
