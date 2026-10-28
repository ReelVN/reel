//! Shared-source subscriptions are retained across ordinary derived recomputation.
use super::*;

fn shared(heap: &mut Heap, count: usize) -> (Value, Vec<Value>) {
    let fields = heap.alloc_table().unwrap();
    let name = heap.alloc_string("value").unwrap();
    heap.table_store(fields, 0, name, Value::int(0)).unwrap();
    let source = heap.alloc_host(fields, &[], None).unwrap();
    heap.observe_host_properties(source, 0, 1).unwrap();
    let outputs = (0..count)
        .map(|_| {
            let output = heap.alloc_derived(Value::NULL).unwrap();
            heap.begin_derived(output).unwrap();
            heap.observe_read(source, Some(0)).unwrap();
            heap.commit_derived(output, Value::int(1)).unwrap();
            output
        })
        .collect();
    (source, outputs)
}

#[test]
#[ignore = "isolated shared-source dependency maintenance measurement"]
fn stable_subscription_cost() {
    for count in [16, 64, 256] {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let (source, outputs) = shared(&mut heap, count);
        let before = heap.property_traversal_work.get();
        let memory_before = heap.stats();
        let started = std::time::Instant::now();
        for _ in 0..16 {
            for &output in &outputs {
                heap.begin_derived(output).unwrap();
                heap.observe_read(source, Some(0)).unwrap();
                heap.commit_derived(output, Value::int(1)).unwrap();
            }
        }
        println!(
            "subscriptions,count={count},rounds=16,elapsed_ns={},traversal_work={},objects={},charged_bytes={},chunk_allocations={}",
            started.elapsed().as_nanos(),
            heap.property_traversal_work.get() - before,
            heap.stats().live_objects - memory_before.live_objects,
            heap.stats().charged_bytes - memory_before.charged_bytes,
            heap.stats().chunk_allocations - memory_before.chunk_allocations
        );
    }
}

#[test]
fn stable_captures_notify_once_and_replaced_inputs_disconnect_the_original_source() {
    let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
    let (source, outputs) = shared(&mut heap, 64);
    let before = heap.property_traversal_work.get();
    for &output in &outputs {
        heap.begin_derived(output).unwrap();
        heap.observe_read(source, Some(0)).unwrap();
        heap.commit_derived(output, Value::int(1)).unwrap();
    }
    // The stable comparison plus clearing two candidate edges stays linear.
    assert!(heap.property_traversal_work.get() - before <= 64 * 18);
    heap.publish_host_property(
        PropertyId {
            host: source,
            field: 0,
        },
        Value::int(1),
    )
    .unwrap();
    let mut notified = Vec::new();
    while let Some(output) = heap.next_derived().unwrap() {
        notified.push(output);
    }
    assert_eq!(notified.len(), outputs.len());
    assert!(outputs.iter().all(|output| {
        notified
            .iter()
            .filter(|value| value.same_bits(*output))
            .count()
            == 1
    }));
    let (replacement, _) = shared(&mut heap, 0);
    heap.begin_derived(outputs[0]).unwrap();
    heap.observe_read(replacement, Some(0)).unwrap();
    heap.commit_derived(outputs[0], Value::int(1)).unwrap();
    heap.publish_host_property(
        PropertyId {
            host: source,
            field: 0,
        },
        Value::int(2),
    )
    .unwrap();
    while let Some(output) = heap.next_derived().unwrap() {
        assert!(!output.same_bits(outputs[0]));
    }
    heap.publish_host_property(
        PropertyId {
            host: replacement,
            field: 0,
        },
        Value::int(2),
    )
    .unwrap();
    assert!(heap.next_derived().unwrap().unwrap().same_bits(outputs[0]));
    assert!(heap.next_derived().unwrap().is_none());
}

