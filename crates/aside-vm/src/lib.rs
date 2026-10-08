//! Aside's storage milestone. No parser, execution scheduler or native callbacks.
#![forbid(unsafe_code)]
mod frame_slots;
mod gc;
mod heap;
mod payload;
mod root_ranges;
mod slots;
mod value;

pub use frame_slots::{
    FrameSlots, FrameSlotsLimits, FrameSlotsStats, RangePosition, RangeReleaseCursor,
    RangeReleaseProgress, RelocationCost, RootRanges,
};
pub use gc::{GcBudget, GcPhase, GcProgress};
pub use heap::{
    Heap, HeapError, HeapLimits, HeapStats, ObjectInfo, ObjectKind, RootKind, RootOwner, RootToken,
};
pub use payload::{
    BYTE_CHUNK_BYTES, BYTE_CHUNK_LEN, CHUNK_METADATA_BYTES, VALUE_CHUNK_BYTES, VALUE_CHUNK_LEN,
};
pub use root_ranges::{HeapBudgetUsage, RootRangeToken};
pub use value::{HeapHandle, HeapId, SessionEpoch, Value, ValueTag, WeakHandle};

// Enforced by both native and 32-bit cross compilation, without runtime linking.
const _: () = assert!(std::mem::size_of::<Value>() == 16);
const _: () = assert!(std::mem::align_of::<Value>() == std::mem::align_of::<u64>());
