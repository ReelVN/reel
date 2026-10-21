use super::*;

fn model(heap: &mut Heap) -> Value {
    let value = heap.alloc_table().unwrap();
    for index in 0..2 {
        heap.table_store(value, index, Value::int(index.into()), Value::int(0))
            .unwrap();
    }
    heap.observe_model_object(value).unwrap();
    value
}

fn derived(heap: &mut Heap, source: Value, field: u32) -> Value {
    let output = heap.alloc_derived(Value::NULL).unwrap();
    heap.begin_derived(output).unwrap();
    heap.observe_read(source, Some(field)).unwrap();
    heap.commit_derived(output, Value::int(0)).unwrap();
    output
}

fn inputs(heap: &Heap, output: Value) -> &crate::dependencies::Dependencies {
    let dependencies = heap
        .host_payload(output, DerivedSlot::Dependencies as u32)
        .unwrap();
    heap.dependency_payload(heap.key(dependencies).unwrap())
        .unwrap()
}

#[test]
fn semantic_read_accounts_first_dependency_scan_and_reuses_the_stable_index() {
    for count in [1, 128] {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let source = heap.alloc_table().unwrap();
        for field in 0..count {
            heap.table_store(source, field, Value::int(field.into()), Value::int(0))
                .unwrap();
        }
        heap.observe_model_object(source).unwrap();
        let upstream = heap.alloc_derived(Value::NULL).unwrap();
        heap.begin_derived(upstream).unwrap();
        for field in 0..count {
            heap.observe_read(source, Some(field)).unwrap();
        }
        heap.commit_derived(upstream, Value::int(0)).unwrap();
        let consumer = heap.alloc_derived(Value::NULL).unwrap();
        heap.begin_derived(consumer).unwrap();
        let property = PropertyId {
            host: upstream,
            field: 0,
        };
        let (refresh, first_work) = heap.reactive_read_source(property).unwrap();
        assert!(refresh.is_none());
        let before = heap.stats().charged_bytes;
        let (refresh, stable_work) = heap.reactive_read_source(property).unwrap();
        assert!(refresh.is_none());
        assert_eq!(first_work - stable_work, u64::from(count));
        assert_eq!(stable_work, 4);
        assert_eq!(heap.stats().charged_bytes, before);
        println!(
            "semantic-read model-fields={count} first-work={first_work} stable-work={stable_work} stable-allocation=0"
        );
        heap.end_derived(consumer).unwrap();
    }
}

#[test]
fn freshness_equal_value_binding_and_alias_rebinding_invalidate_cached_plain_endpoints() {
    let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
    let source = model(&mut heap);
    let alias = model(&mut heap);
    let upstream = derived(&mut heap, source, 1);
    let output = derived(&mut heap, alias, 0);
    let target = PropertyId {
        host: alias,
        field: 0,
    };
    let source_field = PropertyId {
        host: source,
        field: 0,
    };
    let reference = heap.alloc_property_ref(source_field, true).unwrap();
    heap.bind_host_property(target, reference).unwrap();
    assert!(heap.derived_is_current(output, None).unwrap());
    assert!(inputs(&heap, output).ancestors.is_empty());
    let previous_revision = inputs(&heap, output).ancestor_revision;
    let reference = heap
        .alloc_property_ref(
            PropertyId {
                host: upstream,
                field: 0,
            },
            false,
        )
        .unwrap();
    heap.invalidate_reactive(source, Some(1)).unwrap();
    heap.bind_host_property(source_field, reference).unwrap();
    assert!(
        !heap
            .object(heap.key(output).unwrap())
            .unwrap()
            .derived_dirty
    );
    assert!(heap.binding_revision > previous_revision);
    assert!(!heap.derived_is_current(output, None).unwrap());
    assert_eq!(inputs(&heap, output).ancestors.len(), 1);
    let reference = heap.alloc_property_ref(source_field, true).unwrap();
    heap.unbind_property(reference).unwrap();
    assert!(heap.derived_is_current(output, None).unwrap());
    assert!(inputs(&heap, output).ancestors.is_empty());
    let revision = heap.binding_revision;
    heap.invalidate_reactive(source, Some(1)).unwrap();
    assert_eq!(heap.binding_revision, revision);
    assert!(heap.derived_is_current(output, None).unwrap());
    heap.invalidate_reactive(alias, Some(0)).unwrap();
    assert!(!heap.derived_is_current(output, None).unwrap());
}

#[test]
fn freshness_conditional_dependencies_and_cancelled_capture_keep_only_committed_inputs() {
    let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
    let source = model(&mut heap);
    let upstream = derived(&mut heap, source, 1);
    let output = derived(&mut heap, upstream, 0);
    assert!(heap.derived_is_current(output, None).unwrap());
    assert_eq!(inputs(&heap, output).ancestors.len(), 1);
    let original = heap
        .host_payload(output, DerivedSlot::Dependencies as u32)
        .unwrap();
    heap.begin_derived(output).unwrap();
    heap.observe_read(source, Some(0)).unwrap();
    assert!(!heap.derived_is_current(output, None).unwrap());
    heap.end_derived(output).unwrap();
    assert!(
        heap.host_payload(output, DerivedSlot::Dependencies as u32)
            .unwrap()
            .same_bits(original)
    );
    assert!(heap.derived_is_current(output, None).unwrap());
    heap.begin_derived(output).unwrap();
    heap.observe_read(source, Some(0)).unwrap();
    heap.commit_derived(output, Value::int(0)).unwrap();
    assert_eq!(inputs(&heap, output).ancestor_revision, 0);
    assert!(heap.derived_is_current(output, None).unwrap());
    assert!(inputs(&heap, output).ancestors.is_empty());
    heap.invalidate_reactive(source, Some(1)).unwrap();
    assert!(heap.derived_is_current(output, None).unwrap());
    heap.invalidate_reactive(source, Some(0)).unwrap();
    assert!(!heap.derived_is_current(output, None).unwrap());
}

