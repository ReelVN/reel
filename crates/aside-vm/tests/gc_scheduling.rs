use aside_vm::*;

fn heap() -> Heap {
    Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap()
}

fn finish(heap: &mut Heap) {
    while heap.phase() != GcPhase::Idle {
        let p = heap.gc_step(GcBudget::default()).unwrap();
        assert!(p.units <= 256 && p.bytes <= 16_384);
    }
}

#[test]
fn stable_writes_do_not_trigger_scans_and_explicit_collection_is_immediate() {
    let mut heap = heap();
    let kept = heap.alloc_array(&[]).unwrap();
    let root = heap.add_root(RootOwner(1), RootKind::Host, kept).unwrap();
    for i in 0..1_000 {
        let cell = heap.alloc_cell(Value::int(i)).unwrap();
        heap.array_push(kept, cell).unwrap();
    }
    heap.begin_collection().unwrap();
    finish(&mut heap);
    assert!(!heap.collection_due());
    for i in 0..10_000 {
        let cell = heap.sequence_get(kept, i % 1000).unwrap();
        heap.cell_write(cell, Value::int(i.into())).unwrap();
    }
    assert!(!heap.collection_due());
    heap.begin_collection().unwrap();
    assert_eq!(heap.phase(), GcPhase::Mark);
    finish(&mut heap);
    assert_eq!(heap.stats().live_objects, 1001);
    assert!(heap.root_value(root).unwrap().same_bits(kept));
}

#[test]
fn allocations_during_collection_remain_due_and_have_no_unbounded_gc_work() {
    let mut heap = heap();
    heap.begin_collection().unwrap();
    for _ in 0..256 {
        heap.alloc_array(&[]).unwrap();
    }
    finish(&mut heap);
    assert!(heap.collection_due());
    assert_eq!(heap.stats().live_objects, 256);
    heap.begin_collection().unwrap();
    let allocated = allocation_counter::measure(|| finish(&mut heap));
    assert_eq!(allocated.count_total, 0);
    assert_eq!(heap.stats().live_objects, 0);
    assert!(!heap.collection_due());
}

#[test]
fn payload_pressure_triggers_below_the_object_floor() {
    let mut heap = heap();
    let value = "x".repeat(4096);
    for _ in 0..16 {
        heap.alloc_string(&value).unwrap();
    }
    assert!(heap.collection_due());
    assert!(heap.stats().live_objects < 256);
    heap.begin_collection().unwrap();
    finish(&mut heap);
    assert_eq!(heap.stats().payload_bytes, 0);
    assert!(!heap.collection_due());
}

#[test]
fn clearing_an_already_marked_graph_requests_a_following_cycle_and_preserves_aliases() {
    let mut heap = heap();
    let values = heap.alloc_array(&[]).unwrap();
    heap.add_root(RootOwner(1), RootKind::Host, values).unwrap();
    let retained = heap.alloc_cell(Value::int(42)).unwrap();
    heap.add_root(RootOwner(1), RootKind::Host, retained)
        .unwrap();
    heap.array_push(values, retained).unwrap();
    for _ in 0..1000 {
        let cell = heap.alloc_cell(Value::NULL).unwrap();
        heap.array_push(values, cell).unwrap();
    }
    heap.begin_collection().unwrap();
    while heap.phase() == GcPhase::Mark {
        heap.gc_step(GcBudget {
            weak: 0,
            ..Default::default()
        })
        .unwrap();
    }
    heap.clear_collection(values).unwrap();
    assert_eq!(heap.sequence_len(values).unwrap(), 0);
    finish(&mut heap);
    assert!(heap.collection_due());
    heap.begin_collection().unwrap();
    finish(&mut heap);
    assert_eq!(heap.cell_read(retained).unwrap().as_int(), Some(42));
    assert_eq!(heap.stats().live_objects, 2);
    assert!(!heap.collection_due());
    heap.clear_collection(values).unwrap();
    assert!(
        !heap.collection_due(),
        "empty clear must not rescan the heap"
    );
}

#[test]
fn finite_byte_quota_reclaims_inline_objects_before_storage_growth_fails() {
    let baseline = heap().stats().charged_bytes;
    let mut heap = Heap::new(
        SessionEpoch(1),
        HeapLimits {
            max_bytes: baseline + 64 * 1024,
            ..Default::default()
        },
    )
    .unwrap();
    for _ in 0..1000 {
        heap.alloc_cell(Value::NULL).unwrap();
        if heap.collection_due() {
            heap.begin_collection().unwrap();
            finish(&mut heap);
        }
    }
}
