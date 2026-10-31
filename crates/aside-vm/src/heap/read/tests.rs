use super::*;

// The pre-view accessor is an independent error-order oracle for this boundary.
fn checked_field(heap: &Heap, value: Value, position: u32) -> Result<Value, HeapError> {
    let object = heap.object(heap.key(value)?)?;
    if !object.kind.values() || matches!(object.payload, Payload::Table(_)) {
        return Err(HeapError::WrongKind);
    }
    if let Payload::Cell(cell) = &object.payload {
        if position != 0 {
            return Err(HeapError::OutOfBounds);
        }
        return cell.value.ok_or(HeapError::UninitializedCell);
    }
    object.payload.edge(position).ok_or(HeapError::OutOfBounds)
}

fn compare(heap: &Heap, value: Value) {
    for position in [0, 1, 7, 8, 31, 32, 64, u32::MAX] {
        let expected = checked_field(heap, value, position);
        assert_field(heap.get_field(value, position), &expected);
        assert_field(
            heap.read_object(value)
                .and_then(|read| read.field(position)),
            &expected,
        );
    }
}

fn assert_field(actual: Result<Value, HeapError>, expected: &Result<Value, HeapError>) {
    match (actual, expected) {
        (Ok(actual), Ok(expected)) => assert!(actual.same_bits(*expected)),
        (Err(actual), Err(expected)) => assert_eq!(&actual, expected),
        (actual, expected) => panic!("field mismatch {actual:?} != {expected:?}"),
    }
}

#[test]
fn read_views_preserve_kind_bounds_cell_initialization_and_full_handle_checks() {
    let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
    let mut other = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
    let empty = heap.alloc_array(&[]).unwrap();
    let inline = heap.alloc_tuple(&[Value::int(3), Value::int(9)]).unwrap();
    let chunked = heap.alloc_array(&[Value::int(7); 130]).unwrap();
    let cell = heap.alloc_cell(Value::NULL).unwrap();
    let uninitialized = heap
        .alloc_language_cell(crate::CellContract::untyped_mutable())
        .unwrap();
    let text = heap.alloc_string("view").unwrap();
    let table = heap.alloc_table().unwrap();
    let foreign = other.alloc_array(&[]).unwrap();
    let (_, bits) = inline.object_parts().unwrap();
    let stale = Value::object(heap.id(), bits ^ (1 << 32));
    for value in [
        Value::NULL,
        Value::int(0),
        empty,
        inline,
        chunked,
        cell,
        uninitialized,
        text,
        table,
        foreign,
        stale,
    ] {
        compare(&heap, value);
    }
    let read = heap.read_object(chunked).unwrap();
    assert_eq!(read.kind(), ObjectKind::Array);
    assert_eq!(read.info().len, 130);
    assert_eq!(read.field(0).unwrap().as_int(), Some(7));
    assert_eq!(read.field(129).unwrap().as_int(), Some(7));
}

#[test]
fn reacquiring_after_mutation_or_gc_observes_the_current_storage_and_liveness() {
    let mut heap = Heap::new(SessionEpoch(2), HeapLimits::default()).unwrap();
    let live = heap.alloc_array(&[Value::int(1)]).unwrap();
    heap.add_root(RootOwner(1), RootKind::Host, live).unwrap();
    let dead = heap.alloc_array(&[Value::int(2)]).unwrap();
    assert_eq!(
        heap.read_object(live).unwrap().field(0).unwrap().as_int(),
        Some(1)
    );
    heap.set_field(live, 0, Value::int(8)).unwrap();
    assert_eq!(
        heap.read_object(live).unwrap().field(0).unwrap().as_int(),
        Some(8)
    );
    heap.begin_collection().unwrap();
    while !matches!(heap.phase(), GcPhase::Weak | GcPhase::Sweep) {
        heap.gc_step(crate::GcBudget {
            units: 1,
            ..Default::default()
        })
        .unwrap();
    }
    compare(&heap, live);
    compare(&heap, dead);
    assert!(matches!(
        heap.get_field(dead, 0),
        Err(HeapError::UnconfirmedLive)
    ));
    while heap.phase() != GcPhase::Idle {
        heap.gc_step(crate::GcBudget::default()).unwrap();
    }
    compare(&heap, dead);
    assert!(matches!(
        heap.get_field(dead, 0),
        Err(HeapError::InvalidHandle)
    ));
    compare(&heap, live);
}