#[test]
fn freshness_cache_admission_failure_retries_and_gc_releases_capacity_within_budget() {
    let mut heap = Heap::new(
        SessionEpoch(1),
        HeapLimits {
            max_bytes: 1_000_000,
            ..Default::default()
        },
    )
    .unwrap();
    let source = model(&mut heap);
    let mut ancestors = Vec::new();
    for _ in 0..33 {
        ancestors.push(derived(&mut heap, source, 0));
    }
    let output = heap.alloc_derived(Value::NULL).unwrap();
    let root = heap.add_root(RootOwner(1), RootKind::Host, output).unwrap();
    heap.begin_derived(output).unwrap();
    for ancestor in ancestors {
        heap.observe_read(ancestor, Some(0)).unwrap();
    }
    heap.commit_derived(output, Value::int(0)).unwrap();
    let reserved = heap
        .budget_usage()
        .reserve(heap.budget_usage().remaining_bytes() - 63)
        .unwrap();
    assert!(matches!(
        heap.derived_is_current(output, None),
        Err(HeapError::ResourceLimit { requested: 64, .. })
    ));
    assert_eq!(inputs(&heap, output).ancestor_revision, 0);
    assert_eq!(inputs(&heap, output).ancestors.len(), 8);
    drop(reserved);
    let before = heap.stats().charged_bytes;
    assert!(heap.derived_is_current(output, None).unwrap());
    assert_eq!(inputs(&heap, output).ancestors.len(), 33);
    assert_eq!(inputs(&heap, output).ancestor_reserved, 64 * 4);
    assert!(heap.stats().charged_bytes >= before + 64 * 4);
    let dependencies = heap
        .host_payload(output, DerivedSlot::Dependencies as u32)
        .unwrap();
    heap.begin_collection().unwrap();
    while heap.phase() != GcPhase::Idle {
        heap.gc_step(crate::GcBudget::default()).unwrap();
    }
    assert!(heap.derived_is_current(output, None).unwrap());
    heap.release_root(root).unwrap();
    heap.begin_collection().unwrap();
    let mut released_before_refund = false;
    while heap.phase() != GcPhase::Idle {
        let progress = heap
            .gc_step(crate::GcBudget {
                units: 1,
                ..Default::default()
            })
            .unwrap();
        assert!(progress.units <= 1);
        if let Some(object) = heap.objects.get(heap.key(dependencies).unwrap())
            && let Payload::Dependencies(payload) = &object.payload
        {
            released_before_refund |=
                payload.ancestors.capacity() == 0 && payload.ancestor_reserved != 0;
        }
    }
    assert!(released_before_refund);
    assert_eq!(heap.stats().live_objects, 0);
    assert_eq!(heap.stats().payload_bytes, 0);
}

#[test]
fn freshness_snapshot_rebuilds_cached_aliases_from_remapped_sources() {
    use crate::heap::snapshot as graph;
    use crate::snapshot::{SnapshotMap, SnapshotRegistry, VmSnapshot};
    let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
    let slots = crate::FrameSlots::new(
        &mut heap,
        crate::FrameSlotsLimits {
            initial_capacity: 8,
            max_capacity: 128,
            max_window_len: 64,
        },
    )
    .unwrap();
    let source = model(&mut heap);
    let upstream = derived(&mut heap, source, 1);
    let reference = heap
        .alloc_property_ref(
            PropertyId {
                host: upstream,
                field: 0,
            },
            false,
        )
        .unwrap();
    heap.bind_host_property(
        PropertyId {
            host: source,
            field: 0,
        },
        reference,
    )
    .unwrap();
    let output = derived(&mut heap, source, 0);
    heap.add_root(RootOwner(1), RootKind::Host, output).unwrap();
    assert!(heap.derived_is_current(output, None).unwrap());
    assert_eq!(inputs(&heap, output).ancestors.len(), 1);
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
    let mut restored_slots = crate::FrameSlots::new(
        &mut restored,
        crate::FrameSlotsLimits {
            initial_capacity: 8,
            max_capacity: 128,
            max_window_len: 64,
        },
    )
    .unwrap();
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
    let source2 = map.value(source).unwrap();
    assert_ne!(
        source.object_parts().unwrap().1,
        source2.object_parts().unwrap().1
    );
    let output = map.value(output).unwrap();
    assert_eq!(inputs(&restored, output).ancestor_revision, 0);
    assert_eq!(inputs(&restored, output).ancestors.capacity(), 0);
    assert!(restored.derived_is_current(output, None).unwrap());
    assert_eq!(inputs(&restored, output).ancestors.len(), 1);
    restored.invalidate_reactive(source2, Some(1)).unwrap();
    assert!(!restored.derived_is_current(output, None).unwrap());
}
