use super::*;

fn table(heap: &mut Heap) -> Value {
    let value = heap.alloc_table().unwrap();
    heap.table_store(value, 0, Value::int(0), Value::int(1))
        .unwrap();
    value
}

fn finish(heap: &mut Heap, plan: Value) -> (u64, u64) {
    let mut work = 0;
    for _ in 0..100_000 {
        let before = heap.model_adoption_work(plan).unwrap();
        let required = heap.model_adoption_required_work(plan).unwrap();
        match heap.step_model_adoption(plan, ITEM_WORK).unwrap() {
            ModelAdoptionProgress::Preparing {
                work: actual,
                required_work,
            } => {
                assert!(actual <= ITEM_WORK);
                assert_eq!(required_work, ITEM_WORK);
                assert_eq!(required, ITEM_WORK);
                assert_eq!(heap.model_adoption_work(plan).unwrap() - before, actual);
                work += actual;
            }
            ModelAdoptionProgress::Complete {
                work: actual,
                commit_work,
            } => {
                assert!(actual <= ITEM_WORK);
                assert_eq!(heap.model_adoption_work(plan).unwrap() - before, actual);
                assert_eq!(heap.model_adoption_required_work(plan).unwrap(), 0);
                return (work + actual, commit_work);
            }
        }
    }
    panic!("model adoption did not finish");
}

fn proofs_valid(heap: &Heap, plan: Value) -> bool {
    (0..heap.model_adoption_versions_len(plan).unwrap())
        .all(|index| heap.model_adoption_version_valid(plan, index).unwrap())
}

#[test]
fn adoption_scans_each_shared_data_identity_and_publishes_without_walking_edges() {
    let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
    let leaf = table(&mut heap);
    let items = heap.alloc_array(&[leaf, leaf]).unwrap();
    let root = table(&mut heap);
    heap.table_store(root, 0, Value::int(0), items).unwrap();
    heap.array_push(items, root).unwrap();
    let cell = heap
        .alloc_language_cell(crate::CellContract::untyped_mutable())
        .unwrap();
    heap.cell_initialize(cell, leaf).unwrap();
    heap.array_push(items, cell).unwrap();
    let host_fields = table(&mut heap);
    let host = heap.alloc_host(host_fields, &[], None).unwrap();
    heap.array_push(items, host).unwrap();
    let plan = heap.begin_model_adoption(root).unwrap();
    assert!(matches!(
        heap.step_model_adoption(plan, ITEM_WORK - 1).unwrap(),
        ModelAdoptionProgress::Preparing {
            work: 0,
            required_work: ITEM_WORK
        }
    ));
    assert_eq!(heap.model_adoption_work(plan).unwrap(), 0);
    assert!(matches!(
        heap.publish_model_adoption(plan),
        Err(HeapError::WrongKind)
    ));
    let (work, commit_work) = finish(&mut heap, plan);
    assert!(work > ITEM_WORK * 3);
    assert_eq!(heap.model_adoption_versions_len(plan).unwrap(), 4);
    assert!(proofs_valid(&heap, plan));
    for value in [root, items, leaf, cell, host_fields, host] {
        assert!(!heap.object(heap.key(value).unwrap()).unwrap().model);
    }
    let before = heap.stats();
    let traversal = heap.property_traversal_work.get();
    heap.publish_model_adoption(plan).unwrap();
    assert_eq!(heap.property_traversal_work.get() - traversal, commit_work);
    assert_eq!(heap.stats().charged_bytes, before.charged_bytes);
    assert_eq!(heap.stats().chunk_allocations, before.chunk_allocations);
    for value in [root, items, leaf, cell] {
        assert!(heap.object(heap.key(value).unwrap()).unwrap().model);
    }
    for value in [host_fields, host] {
        assert!(!heap.object(heap.key(value).unwrap()).unwrap().model);
    }
}

#[test]
fn adoption_proofs_detect_raw_graph_changes_and_accept_already_observed_graphs() {
    let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
    let leaf = table(&mut heap);
    let root = heap.alloc_array(&[leaf]).unwrap();
    let plan = heap.begin_model_adoption(root).unwrap();
    finish(&mut heap, plan);
    assert!(proofs_valid(&heap, plan));
    heap.table_store(leaf, 0, Value::int(0), Value::int(2))
        .unwrap();
    assert!(!proofs_valid(&heap, plan));
    assert!(!heap.is_model_table(leaf).unwrap());
    let owner = table(&mut heap);
    heap.observe_model_object(owner).unwrap();
    heap.adopt_model_value(owner, root).unwrap();
    let later = table(&mut heap);
    heap.array_push(root, later).unwrap();
    assert!(heap.is_model_table(later).unwrap());
    assert!(proofs_valid(&heap, plan));
    heap.publish_model_adoption(plan).unwrap();
}

