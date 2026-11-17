//! Public V03 slice, rooted delivery and native response contract.
use crate::bytecode::{self, *};
use crate::{
    FrameSlotsLimits, FrameSlotsStats, HeapError, HeapId, HeapLimits, HeapStats, RootOwner,
    SessionEpoch, Value,
};
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FrameId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskId {
    pub(super) heap: HeapId,
    pub(super) epoch: SessionEpoch,
    pub(super) serial: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModuleInstanceId(pub u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CodeRef {
    pub function: FunctionId,
    pub version: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Location {
    pub code: CodeRef,
    pub pc: Pc,
    pub source: Option<SourceSpan>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeliveryId {
    pub(super) task: TaskId,
    pub(super) heap: HeapId,
    pub(super) epoch: SessionEpoch,
    pub(super) serial: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WaitId {
    pub(super) task: TaskId,
    pub(super) heap: HeapId,
    pub(super) epoch: SessionEpoch,
    pub(super) serial: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NativeInput {
    Entry,
    Continue { version: u64, stage: u64 },
    CallbackReturned,
    CallbackFault,
    CallbackCancelled,
    WaitingReturned,
    WaitingFault,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContextFlags {
    pub no_wait: bool,
    pub init: bool,
    pub derived: bool,
}
impl From<Context> for ContextFlags {
    fn from(c: Context) -> Self {
        Self {
            no_wait: c == Context::NoWait,
            init: c == Context::Init,
            derived: c == Context::Derived,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContinuationState {
    pub version: u64,
    pub stage: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostTicket {
    pub(super) heap: HeapId,
    pub(super) epoch: SessionEpoch,
    pub(super) serial: u64,
    pub frame: FrameId,
    pub task: TaskId,
    pub native: NativeId,
    pub signature: SignatureId,
    pub abi_version: u32,
    pub owner: RootOwner,
    pub location: Location,
    pub input: NativeInput,
    pub context: ContextFlags,
    pub continuation: Option<ContinuationState>,
    pub effects: Effects,
    /// Stable identity of this await expression's execution, if this is an await adapter.
    pub await_activation: Option<u64>,
}

/// Borrow the explicitly supplied native arguments in source evaluation order.
/// Each entry keeps its resolved parameter index and original value, including
/// an explicit null or property reference. Defaults and a bound receiver are absent.
#[derive(Clone, Copy)]
pub struct ProvidedArguments<'a> {
    pub(super) values: &'a [Value],
    pub(super) positions: Option<&'a [u32]>,
    pub(super) positional_start: Option<u32>,
}

impl ProvidedArguments<'_> {
    pub fn len(&self) -> usize {
        self.positional_start.map_or_else(
            || self.positions.map_or(self.values.len() / 2, |p| p.len()),
            |start| self.values.len() - start as usize,
        )
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Read an argument without allocating or repeating name binding.
    pub fn get(&self, index: usize) -> Option<(u32, Value)> {
        if let Some(start) = self.positional_start {
            let position = (start as usize).checked_add(index)?;
            return self
                .values
                .get(position)
                .map(|value| (position as u32, *value));
        }
        if let Some(positions) = self.positions {
            let position = *positions.get(index)?;
            return Some((position, self.values[position as usize]));
        }
        let pair = self
            .values
            .get(index.checked_mul(2)?..index.checked_mul(2)?.checked_add(2)?)?;
        Some((
            pair[0].as_int().expect("bound native argument index") as u32,
            pair[1],
        ))
    }
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (u32, Value)> + '_ {
        (0..self.len()).map(|index| self.get(index).expect("provided argument index"))
    }
}
impl HostTicket {
    pub fn wait_id(self) -> WaitId {
        WaitId {
            task: self.task,
            heap: self.heap,
            epoch: self.epoch,
            serial: self.serial,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Continuation<'a> {
    pub version: u64,
    pub stage: u64,
    pub values: &'a [Value],
    pub faults: FaultRetention<'a>,
}
#[derive(Clone, Copy, Debug)]
pub enum FaultRetention<'a> {
    Preserve,
    Replace(&'a [FaultId]),
}
pub enum HostResponse<'a> {
    Ready(Value),
    Pending(WaitId),
    CallScript {
        closure: Value,
        arguments: &'a [Value],
        continuation: Continuation<'a>,
    },
    Continue(Continuation<'a>),
    Error(NewFault),
    ForwardFault(FaultId),
}
#[derive(Clone, Copy, Debug)]
pub enum Completion {
    Ready(Value),
    Error(NewFault),
    ForwardFault(FaultId),
}
#[derive(Clone, Copy, Debug)]
pub struct SliceBudget {
    pub instructions: u32,
    pub work: u64,
    pub allocation_bytes: u64,
    /// Optional soft deadline in the host's monotonic nanosecond clock. Check on
    /// slice entry and host resumption, then at most every 32 internal execution
    /// steps. An allocation or an operation charged at least 32 work units forces
    /// the next boundary to check again. One indivisible operation can overrun
    /// this deadline; instruction, work and allocation quotas remain exact.
    pub deadline_ns: Option<u64>,
}
impl Default for SliceBudget {
    fn default() -> Self {
        Self {
            instructions: 1024,
            work: 65536,
            allocation_bytes: 1024 * 1024,
            deadline_ns: None,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SliceUsage {
    pub instructions: u32,
    pub work: u64,
    pub allocation_bytes: u64,
    pub required_work: u64,
    pub required_allocation: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunOutcome {
    Returned(DeliveryId),
    BudgetYield(SliceUsage),
    HostCall(HostTicket),
    Waiting(WaitId),
    Fault(DeliveryId),
    ResourceBlocked(ResourceBlocked),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BlockedResource {
    HeapObjects,
    CatchRecords,
    UnwindRecords,
    NativeFaultSlots { frame: FrameId },
    RootRanges,
    WindowValues,
    HeapBytes,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ResourceBlocked {
    pub task: TaskId,
    pub resource: BlockedResource,
    pub current: u64,
    pub required: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
#[derive(serde::Serialize, serde::Deserialize)]
pub enum FaultKind {
    Thrown = 0,
    Type = 1,
    Uninitialized = 2,
    AlreadyInitialized = 3,
    ReadOnly = 4,
    DuplicateArgument = 5,
    MissingArgument = 6,
    UnknownArgument = 7,
    Overflow = 8,
    DivisionByZero = 9,
    Effect = 10,
    Resource = 11,
    InvalidState = 12,
    Cancelled = 13,
    OutOfBounds = 14,
    MissingKey = 15,
    UnhashableKey = 16,
    ConcurrentModification = 17,
    ModuleInitializationCycle = 18,
    ArgumentOrder = 19,
    MatchError = 20,
    UninitializedField = 21,
    ConstructionFailed = 22,
    PatternMismatch = 23,
    NotAwaitable = 24,
}
pub use crate::{FaultCode, FaultId, FaultInfo, FaultRoot, NewFault};
#[derive(Debug)]
pub enum VmError {
    Format(bytecode::Error),
    Heap(HeapError),
    UnknownTask,
    InvalidTicket,
    InvalidWait,
    InvalidDelivery,
    Busy,
    Idle,
    Limit,
    Contract(&'static str),
    InvalidFault,
    Effect,
}
impl From<HeapError> for VmError {
    fn from(e: HeapError) -> Self {
        Self::Heap(e)
    }
}
impl From<bytecode::Error> for VmError {
    fn from(e: bytecode::Error) -> Self {
        Self::Format(e)
    }
}
impl std::fmt::Display for VmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for VmError {}
#[derive(Clone, Copy, Debug)]
pub struct VmLimits {
    /// Optional host task quota. Task state is allocated on demand and reused.
    pub max_tasks: u32,
    pub heap: HeapLimits,
    /// Execution storage. Loaded constants, symbols, native values and globals
    /// add their exact root count; all capacity still charges the heap budget.
    pub slots: FrameSlotsLimits,
    pub max_frames: u32,
    pub max_unwinds: u32,
    pub max_arguments: u32,
    /// Maximum values accepted in a native continuation. Its root storage grows
    /// with actual responses; an ordinary leaf call does not reserve this limit.
    pub max_continuation_values: u32,
    pub max_native_faults: u32,
}
impl Default for VmLimits {
    fn default() -> Self {
        Self {
            max_tasks: u32::MAX,
            heap: HeapLimits::default(),
            slots: FrameSlotsLimits {
                initial_capacity: 16384,
                max_capacity: u32::MAX,
                max_window_len: 8192,
            },
            max_frames: 128,
            max_unwinds: 256,
            max_arguments: 256,
            max_continuation_values: 256,
            max_native_faults: 8,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct VmStats {
    pub frames: u32,
    pub unwinds: u32,
    pub comparison_depth: u32,
    pub native_entries: u64,
    /// Live weak method bindings reused by this VM; reset on restore.
    pub native_binding_hits: u64,
    /// Binding cache misses or values rejected by the GC cutoff; reset on restore.
    pub native_binding_misses: u64,
    /// Live script method bindings reused from the shared weak cache; reset on restore.
    pub script_binding_hits: u64,
    /// Script binding misses or GC-unconfirmed values recreated; reset on restore.
    pub script_binding_misses: u64,
    pub native_resumes: u64,
    /// Redundant whole-native-window initialization writes (removed in V03).
    pub native_null_publications: u64,
    pub slices: u64,
    /// Instructions admitted by completed run_slice calls; reset on restore.
    pub cumulative_instructions: u64,
    /// Work admitted by completed run_slice calls; reset on restore.
    pub cumulative_work: u64,
    /// Successfully started scheduled or lazy observer recomputations; reset on restore.
    /// Initial observer construction is excluded.
    pub derived_starts: u64,
    /// Recomputation starts using the compact script path; reset on restore.
    pub compact_derived_starts: u64,
    /// Standard derived outputs registered successfully by respond_derive; reset on restore.
    pub derived_outputs: u64,
    /// Registered standard outputs admitted to compact recomputation; reset on restore.
    pub compact_derived_outputs: u64,
    /// Complete Value parameter transactions admitted by an opted-in validation build.
    #[cfg(any(test, feature = "vm-validation-oracle"))]
    pub value_entry_transactions: u64,
    /// Those transactions using a receiver already rooted by the original call push.
    #[cfg(any(test, feature = "vm-validation-oracle"))]
    pub value_entry_receivers: u64,
    /// Experimental residual spans retired by an opted-in validation build.
    #[cfg(any(test, feature = "vm-validation-oracle"))]
    pub residual_body_spans: u64,
    /// Original instructions retired by those spans, with unchanged instruction grants.
    #[cfg(any(test, feature = "vm-validation-oracle"))]
    pub residual_body_instructions: u64,
    /// Guarded residual plans selected by an opted-in validation build.
    #[cfg(any(test, feature = "vm-validation-oracle"))]
    pub residual_body_hits: u64,
    /// Candidate plans rejected by their current object/value guards.
    #[cfg(any(test, feature = "vm-validation-oracle"))]
    pub residual_body_guard_fallbacks: u64,
    /// Residual spans sent through the original per-instruction budget path.
    #[cfg(any(test, feature = "vm-validation-oracle"))]
    pub residual_body_budget_fallbacks: u64,
    /// Residual spans limited by an original supplied-clock checkpoint.
    #[cfg(any(test, feature = "vm-validation-oracle"))]
    pub residual_body_clock_fallbacks: u64,
    /// Residual entry guards reused through current weak receiver/version proofs.
    #[cfg(any(test, feature = "vm-validation-oracle"))]
    pub residual_body_memo_hits: u64,
    pub usage: SliceUsage,
    pub heap: HeapStats,
    pub slots: FrameSlotsStats,
    pub program_bytes: u64,
    pub control_bytes: u64,
}

/// This is the dispatcher support list, never a claim about source compilation.
pub const SUPPORTED_OPCODES: &[Opcode] = &[
    Opcode::LoadConst,
    Opcode::Move,
    Opcode::Clear,
    Opcode::Clear2,
    Opcode::Clear3,
    Opcode::BindInit,
    Opcode::BindRead,
    Opcode::BindWrite,
    Opcode::CellNew,
    Opcode::CellRead,
    Opcode::CellWrite,
    Opcode::CaptureRead,
    Opcode::GlobalRead,
    Opcode::GlobalInit,
    Opcode::GlobalWrite,
    Opcode::Add,
    Opcode::Sub,
    Opcode::Mul,
    Opcode::Div,
    Opcode::Rem,
    Opcode::BitAnd,
    Opcode::BitOr,
    Opcode::BitXor,
    Opcode::Shl,
    Opcode::Shr,
    Opcode::Eq,
    Opcode::Ne,
    Opcode::Lt,
    Opcode::Le,
    Opcode::Gt,
    Opcode::Ge,
    Opcode::Neg,
    Opcode::Pos,
    Opcode::Not,
    Opcode::BitNot,
    Opcode::Is,
    Opcode::CheckType,
    Opcode::TextNew,
    Opcode::TextPush,
    Opcode::TextFinish,
    Opcode::Jump,
    Opcode::BranchBool,
    Opcode::BranchNull,
    Opcode::MakeClosure,
    Opcode::MakeTuple,
    Opcode::MakeArray,
    Opcode::MakeTable,
    Opcode::MakeClass,
    Opcode::SuperRef,
    Opcode::MemberGet,
    Opcode::MemberSet,
    Opcode::PropertyRef,
    Opcode::IndexGet,
    Opcode::IndexSet,
    Opcode::Call,
    Opcode::CallNoWait,
    Opcode::Await,
    Opcode::ChildrenPush,
    Opcode::ChildrenSpread,
    Opcode::ArrayPush,
    Opcode::ArraySpread,
    Opcode::TableSpread,
    Opcode::MakeRange,
    Opcode::MakeRangeInclusive,
    Opcode::IterInit,
    Opcode::IterNext,
    Opcode::IterAdvance,
    Opcode::Yield,
    Opcode::IterClose,
    Opcode::Return,
    Opcode::ArgsNew,
    Opcode::ArgsPush,
    Opcode::ArgsSpread,
    Opcode::ArgsNamed,
    Opcode::Throw,
    Opcode::MatchFail,
    Opcode::PatternRequire,
    Opcode::Rethrow,
    Opcode::Leave,
    Opcode::EndFinally,
];

impl FaultKind {
    pub fn message(self) -> &'static str {
        match self {
            Self::Thrown => "explicit throw",
            Self::Type => "type contract failed",
            Self::NotAwaitable => {
                "await requires an Awaitable; ordinary function calls already wait for their result"
            }
            Self::Uninitialized => "read before initialization",
            Self::AlreadyInitialized => "binding already initialized",
            Self::ReadOnly => "read-only binding",
            Self::DuplicateArgument => "duplicate argument",
            Self::MissingArgument => "missing argument",
            Self::UnknownArgument => "unknown argument",
            Self::ArgumentOrder => "positional argument after named argument",
            Self::PatternMismatch => "value does not match the binding pattern",
            Self::MatchError => "no match arm accepted the value",
            Self::UninitializedField => "field read before initialization",
            Self::ConstructionFailed => "instance construction failed",
            Self::Overflow => "integer overflow",
            Self::DivisionByZero => "division by zero",
            Self::Effect => "effect constraint",
            Self::Resource => "resource capacity",
            Self::InvalidState => "invalid execution state",
            Self::Cancelled => "cooperative cancellation",
            Self::OutOfBounds => "index out of bounds",
            Self::MissingKey => "missing table key",
            Self::UnhashableKey => "unhashable table key",
            Self::ConcurrentModification => "collection modified during operation",
            Self::ModuleInitializationCycle => "module binding read before initialization",
        }
    }
}
