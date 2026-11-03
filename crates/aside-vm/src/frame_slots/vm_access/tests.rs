//! Scope boundaries preserve the public rejection and root-lifetime contracts.
use super::*;
use crate::SessionEpoch;

fn pool() -> (Heap, FrameSlots, RootRangeToken) {
    let mut heap = Heap::new(SessionEpoch(91), crate::HeapLimits::default()).unwrap();
    let mut slots = FrameSlots::new(
        &mut heap,
        FrameSlotsLimits {
            initial_capacity: 8,
            max_capacity: 128,
            max_window_len: 128,
        },
    )
    .unwrap();
    let range = slots
        .open(&mut heap, RootOwner(1), RootKind::TaskFrame, 2)
        .unwrap();
    slots
        .write(&mut heap, RangePosition { range, index: 0 }, Value::int(7))
        .unwrap();
    (heap, slots, range)
}

#[test]
fn public_access_keeps_heap_epoch_and_provider_rejection_during_vm_scope() {
    let (mut heap, mut slots, range) = pool();
    let (mut foreign_heap, foreign_slots, foreign_range) = pool();
    let scope = slots.begin_vm_access(&heap).unwrap();
    assert!(slots.vm_access_active());
    assert_eq!(
        slots
            .read(RangePosition {
                range: foreign_range,
                index: 0
            })
            .unwrap_err(),
        HeapError::WrongHeap
    );
    let mut wrong_epoch = range;
    wrong_epoch.epoch = SessionEpoch(range.epoch.0 + 1);
    assert_eq!(
        slots
            .read(RangePosition {
                range: wrong_epoch,
                index: 0
            })
            .unwrap_err(),
        HeapError::WrongEpoch
    );
    assert_eq!(
        slots
            .write(
                &mut foreign_heap,
                RangePosition { range, index: 0 },
                Value::int(9)
            )
            .unwrap_err(),
        HeapError::WrongHeap
    );
    // The private write is also bound to the actual Heap argument, not a flag.
    assert_eq!(
        slots
            .write_vm(
                &mut foreign_heap,
                RangePosition { range, index: 0 },
                Value::int(9)
            )
            .unwrap_err(),
        HeapError::WrongHeap
    );
    assert_eq!(
        slots
            .read_vm(RangePosition { range, index: 0 })
            .unwrap()
            .as_int(),
        Some(7)
    );
    drop(scope);
    assert!(!slots.vm_access_active());
    slots.close(&mut heap).unwrap();
    drop(foreign_slots);
}

#[test]
fn scope_keeps_generation_bounds_bitmap_and_current_relocated_mapping() {
    let (mut heap, mut slots, range) = pool();
    let scope = slots.begin_vm_access(&heap).unwrap();
    assert_eq!(
        slots
            .read_vm(RangePosition { range, index: 1 })
            .unwrap_err(),
        HeapError::UninitializedSlot
    );
    assert_eq!(
        slots
            .read_vm(RangePosition { range, index: 2 })
            .unwrap_err(),
        HeapError::OutOfBounds
    );
    slots.relocate(&mut heap, 64).unwrap();
    assert_eq!(
        slots
            .read_vm(RangePosition { range, index: 0 })
            .unwrap()
            .as_int(),
        Some(7)
    );
    slots.release(&mut heap, range).unwrap();
    let replacement = slots
        .open_vm(&mut heap, RootOwner(1), RootKind::TaskFrame, 2)
        .unwrap();
    assert_ne!(range, replacement);
    assert_eq!(
        slots
            .read_vm(RangePosition { range, index: 0 })
            .unwrap_err(),
        HeapError::InvalidRootRange
    );
    assert_eq!(
        slots
            .read_vm(RangePosition {
                range: replacement,
                index: 0
            })
            .unwrap_err(),
        HeapError::UninitializedSlot
    );
    drop(scope);
    slots.close(&mut heap).unwrap();
}

#[test]
fn proof_ends_on_unwind_close_and_heap_drop() {
    let (mut heap, mut slots, range) = pool();
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _scope = slots.begin_vm_access(&heap).unwrap();
        panic!("external clock panic");
    }));
    assert!(failed.is_err());
    assert!(!slots.vm_access_active());
    let scope = slots.begin_vm_access(&heap).unwrap();
    slots.close(&mut heap).unwrap();
    assert!(!slots.vm_access_active());
    assert_eq!(
        slots
            .read_vm(RangePosition { range, index: 0 })
            .unwrap_err(),
        HeapError::WrongRootProvider
    );
    drop(scope);
    let (heap, slots, range) = pool();
    let scope = slots.begin_vm_access(&heap).unwrap();
    drop(heap);
    assert!(!slots.vm_access_active());
    assert_eq!(
        slots
            .read_vm(RangePosition { range, index: 0 })
            .unwrap_err(),
        HeapError::HeapUnavailable
    );
    drop(scope);
}

#[test]
fn failed_proof_preserves_original_checked_access() {
    let (heap, slots, range) = pool();
    let (foreign, _, _) = pool();
    assert!(slots.begin_vm_access(&foreign).is_none());
    assert!(!slots.vm_access_active());
    assert_eq!(
        slots
            .read_vm(RangePosition { range, index: 0 })
            .unwrap()
            .as_int(),
        Some(7)
    );
    heap.usage.0.provider_state.set(RootProviderState::Detached);
    assert!(slots.begin_vm_access(&heap).is_none());
    assert_eq!(
        slots
            .read_vm(RangePosition { range, index: 0 })
            .unwrap_err(),
        HeapError::MissingRootProvider
    );
    heap.usage.0.provider_state.set(RootProviderState::Attached);
}

#[test]
fn execution_proof_reuses_the_existing_provider_control_byte() {
    use std::cell::{Cell, OnceCell};
    use std::sync::{Arc, atomic::AtomicUsize};
    struct OriginalBook {
        _limit: u64,
        _used: Cell<u64>,
        _peak: Cell<u64>,
        _heap_alive: Cell<bool>,
        _attachment: Cell<Option<u64>>,
        _provider_alive: Cell<bool>,
        _shared_refunds: OnceCell<Arc<AtomicUsize>>,
        _shared_bytes: Cell<usize>,
    }
    assert_eq!(size_of::<RootProviderState>(), size_of::<bool>());
    assert_eq!(
        size_of::<crate::root_ranges::ByteBook>(),
        size_of::<OriginalBook>()
    );
    let (mut heap, mut slots, _) = pool();
    let before = heap.budget_usage().charged_bytes();
    let scope = slots.begin_vm_access(&heap).unwrap();
    assert_eq!(heap.budget_usage().charged_bytes(), before);
    drop(scope);
    assert_eq!(heap.budget_usage().charged_bytes(), before);
    slots.close(&mut heap).unwrap();
    println!(
        "provider-control-byte old={} new={} book-old={} book-new={}",
        size_of::<bool>(),
        size_of::<RootProviderState>(),
        size_of::<OriginalBook>(),
        size_of::<crate::root_ranges::ByteBook>()
    );
}
