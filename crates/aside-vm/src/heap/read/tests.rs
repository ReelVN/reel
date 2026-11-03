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
    let expected = heap
        .key(value)
        .and_then(|key| heap.object(key))
        .map(|object| {
            (
                object.kind,
                match &object.payload {
                    Payload::Table(table) => table.values.len / 2,
                    _ => object.payload.len(),
                },
                object.content_version,
                object.structure_version,
            )
        });
    assert_eq!(
        heap.read_object(value).map(|read| (
            read.kind(),
            read.len(),
            read.info().content_version,
            read.info().structure_version
        )),
        expected,
    );
    assert_eq!(
        heap.accepts_builtin(crate::bytecode::BuiltinType::Any, value),
        if value.tag() == crate::ValueTag::Object {
            expected.map(|_| true)
        } else {
            Ok(true)
        },
    );
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

#[test]
fn insertion_matches_validate_then_shade_in_every_collection_phase() {
    fn fixture(phase: GcPhase, case: u8) -> (Heap, Value, HeapKey) {
        let mut heap = Heap::new(SessionEpoch(4), HeapLimits::default()).unwrap();
        let value = heap.alloc_array(&[Value::int(9)]).unwrap();
        let key = heap.key(value).unwrap();
        let object = heap.objects.get_mut(key).unwrap();
        object.mark = if case == 1 { 42 } else { 41 };
        object.scan = 1;
        object.metadata_scanned = true;
        object.reactive_scanned = true;
        object.queued = false;
        object.dying = case == 2;
        heap.gc.cycle = 42;
        heap.gc.phase = phase;
        let value = match case {
            3 => Value::NULL,
            4 => Value::float(f64::from_bits(0x8000_0000_0000_0000)),
            5 => {
                let (_, bits) = value.object_parts().unwrap();
                Value::object(heap.id(), bits ^ (1 << 32))
            }
            6 => {
                let (_, bits) = value.object_parts().unwrap();
                Value::object(HeapId(0), bits)
            }
            _ => value,
        };
        (heap, value, key)
    }
    for phase in [GcPhase::Idle, GcPhase::Mark, GcPhase::Weak, GcPhase::Sweep] {
        for case in 0..=6 {
            let (mut actual, value, key) = fixture(phase, case);
            let (mut expected, old_value, old_key) = fixture(phase, case);
            // Independent pre-merge contract, including error priority and the
            // gray queue/scan state, rather than only eventual reachability.
            let old = expected
                .validate_value(old_value)
                .map(|()| expected.shade(old_value));
            assert_eq!(actual.root_insertion_barrier(value), old);
            assert_eq!(actual.gray, expected.gray);
            let state = |heap: &Heap, key| {
                let object = heap.objects.get(key).unwrap();
                (
                    object.mark,
                    object.scan,
                    object.metadata_scanned,
                    object.reactive_scanned,
                    object.queued,
                    object.dying,
                )
            };
            assert_eq!(state(&actual, key), state(&expected, old_key));
            assert_eq!(actual.stats().charged_bytes, expected.stats().charged_bytes);
        }
    }
}

#[test]
fn callable_view_keeps_host_retirement_and_kind_error_priority() {
    let mut heap = Heap::new(SessionEpoch(5), HeapLimits::default()).unwrap();
    let fields = heap.alloc_table().unwrap();
    let host = heap.alloc_host(fields, &[], None).unwrap();
    let ordinary = heap.alloc_array(&[]).unwrap();
    for value in [Value::NULL, ordinary, host] {
        assert_eq!(
            heap.read_object(value).and_then(|read| read.host_call()),
            heap.host_call(value)
        );
    }
    heap.set_host_call(host, crate::bytecode::FunctionId(7))
        .unwrap();
    assert_eq!(
        heap.read_object(host).unwrap().host_call(),
        heap.host_call(host)
    );
    heap.retire_host(host).unwrap();
    assert_eq!(
        heap.read_object(host).unwrap().host_call(),
        Err(HeapError::RetiredHost)
    );
    assert_eq!(
        heap.read_object(host).unwrap().host_call(),
        heap.host_call(host)
    );
}

#[test]
fn host_views_preserve_fields_payload_bounds_and_retired_payload_reads() {
    fn old_fields(heap: &Heap, value: Value) -> Result<Value, HeapError> {
        if heap.object(heap.key(value)?)?.kind != ObjectKind::Host {
            return Err(HeapError::WrongKind);
        }
        let fields = checked_field(heap, value, 0)?;
        if fields.same_bits(Value::NULL) {
            return Err(HeapError::RetiredHost);
        }
        Ok(fields)
    }
    fn old_payload_len(heap: &Heap, value: Value) -> Result<u32, HeapError> {
        let object = heap.object(heap.key(value)?)?;
        if object.kind != ObjectKind::Host {
            return Err(HeapError::WrongKind);
        }
        object
            .payload
            .len()
            .checked_sub(4)
            .ok_or(HeapError::WrongKind)
    }
    fn old_payload(heap: &Heap, value: Value, index: u32) -> Result<Value, HeapError> {
        if index >= old_payload_len(heap, value)? {
            return Err(HeapError::OutOfBounds);
        }
        checked_field(heap, value, index + 4)
    }
    fn compare_host(heap: &Heap, value: Value) {
        assert_field(heap.host_fields(value), &old_fields(heap, value));
        assert_eq!(heap.host_payload_len(value), old_payload_len(heap, value));
        for index in [0, 1, 7, 8, 129, 130, u32::MAX] {
            assert_field(
                heap.host_payload(value, index),
                &old_payload(heap, value, index),
            );
        }
    }
    let mut heap = Heap::new(SessionEpoch(6), HeapLimits::default()).unwrap();
    let mut other = Heap::new(SessionEpoch(6), HeapLimits::default()).unwrap();
    let fields = heap.alloc_table().unwrap();
    let empty = heap.alloc_host(fields, &[], None).unwrap();
    let host = heap
        .alloc_host(fields, &[Value::int(9); 130], None)
        .unwrap();
    let array = heap.alloc_array(&[]).unwrap();
    let foreign_fields = other.alloc_table().unwrap();
    let foreign = other.alloc_host(foreign_fields, &[], None).unwrap();
    let (_, bits) = host.object_parts().unwrap();
    let stale = Value::object(heap.id(), bits ^ (1 << 32));
    for value in [
        Value::NULL,
        Value::int(1),
        array,
        empty,
        host,
        foreign,
        stale,
    ] {
        compare_host(&heap, value);
    }
    heap.retire_host(host).unwrap();
    compare_host(&heap, host);
    assert!(matches!(
        heap.host_fields(host),
        Err(HeapError::RetiredHost)
    ));
    assert!(heap.host_payload(host, 129).unwrap().same_bits(Value::NULL));
    heap.begin_collection().unwrap();
    while !matches!(heap.phase(), GcPhase::Weak | GcPhase::Sweep) {
        heap.gc_step(crate::GcBudget {
            units: 1,
            ..Default::default()
        })
        .unwrap();
    }
    compare_host(&heap, host);
    assert_eq!(heap.host_payload_len(host), Err(HeapError::UnconfirmedLive));
    while heap.phase() != GcPhase::Idle {
        heap.gc_step(crate::GcBudget::default()).unwrap();
    }
    compare_host(&heap, host);
    assert_eq!(heap.host_payload_len(host), Err(HeapError::InvalidHandle));
}