#[test]
fn abandoning_an_adoption_releases_its_roots_without_observing_user_data() {
    let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
    let leaf = table(&mut heap);
    let source = heap.alloc_array(&[leaf]).unwrap();
    heap.add_root(RootOwner(1), RootKind::Host, source).unwrap();
    let before = heap.stats().live_objects;
    let plan = heap.begin_model_adoption(source).unwrap();
    let root = heap.add_root(RootOwner(1), RootKind::Host, plan).unwrap();
    finish(&mut heap, plan);
    heap.release_root(root).unwrap();
    heap.begin_collection().unwrap();
    while heap.phase() != crate::GcPhase::Idle {
        heap.gc_step(crate::GcBudget::default()).unwrap();
    }
    assert_eq!(heap.stats().live_objects, before);
    assert!(!heap.object(heap.key(source).unwrap()).unwrap().model);
    assert!(!heap.is_model_table(leaf).unwrap());
}

#[test]
fn adoption_cursor_keeps_pending_edges_and_identity_index_across_gc_and_snapshot() {
    use crate::heap::snapshot as graph;
    use crate::snapshot::{SnapshotMap, SnapshotRegistry, VmSnapshot};
    let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
    let limits = crate::FrameSlotsLimits {
        initial_capacity: 8,
        max_capacity: 128,
        max_window_len: 64,
    };
    let slots = crate::FrameSlots::new(&mut heap, limits).unwrap();
    let mut source = table(&mut heap);
    for _ in 0..32 {
        source = heap.alloc_array(&[source, source]).unwrap();
    }
    let plan = heap.begin_model_adoption(source).unwrap();
    heap.add_root(RootOwner(1), RootKind::Host, plan).unwrap();
    for _ in 0..9 {
        heap.step_model_adoption(plan, ITEM_WORK).unwrap();
        if heap.phase() == crate::GcPhase::Idle {
            heap.begin_collection().unwrap();
        }
        heap.gc_step(crate::GcBudget {
            units: 1,
            ..Default::default()
        })
        .unwrap();
    }
    while heap.phase() != crate::GcPhase::Idle {
        heap.gc_step(crate::GcBudget::default()).unwrap();
    }
    let registry = SnapshotRegistry::default();
    let mut image = VmSnapshot {
        version: VmSnapshot::FORMAT_VERSION,
        code: [0; 32],
        source_heap: heap.id().get(),
        source_epoch: 1,
        records: Vec::new(),
        bytes: 0,
    };
    let mut capture = graph::GraphCapture::new(&heap).unwrap();
    while !capture.step(&heap, &slots, &registry, &mut image).unwrap() {}
    let image = VmSnapshot::decode(&image.encode().unwrap()).unwrap();
    let mut restored = Heap::new(SessionEpoch(2), HeapLimits::default()).unwrap();
    for _ in 0..7 {
        restored.alloc_array(&[]).unwrap();
    }
    let mut restored_slots = crate::FrameSlots::new(&mut restored, limits).unwrap();
    let mut map = SnapshotMap::new(&restored, &image);
    for record in &image.records {
        graph::allocate(
            &mut restored,
            &mut restored_slots,
            &mut map,
            record,
            &registry,
        )
        .unwrap();
    }
    for record in &image.records {
        graph::connect(&mut restored, &mut restored_slots, &mut map, record).unwrap();
    }
    for record in &image.records {
        graph::validate(&restored, &map, record).unwrap();
    }
    let plan2 = map.value(plan).unwrap();
    let source2 = map.value(source).unwrap();
    assert_ne!(
        plan.object_parts().unwrap().1,
        plan2.object_parts().unwrap().1
    );
    let previous = restored.model_adoption_work(plan2).unwrap();
    let (remaining, _) = finish(&mut restored, plan2);
    assert_eq!(
        restored.model_adoption_work(plan2).unwrap(),
        previous + remaining
    );
    assert_eq!(restored.model_adoption_versions_len(plan2).unwrap(), 33);
    assert!(proofs_valid(&restored, plan2));
    assert!(
        !restored
            .object(restored.key(source2).unwrap())
            .unwrap()
            .model
    );
    restored.publish_model_adoption(plan2).unwrap();
    assert!(
        restored
            .object(restored.key(source2).unwrap())
            .unwrap()
            .model
    );
}
