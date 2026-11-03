//! Cell reads preserve the previous validation, proposal and observation order.
use super::*;
use crate::CellContract;

fn ordinary_reference(heap: &Heap, cell: Value) -> Result<Value, HeapError> {
    heap.cell_info(cell)?;
    Ok(heap
        .proposed_property(cell, 0)
        .unwrap_or(heap.get_field(cell, 0)?))
}

fn observed_reference(heap: &mut Heap, cell: Value) -> Result<Option<Value>, HeapError> {
    if !heap.cell_info(cell)?.initialized {
        return Ok(None);
    }
    heap.observe_read(cell, Some(0))?;
    ordinary_reference(heap, cell).map(Some)
}

fn same(actual: Result<Value, HeapError>, expected: Result<Value, HeapError>) {
    match (actual, expected) {
        (Ok(actual), Ok(expected)) => assert!(actual.same_bits(expected)),
        (Err(actual), Err(expected)) => assert_eq!(actual, expected),
        other => panic!("Cell read diverged: {other:?}"),
    }
}

fn heap() -> Heap {
    Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap()
}

#[test]
fn validated_cells_preserve_kinds_identity_initialization_and_contracts() {
    let mut heap = heap();
    let mut foreign = self::heap();
    let uninitialized = heap
        .alloc_language_cell(CellContract::untyped_mutable())
        .unwrap();
    let null = heap.alloc_cell(Value::NULL).unwrap();
    let integer = heap.alloc_cell(Value::int(41)).unwrap();
    let array = heap.alloc_array(&[Value::int(41)]).unwrap();
    let foreign = foreign.alloc_cell(Value::int(41)).unwrap();
    let (_, key) = integer.object_parts().unwrap();
    let forged = Value::object(heap.id(), key ^ (1 << 32));
    for value in [
        uninitialized,
        null,
        integer,
        array,
        foreign,
        forged,
        Value::NULL,
    ] {
        same(heap.cell_read(value), ordinary_reference(&heap, value));
        let expected = observed_reference(&mut heap, value);
        let actual = heap.observed_cell_value(value);
        match (actual, expected) {
            (Ok(Some(actual)), Ok(Some(expected))) => assert!(actual.same_bits(expected)),
            (Ok(None), Ok(None)) => {}
            (Err(actual), Err(expected)) => assert_eq!(actual, expected),
            other => panic!("observed Cell read diverged: {other:?}"),
        }
    }
    let contract = heap.cell_info(uninitialized).unwrap().contract;
    heap.cell_initialize(uninitialized, Value::int(7)).unwrap();
    assert!(heap.cell_info(uninitialized).unwrap().initialized);
    assert_eq!(heap.cell_info(uninitialized).unwrap().contract, contract);
    same(
        heap.cell_read(uninitialized),
        ordinary_reference(&heap, uninitialized),
    );
    heap.begin_collection().unwrap();
    while heap.phase() != GcPhase::Idle {
        heap.gc_step(crate::GcBudget::default()).unwrap();
    }
    let replacement = heap.alloc_cell(Value::int(99)).unwrap();
    same(heap.cell_read(integer), ordinary_reference(&heap, integer));
    assert!(heap.cell_read(integer).is_err());
    same(
        heap.cell_read(replacement),
        ordinary_reference(&heap, replacement),
    );
}

#[test]
fn stored_initialization_wins_over_a_proposal_and_observation_errors() {
    let mut heap = heap();
    let cell = heap
        .alloc_language_cell(CellContract::untyped_mutable())
        .unwrap();
    heap.observe_model_object(cell).unwrap();
    heap.property_proposal = Some(Value::int(77));
    heap.property_work.push(PropertyId {
        host: cell,
        field: 0,
    });
    assert_eq!(heap.proposed_property(cell, 0).unwrap().as_int(), Some(77));
    same(heap.cell_read(cell), ordinary_reference(&heap, cell));
    assert_eq!(
        heap.cell_read(cell).unwrap_err(),
        HeapError::UninitializedCell
    );
    // A broken observer must not be touched before the initialization check.
    heap.reactive_capture = Some(Value::NULL);
    assert!(observed_reference(&mut heap, cell).unwrap().is_none());
    assert!(heap.observed_cell_value(cell).unwrap().is_none());
    heap.reactive_capture = None;
    heap.cell_initialize(cell, Value::int(11)).unwrap();
    heap.reactive_capture = Some(Value::NULL);
    assert_eq!(
        heap.observed_cell_value(cell).unwrap_err(),
        observed_reference(&mut heap, cell).unwrap_err()
    );
    heap.reactive_capture = None;
    same(heap.cell_read(cell), ordinary_reference(&heap, cell));
    assert_eq!(heap.cell_read(cell).unwrap().as_int(), Some(77));
    assert_eq!(
        heap.observed_cell_value(cell).unwrap().unwrap().as_int(),
        Some(77)
    );
    assert_eq!(heap.get_field(cell, 0).unwrap().as_int(), Some(11));
}

#[test]
fn observed_cells_keep_semantic_dependencies_without_observing_ordinary_reads() {
    for reference in [false, true] {
        for observed in [false, true] {
            let mut heap = heap();
            let cell = heap.alloc_cell(Value::int(41)).unwrap();
            heap.observe_model_object(cell).unwrap();
            let output = heap.alloc_derived(Value::NULL).unwrap();
            heap.begin_derived(output).unwrap();
            let value = if observed {
                if reference {
                    observed_reference(&mut heap, cell).unwrap().unwrap()
                } else {
                    heap.observed_cell_value(cell).unwrap().unwrap()
                }
            } else if reference {
                ordinary_reference(&heap, cell).unwrap()
            } else {
                heap.cell_read(cell).unwrap()
            };
            assert_eq!(value.as_int(), Some(41));
            heap.commit_derived(output, value).unwrap();
            assert!(heap.derived_is_current(output, None).unwrap());
            heap.cell_write(cell, Value::int(42)).unwrap();
            assert_eq!(heap.derived_is_current(output, None).unwrap(), !observed);
        }
    }
}