#[test]
fn small_candidates_reuse_identity_and_release_abandoned_strong_inputs() {
    let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
    let (source, outputs) = shared(&mut heap, 1);
    let output = outputs[0];
    heap.add_root(RootOwner(1), RootKind::Host, output).unwrap();
    heap.begin_derived(output).unwrap();
    heap.observe_read(source, Some(0)).unwrap();
    heap.commit_derived(output, Value::int(1)).unwrap();
    let before = heap.stats();
    for _ in 0..64 {
        heap.begin_derived(output).unwrap();
        heap.observe_read(source, Some(0)).unwrap();
        heap.commit_derived(output, Value::int(1)).unwrap();
    }
    assert_eq!(heap.stats().live_objects, before.live_objects);
    assert_eq!(heap.stats().charged_bytes, before.charged_bytes);
    let (abandoned, _) = shared(&mut heap, 0);
    heap.begin_derived(output).unwrap();
    heap.observe_read(abandoned, Some(0)).unwrap();
    heap.end_derived(output).unwrap();
    heap.begin_collection().unwrap();
    while heap.phase() != GcPhase::Idle {
        heap.gc_step(crate::GcBudget::default()).unwrap();
    }
    assert!(heap.info(source).is_ok());
    assert!(heap.info(abandoned).is_err());
    let scratch = heap
        .host_payload(output, DerivedSlot::Candidate as u32)
        .unwrap();
    assert_eq!(heap.sequence_len(scratch).unwrap(), 0);
}

#[test]
fn exhausted_large_index_admission_keeps_committed_dependencies_and_reusable_small_scratch() {
    let mut heap = Heap::new(
        SessionEpoch(1),
        HeapLimits {
            max_bytes: 1 << 20,
            ..HeapLimits::default()
        },
    )
    .unwrap();
    let fields = heap.alloc_table().unwrap();
    for field in 0..5 {
        heap.table_store(fields, field, Value::int(field.into()), Value::int(0))
            .unwrap();
    }
    let source = heap.alloc_host(fields, &[], None).unwrap();
    heap.observe_host_properties(source, 0, 5).unwrap();
    let output = heap.alloc_derived(Value::NULL).unwrap();
    heap.add_root(RootOwner(1), RootKind::Host, output).unwrap();
    for _ in 0..2 {
        heap.begin_derived(output).unwrap();
        heap.observe_read(source, Some(0)).unwrap();
        heap.commit_derived(output, Value::int(1)).unwrap();
    }
    let objects = heap.stats().live_objects;
    let reserved = heap
        .budget_usage()
        .reserve(heap.budget_usage().remaining_bytes())
        .unwrap();
    heap.begin_derived(output).unwrap();
    for field in 0..4 {
        heap.observe_read(source, Some(field)).unwrap();
    }
    assert!(matches!(
        heap.observe_read(source, Some(4)),
        Err(HeapError::ResourceLimit { .. })
    ));
    heap.end_derived(output).unwrap();
    drop(reserved);
    assert_eq!(heap.stats().live_objects, objects);
    let committed = heap
        .host_payload(output, DerivedSlot::Dependencies as u32)
        .unwrap();
    assert_eq!(heap.sequence_len(committed).unwrap(), 2);
    let scratch = heap
        .host_payload(output, DerivedSlot::Candidate as u32)
        .unwrap();
    assert_eq!(heap.sequence_len(scratch).unwrap(), 0);
}

#[test]
#[ignore = "release stable dependency-chain commit measurement"]
fn stable_derived_chain_commit_cost() {
    for depth in [32, 128, 256] {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let (source, _) = shared(&mut heap, 0);
        let mut upstream = source;
        let mut edges = Vec::new();
        for _ in 0..depth {
            let output = heap.alloc_derived(Value::NULL).unwrap();
            heap.begin_derived(output).unwrap();
            heap.observe_read(upstream, Some(0)).unwrap();
            heap.commit_derived(output, Value::int(1)).unwrap();
            edges.push((output, upstream));
            upstream = output;
        }
        let before = heap.stats();
        let start = std::time::Instant::now();
        for _ in 0..32 {
            for &(output, source) in &edges {
                heap.begin_derived(output).unwrap();
                heap.observe_read(source, Some(0)).unwrap();
                heap.commit_derived(output, Value::int(1)).unwrap();
            }
        }
        eprintln!(
            "stable_chain depth={depth} rounds=32 elapsed_us={} added_bytes={} added_chunks={}",
            start.elapsed().as_micros(),
            heap.stats().charged_bytes - before.charged_bytes,
            heap.stats().chunk_allocations - before.chunk_allocations
        );
    }
}
