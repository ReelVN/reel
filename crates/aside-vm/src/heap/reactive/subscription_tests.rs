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
    assert!(heap.property_traversal_work.get() - before <= 64 * 16);
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
