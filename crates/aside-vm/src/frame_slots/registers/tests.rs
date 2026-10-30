use super::*;
use crate::{GcBudget, GcPhase, HeapLimits};

fn pool() -> (Heap, FrameSlots) {
    let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
    let slots = FrameSlots::new(
        &mut heap,
        FrameSlotsLimits {
            initial_capacity: 256,
            max_capacity: 256,
            max_window_len: 128,
        },
    )
    .unwrap();
    (heap, slots)
}

#[test]
fn register_view_matches_checked_access_at_bitmap_boundaries() {
    for prefix in [0, 1, 63, 64, 65] {
        let (mut heap, mut slots) = pool();
        let _prefix = slots
            .open(&mut heap, RootOwner(1), RootKind::Arguments, prefix)
            .unwrap();
        let range = slots
            .open(&mut heap, RootOwner(1), RootKind::TaskFrame, 70)
            .unwrap();
        {
            let mut registers = slots.registers(&mut heap, range).unwrap();
            assert_eq!(registers.read(0).unwrap_err(), HeapError::UninitializedSlot);
            assert_eq!(registers.read(70).unwrap_err(), HeapError::OutOfBounds);
            for index in 0..70 {
                registers
                    .write(index, Value::int(i64::from(index)))
                    .unwrap();
            }
            let (first, last) = registers.read_pair(0, 69).unwrap();
            assert_eq!((first.as_int(), last.as_int()), (Some(0), Some(69)));
            // A later invalid clear retains the preceding visible writes.
            assert_eq!(registers.clear([63, 64, 70]), Err(HeapError::OutOfBounds));
        }
        for index in 0..70 {
            let value = slots.read(RangePosition { range, index });
            if matches!(index, 63 | 64) {
                assert_eq!(value.unwrap_err(), HeapError::UninitializedSlot);
            } else {
                assert_eq!(value.unwrap().as_int(), Some(i64::from(index)));
            }
        }
        slots.release(&mut heap, range).unwrap();
        assert!(matches!(
            slots.registers(&mut heap, range),
            Err(HeapError::InvalidRootRange)
        ));
    }
}

#[test]
fn register_view_retains_root_barriers_and_checks_its_heap() {
    let (mut heap, mut slots) = pool();
    let range = slots
        .open(&mut heap, RootOwner(1), RootKind::TaskFrame, 2)
        .unwrap();
    let value = heap.alloc_array(&[Value::int(42)]).unwrap();
    let mut other = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
    assert!(matches!(
        slots.registers(&mut other, range),
        Err(HeapError::WrongHeap)
    ));
    heap.begin_collection().unwrap();
    {
        let mut registers = slots.registers(&mut heap, range).unwrap();
        registers.write(0, value).unwrap();
        // A cross-heap object is rejected before replacing the current root.
        let foreign = other.alloc_array(&[]).unwrap();
        assert_eq!(registers.write(0, foreign), Err(HeapError::WrongHeap));
        assert!(registers.read(0).unwrap().same_bits(value));
    }
    while heap.phase() != GcPhase::Idle {
        heap.gc_step_with_ranges(
            GcBudget {
                units: 1,
                ..GcBudget::default()
            },
            slots.roots(),
        )
        .unwrap();
    }
    assert_eq!(heap.sequence_get(value, 0).unwrap().as_int(), Some(42));
}
