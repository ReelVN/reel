//! Existing per-VM member metadata grouped for one short execution borrow.
use super::{member_hints, method_bindings};

/// Non-owning positions and weak bindings; load/restore constructs this cold.
/// Table positions retain their separate store and unchanged hash/work policy.
#[derive(Default)]
pub(super) struct MemberState {
    pub class_hints: member_hints::MemberHints<{ member_hints::CLASS_SLOTS }>,
    pub bindings: method_bindings::MethodBindings,
    pub script_hits: u64,
    pub script_misses: u64,
    pub native_hits: u64,
    pub native_misses: u64,
}

// Group the existing fields without another allocation or metadata store.
const _: () = assert!(
    size_of::<MemberState>()
        == size_of::<member_hints::MemberHints<{ member_hints::CLASS_SLOTS }>>()
            + size_of::<method_bindings::MethodBindings>()
            + 4 * size_of::<u64>()
);
