use super::*;
use crate::GcBudget;
fn heap(limit: u32) -> Heap {
    Heap::new(
        SessionEpoch(1),
        HeapLimits {
            max_mutation_values: limit,
            ..Default::default()
        },
    )
    .unwrap()
}
#[test]
fn prefix_updates_cross_storage_blocks_and_charge_only_actual_growth() {
    let mut heap = heap(7);
    let target = heap.alloc_array(&[]).unwrap();
    let data: Vec<_> = (0..161).map(Value::int).collect();
    let before = heap.stats().payload_bytes;
    let mut written = 0;
    while written < data.len() {
        let count = heap
            .array_write(target, written as u32, &data[written..])
            .unwrap();
        assert!((1..=7).contains(&count));
        written += count;
    }
    assert_eq!(
        heap.stats().payload_bytes - before,
        Values::allocation_bytes(data.len() as u64)
    );
    for (index, value) in data.iter().enumerate() {
        assert!(
            heap.sequence_get(target, index as u32)
                .unwrap()
                .same_bits(*value)
        );
    }
    let old = heap.info(target).unwrap();
    let before = heap.stats().payload_bytes;
    assert_eq!(
        heap.array_write(target, 29, &[Value::int(99); 7]).unwrap(),
        7
    );
    assert_eq!(heap.stats().payload_bytes, before);
    assert_eq!(
        heap.info(target).unwrap().structure_version,
        old.structure_version
    );
    assert_eq!(
        heap.info(target).unwrap().content_version,
        old.content_version + 1
    );
    for offset in [0, 7, 8, 31, 32, 95, 96, 160, 161] {
        let segment = heap.array_segment(target, offset).unwrap();
        assert!(segment.len() <= 7);
        for (i, value) in segment.iter().enumerate() {
            assert!(
                heap.sequence_get(target, offset + i as u32)
                    .unwrap()
                    .same_bits(*value)
            );
        }
    }
    let version = heap.info(target).unwrap().content_version;
    assert_eq!(heap.array_write(target, 161, &[]).unwrap(), 0);
    assert_eq!(heap.info(target).unwrap().content_version, version);
}
#[test]
fn validation_and_growth_failure_publish_no_partial_prefix() {
    let mut heap = heap(8);
    let target = heap.alloc_array(&[Value::int(0); 8]).unwrap();
    let mut other = super::tests::heap(8);
    let foreign = other.alloc_array(&[]).unwrap();
    let version = heap.info(target).unwrap().content_version;
    assert_eq!(
        heap.array_write(target, 0, &[Value::int(4), foreign]),
        Err(HeapError::WrongHeap)
    );
    assert_eq!(heap.info(target).unwrap().content_version, version);
    assert_eq!(heap.sequence_get(target, 0).unwrap().as_int(), Some(0));
    assert_eq!(
        heap.array_write(target, 9, &[Value::NULL]),
        Err(HeapError::OutOfBounds)
    );
    let used = heap.budget_usage().charged_bytes();
    let extra = Values::growth_bytes(8, 1);
    let reservation = heap
        .budget_usage()
        .reserve(heap.budget_usage().limit() - used - extra + 1)
        .unwrap();
    let reserved = heap.budget_usage().charged_bytes();
    assert!(heap.array_write(target, 8, &[Value::int(9)]).is_err());
    assert_eq!(heap.info(target).unwrap().len, 8);
    assert_eq!(heap.budget_usage().charged_bytes(), reserved);
    drop(reservation);
    assert_eq!(heap.budget_usage().charged_bytes(), used);
    heap.observe_readonly_collection(target).unwrap();
    assert_eq!(
        heap.array_write(target, 0, &[Value::NULL]),
        Err(HeapError::WrongKind)
    );
}
#[test]
fn copied_object_is_shaded_when_destination_has_already_been_scanned() {
    let mut heap = heap(8);
    let target = heap.alloc_array(&[Value::NULL; 4]).unwrap();
    let child = heap.alloc_array(&[Value::int(9)]).unwrap();
    let handle = heap.handle(child).unwrap();
    heap.add_root(RootOwner(1), RootKind::Work, target).unwrap();
    heap.begin_collection().unwrap();
    let key = heap.key(target).unwrap();
    for _ in 0..1000 {
        if heap.objects.get(key).unwrap().scan == 4 {
            break;
        }
        heap.gc_step(GcBudget {
            units: 1,
            ..Default::default()
        })
        .unwrap();
    }
    assert_eq!(heap.phase(), GcPhase::Mark);
    assert_eq!(heap.objects.get(key).unwrap().scan, 4);
    heap.array_write(target, 0, &[child]).unwrap();
    while heap.phase() != GcPhase::Idle {
        heap.gc_step(GcBudget {
            units: 1,
            ..Default::default()
        })
        .unwrap();
    }
    assert!(heap.import_handle(handle).unwrap().same_bits(child));
    assert!(heap.sequence_get(target, 0).unwrap().same_bits(child));
}
