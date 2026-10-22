use super::*;

fn selection(view: &PropertyRead<'_>, host: Value, _: u32, _: Value) -> Result<(), HeapError> {
    let length = view.read(PropertyId { host, field: 0 })?.as_int().unwrap();
    let caret = view.read(PropertyId { host, field: 1 })?.as_int().unwrap();
    if 0 <= caret && caret <= length {
        Ok(())
    } else {
        Err(HeapError::TypeMismatch)
    }
}

fn service(heap: &mut Heap) -> Value {
    let fields = heap.alloc_table().unwrap();
    for field in 0..2 {
        heap.table_store(fields, field, Value::int(field.into()), Value::int(0))
            .unwrap();
    }
    let host = heap.alloc_host(fields, &[], Some(selection)).unwrap();
    heap.observe_host_properties(host, 0, 2).unwrap();
    host
}

fn writes(host: Value, length: i64, caret: i64) -> [(PropertyId, Value); 2] {
    [
        (PropertyId { host, field: 0 }, Value::int(length)),
        (PropertyId { host, field: 1 }, Value::int(caret)),
    ]
}

fn values(heap: &Heap, host: Value) -> [i64; 2] {
    std::array::from_fn(|field| {
        heap.host_property(PropertyId {
            host,
            field: field as u32,
        })
        .unwrap()
        .as_int()
        .unwrap()
    })
}

fn prepare(
    heap: &mut Heap,
    plan: PropertyBatch,
    writes: &dyn PropertyWrites,
) -> Result<(u64, u64), HeapError> {
    let mut slices = 0;
    loop {
        slices += 1;
        assert!(slices < 100_000);
        let allowance = heap.property_batch_required_work(plan, writes)?.max(32);
        match heap.step_property_batch(plan, writes, allowance)? {
            PropertyBatchProgress::Preparing { work, .. } => assert!(work <= allowance),
            PropertyBatchProgress::Prepared {
                work,
                required_commit_work,
            } => {
                assert!(work <= allowance);
                return Ok((required_commit_work, slices));
            }
            PropertyBatchProgress::Committed { .. } => panic!("preparation published values"),
        }
    }
}

#[test]
fn publication_slices_readonly_fanout_and_commits_connected_values_together() {
    let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
    let source = service(&mut heap);
    let reference = heap
        .alloc_property_ref(
            PropertyId {
                host: source,
                field: 1,
            },
            false,
        )
        .unwrap();
    let mut followers = Vec::new();
    for _ in 0..256 {
        let follower = super::tests::host(&mut heap, 0);
        heap.bind_host_property(follower, reference).unwrap();
        followers.push(follower);
    }
    let update = writes(source, 12, 10);
    let plan = heap.begin_host_publication().unwrap();
    let (required, slices) = prepare(&mut heap, plan, &update).unwrap();
    assert!(slices > 256);
    assert_eq!(values(&heap, source), [0, 0]);
    assert!(matches!(
        heap.commit_property_batch(plan, &update, required - 1)
            .unwrap(),
        PropertyBatchProgress::Prepared { work: 0, .. }
    ));
    assert_eq!(values(&heap, source), [0, 0]);
    for &follower in &followers {
        assert_eq!(heap.host_property(follower).unwrap().as_int(), Some(0));
    }
    assert!(
        matches!(heap.commit_property_batch(plan, &update, required).unwrap(), PropertyBatchProgress::Committed { work } if work <= required)
    );
    assert_eq!(values(&heap, source), [12, 10]);
    for follower in followers {
        assert_eq!(heap.host_property(follower).unwrap().as_int(), Some(10));
    }
    eprintln!(
        "authoritative publication: followers=256 preparation_slices={slices} required_commit_work={required}"
    );
}

#[test]
fn publication_preserves_validation_and_binding_permissions_across_restarts() {
    let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
    let source = service(&mut heap);
    for script in [false, true] {
        let ordinary = heap.begin_property_batch(script).unwrap();
        assert_eq!(
            prepare(&mut heap, ordinary, &writes(source, 12, 10)),
            Err(HeapError::ReadOnly)
        );
    }
    let invalid = heap.begin_host_publication().unwrap();
    assert_eq!(
        prepare(&mut heap, invalid, &writes(source, 5, 9)),
        Err(HeapError::TypeMismatch)
    );
    assert_eq!(values(&heap, source), [0, 0]);

    let other = super::tests::host(&mut heap, 3);
    let reference = heap.alloc_property_ref(other, true).unwrap();
    let rebound = heap.begin_host_publication().unwrap();
    assert_eq!(
        prepare(
            &mut heap,
            rebound,
            &[(
                PropertyId {
                    host: source,
                    field: 0
                },
                reference
            )]
        ),
        Err(HeapError::ReadOnly)
    );
    let plan = heap.begin_host_publication().unwrap();
    let update = writes(source, 12, 10);
    let (required, _) = prepare(&mut heap, plan, &update).unwrap();
    heap.publish_host_properties(&writes(source, 2, 1)).unwrap();
    assert!(matches!(
        heap.commit_property_batch(plan, &update, required).unwrap(),
        PropertyBatchProgress::Preparing { .. }
    ));
    assert_eq!(values(&heap, source), [2, 1]);
    let (required, _) = prepare(&mut heap, plan, &update).unwrap();
    heap.commit_property_batch(plan, &update, required).unwrap();
    assert_eq!(values(&heap, source), [12, 10]);
}

#[test]
fn publication_permission_and_original_inputs_survive_gc_and_snapshot() {
    use crate::heap::snapshot as graph;
    use crate::snapshot::{SnapshotMap, SnapshotRegistry, VmSnapshot};
    let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
    let limits = crate::FrameSlotsLimits {
        initial_capacity: 8,
        max_capacity: 128,
        max_window_len: 64,
    };
    let slots = crate::FrameSlots::new(&mut heap, limits).unwrap();
    let source = service(&mut heap);
    let update = writes(source, 12, 10);
    let plan = heap.begin_host_publication().unwrap();
    let roots = heap.alloc_array(&[source, plan.value()]).unwrap();
    heap.add_root(RootOwner(1), RootKind::Host, roots).unwrap();
    for _ in 0..9 {
        let allowance = heap
            .property_batch_required_work(plan, &update)
            .unwrap()
            .max(32);
        heap.step_property_batch(plan, &update, allowance).unwrap();
        if heap.phase() == GcPhase::Idle {
            heap.begin_collection().unwrap();
        }
        heap.gc_step(crate::GcBudget {
            units: 1,
            ..Default::default()
        })
        .unwrap();
    }
    while heap.phase() != GcPhase::Idle {
        heap.gc_step(crate::GcBudget::default()).unwrap();
    }
    let mut registry = SnapshotRegistry::default();
    registry
        .register_validator("test.selection", 1, selection)
        .unwrap();
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
    let source = map.value(source).unwrap();
    let plan = PropertyBatch::from_value(map.value(plan.value()).unwrap());
    let update = writes(source, 12, 10);
    assert_eq!(values(&restored, source), [0, 0]);
    let (required, _) = prepare(&mut restored, plan, &update).unwrap();
    restored
        .commit_property_batch(plan, &update, required)
        .unwrap();
    assert_eq!(values(&restored, source), [12, 10]);
}
