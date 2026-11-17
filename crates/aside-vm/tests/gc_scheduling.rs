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
fn array_scan_keeps_references_after_scalar_runs_and_stays_within_budget() {
    let mut heap = heap();
    let child = heap.alloc_cell(Value::int(42)).unwrap();
    let mut values = [Value::int(0); 150];
    values[79] = child;
    let array = heap.alloc_array(&values).unwrap();
    let root = heap.add_root(RootOwner(1), RootKind::Host, array).unwrap();
    let budget = GcBudget {
        edges: 6,
        units: 7,
        ..Default::default()
    };
    let collect = |heap: &mut Heap| {
        heap.begin_collection().unwrap();
        loop {
            let progress = heap.gc_step(budget).unwrap();
            assert!(progress.edges <= budget.edges);
            assert!(progress.units <= budget.units);
            if progress.cycle_complete {
                break;
            }
            assert!(progress.units != 0);
        }
    };
    collect(&mut heap);
    assert_eq!(heap.cell_read(child).unwrap().as_int(), Some(42));
    heap.release_root(root).unwrap();
    collect(&mut heap);
    assert_eq!(heap.stats().live_objects, 0);
    assert_eq!(heap.stats().payload_bytes, 0);
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

#[test]
fn weak_work_tracks_references_and_retires_cleared_entries_without_gc_allocation() {
    let mut heap = heap();
    let ordinary = heap.alloc_array(&[]).unwrap();
    heap.add_root(RootOwner(1), RootKind::Host, ordinary)
        .unwrap();
    for _ in 0..10_000 {
        let cell = heap.alloc_cell(Value::NULL).unwrap();
        heap.array_push(ordinary, cell).unwrap();
    }
    let target = heap.alloc_cell(Value::int(42)).unwrap();
    let target_root = heap.add_root(RootOwner(1), RootKind::Host, target).unwrap();
    let live = heap.alloc_weakref(target).unwrap();
    heap.add_root(RootOwner(1), RootKind::Host, live).unwrap();
    let dead_target = heap.alloc_cell(Value::NULL).unwrap();
    let cleared = heap.alloc_weakref(dead_target).unwrap();
    heap.add_root(RootOwner(1), RootKind::Host, cleared)
        .unwrap();
    heap.alloc_weakref(target).unwrap();

    heap.begin_collection().unwrap();
    while heap.phase() == GcPhase::Mark {
        heap.gc_step(GcBudget {
            weak: 0,
            ..Default::default()
        })
        .unwrap();
    }
    assert_eq!(heap.phase(), GcPhase::Weak);
    // A reference published after the cutoff belongs to the next cycle.
    let late = heap.alloc_weakref(target).unwrap();
    heap.add_root(RootOwner(1), RootKind::Host, late).unwrap();
    let finish_count = |heap: &mut Heap| {
        let mut weak = 0;
        let allocations = allocation_counter::measure(|| {
            while heap.phase() != GcPhase::Idle {
                let progress = heap.gc_step(GcBudget::default()).unwrap();
                weak += progress.weak;
                assert!(progress.units <= 256 && progress.bytes <= 16_384);
            }
        });
        assert_eq!(allocations.count_total, 0);
        weak
    };
    assert_eq!(finish_count(&mut heap), 3);
    assert!(heap.upgrade_weakref(cleared).unwrap().is_none());
    assert!(heap.upgrade_weakref(late).unwrap().is_some());
    heap.begin_collection().unwrap();
    assert_eq!(finish_count(&mut heap), 2);

    heap.release_root(target_root).unwrap();
    heap.begin_collection().unwrap();
    assert_eq!(finish_count(&mut heap), 2);
    assert!(heap.upgrade_weakref(live).unwrap().is_none());
    assert!(heap.upgrade_weakref(late).unwrap().is_none());
    heap.begin_collection().unwrap();
    assert_eq!(finish_count(&mut heap), 0);
    assert_eq!(heap.stats().live_objects, 10_004);
}
