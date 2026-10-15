//! Aside storage, verified bytecode and a budgeted resumable execution core.
#![forbid(unsafe_code)]
pub mod bytecode;
mod cell;
mod class;
pub mod executor;
mod fault;
mod frame_slots;
mod gc;
mod heap;
mod payload;
mod root_ranges;
mod slots;
mod type_checks;
mod value;

pub use cell::{CellContract, CellInfo};
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
    Heap, HeapError, HeapLimits, HeapStats, HostPropertyValidator, ObjectInfo, ObjectKind,
    PropertyId, RootKind, RootOwner, RootToken, TableLookupProgress, TableLookupStatus,
    TableStringLookup,
};
pub use payload::{
    BYTE_CHUNK_BYTES, BYTE_CHUNK_LEN, CHUNK_METADATA_BYTES, VALUE_CHUNK_BYTES, VALUE_CHUNK_LEN,
};
pub use root_ranges::{HeapBudgetUsage, Reservation as HeapReservation, RootRangeToken};
pub use value::{HeapHandle, HeapId, SessionEpoch, Value, ValueTag, WeakHandle};

// Enforced by both native and 32-bit cross compilation, without runtime linking.
const _: () = assert!(std::mem::size_of::<Value>() == 16);
const _: () = assert!(std::mem::align_of::<Value>() == std::mem::align_of::<u64>());
