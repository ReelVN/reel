//! Aside storage, verified bytecode and a budgeted resumable execution core.
#![forbid(unsafe_code)]
mod builtin_methods;
pub mod bytecode;
pub use builtin_methods::{BuiltinDeclaration, BuiltinMethod, BuiltinParameter};
mod cell;
mod class;
mod dependencies;
pub mod executor;
mod fault;
mod frame_slots;
mod gc;
mod heap;
mod host_types;
mod payload;
mod root_ranges;
mod slots;
pub mod snapshot;
mod type_checks;
mod value;

pub use cell::{CellContract, CellInfo};
pub use class::InstanceField;
pub use fault::{
    CauseInfo, CauseKind, CloseKind, CoreErrorInfo, FaultCauseCursor, FaultCode, FaultGraphCursor,
    FaultGraphStep, FaultId, FaultInfo, FaultRoot, FaultStackCursor, NewFault, ReplacedExit,
    StackEntry, StackFrameKind,
};
pub use frame_slots::{
    FrameSlots, FrameSlotsLimits, FrameSlotsStats, RangePosition, RangeReleaseCursor,
    RangeReleaseProgress, RelocationCost, RootRanges,
};
pub use gc::{GcBudget, GcPhase, GcProgress};
pub use heap::{
    ChangeCursor, Heap, HeapError, HeapLimits, HeapStats, HostPropertyValidator, ObjectInfo,
    ObjectKind, PropertyBatch, PropertyBatchProgress, PropertyId, PropertyRead, PropertyWrites,
    RootKind, RootOwner, RootToken, TableLookupProgress, TableLookupStatus, TableStringLookup,
};
pub use host_types::{HostMethodSupport, HostTypeId, HostTypeSupport};
pub use payload::{
    BYTE_CHUNK_BYTES, BYTE_CHUNK_LEN, CHUNK_METADATA_BYTES, VALUE_CHUNK_BYTES, VALUE_CHUNK_LEN,
};
pub use root_ranges::{
    HeapBudgetUsage, Reservation as HeapReservation, RootRangeToken, SharedHeapReservation,
};
pub use value::{HeapHandle, HeapId, SessionEpoch, Value, ValueTag, WeakHandle};

// Enforced by both native and 32-bit cross compilation, without runtime linking.
const _: () = assert!(std::mem::size_of::<Value>() == 16);
const _: () = assert!(std::mem::align_of::<Value>() == std::mem::align_of::<u64>());
