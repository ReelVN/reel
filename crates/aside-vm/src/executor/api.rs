//! Public V03 slice, rooted delivery and native response contract.
use crate::bytecode::{self, *};
use crate::{
    FrameSlotsLimits, FrameSlotsStats, HeapError, HeapId, HeapLimits, HeapStats, RootOwner,
    SessionEpoch, Value,
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskId {
    pub(super) heap: HeapId,
    pub(super) epoch: SessionEpoch,
    pub(super) serial: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModuleInstanceId(pub u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodeRef {
    pub function: FunctionId,
    pub version: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeInput {
    Entry,
    Continue { version: u64, stage: u64 },
    CallbackReturned,
    CallbackFault,
    WaitingReturned,
    WaitingFault,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockedResource {
    HeapObjects,
    CatchRecords,
    UnwindRecords,
    NativeFaultSlots { frame: FrameId },
    RootRanges,
    WindowValues,
    HeapBytes,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourceBlocked {
    pub task: TaskId,
    pub resource: BlockedResource,
    pub current: u64,
    pub required: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
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
    pub max_tasks: u32,
    pub heap: HeapLimits,
    pub slots: FrameSlotsLimits,
    pub max_frames: u32,
    pub max_unwinds: u32,
    pub max_arguments: u32,
    pub max_continuation_values: u32,
    pub max_native_faults: u32,
}
impl Default for VmLimits {
    fn default() -> Self {
        Self {
            max_tasks: 16,
            heap: HeapLimits::default(),
            slots: FrameSlotsLimits {
                initial_capacity: 16384,
                max_capacity: 16384,
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
    pub native_entries: u64,
    pub native_resumes: u64,
    /// Redundant whole-native-window initialization writes (removed in V03).
    pub native_null_publications: u64,
    pub slices: u64,
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
    Opcode::Jump,
    Opcode::BranchBool,
    Opcode::BranchNull,
    Opcode::MakeClosure,
    Opcode::Call,
    Opcode::Return,
    Opcode::ArgsNew,
    Opcode::ArgsPush,
    Opcode::ArgsNamed,
    Opcode::Throw,
    Opcode::Rethrow,
    Opcode::Leave,
    Opcode::EndFinally,
];

impl FaultKind {
    pub fn message(self) -> &'static str {
        match self {
            Self::Thrown => "explicit throw",
            Self::Type => "type contract failed",
            Self::Uninitialized => "read before initialization",
            Self::AlreadyInitialized => "binding already initialized",
            Self::ReadOnly => "read-only binding",
            Self::DuplicateArgument => "duplicate argument",
            Self::MissingArgument => "missing argument",
            Self::UnknownArgument => "unknown argument",
            Self::Overflow => "integer overflow",
            Self::DivisionByZero => "division by zero",
            Self::Effect => "effect constraint",
            Self::Resource => "resource capacity",
            Self::InvalidState => "invalid execution state",
            Self::Cancelled => "cooperative cancellation",
        }
    }
}
