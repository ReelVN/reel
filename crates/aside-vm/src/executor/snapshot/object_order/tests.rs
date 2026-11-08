use super::*;
use crate::{
    GcBudget, GcPhase, HeapLimits, RootKind, RootOwner, SessionEpoch,
    bytecode::{Limits, fixtures, verify},
    executor::{Vm, VmLimits},
    snapshot::{SnapshotBudget, SnapshotRegistry},
};

#[test]
fn identity_order_charges_and_releases_its_complete_temporary_capacity() {
    let heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
    let before = heap.budget_usage().charged_bytes();
    let records: Vec<_> = [0x3_0000_0001, 0x1_0000_0009, 0x1_0000_0003]
        .into_iter()
        .map(|key| Record::new(RecordKind::Object, key, 0, &()).unwrap())
        .collect();
    let mut order = ObjectOrder::default();
    let mut actual = Vec::new();
    loop {
        match order.step(&heap, &records).unwrap() {
            Step::Allocate(index) => {
                assert_eq!(
                    heap.budget_usage().charged_bytes(),
                    before + (order.entries.capacity() * size_of::<Reverse<(u64, usize)>>()) as u64
                );
                actual.push(records[index].key);
            }
            Step::Done => break,
            Step::Work => (),
        }
    }
    assert_eq!(actual, [0x1_0000_0003, 0x1_0000_0009, 0x3_0000_0001]);
    assert_eq!(heap.budget_usage().charged_bytes(), before);
}

#[test]
fn identity_order_rejects_temporary_storage_outside_the_heap_budget() {
    let heap = Heap::new(
        SessionEpoch(1),
        HeapLimits {
            max_bytes: 16 << 10,
            ..Default::default()
        },
    )
    .unwrap();
    let records: Vec<_> = (0..256)
        .map(|key| Record::new(RecordKind::Object, key, 0, &()).unwrap())
        .collect();
    let before = heap.budget_usage().charged_bytes();
    let mut order = ObjectOrder::default();
    for _ in 0..records.len() {
        assert!(matches!(order.step(&heap, &records).unwrap(), Step::Work));
    }
    assert!(matches!(
        order.step(&heap, &records),
        Err(SnapshotError::Heap(HeapError::ResourceLimit { .. }))
    ));
    assert_eq!(order.entries.capacity(), 0);
    assert_eq!(heap.budget_usage().charged_bytes(), before);
}

#[test]
fn restoration_preserves_handle_order_across_slot_reuse_and_single_unit_steps() {
    for additional in [0, 4096] {
        let program = verify(fixtures::helper_native(), Limits::default()).unwrap();
        let support = crate::executor::review_tests::echo_registry();
        let mut vm = Vm::load(
            program.clone(),
            support,
            VmLimits::default(),
            SessionEpoch(1),
        )
        .unwrap();
        let dead = vm.heap_mut().alloc_table().unwrap();
        let live = vm.heap_mut().alloc_table().unwrap();
        vm.heap_mut()
            .add_root(RootOwner(7), RootKind::Host, live)
            .unwrap();
        vm.begin_collection().unwrap();
        while vm.heap().phase() != GcPhase::Idle {
            vm.gc_step(GcBudget::default()).unwrap();
        }
        assert!(vm.heap().handle(dead).is_err());
        let reused = vm.heap_mut().alloc_table().unwrap();
        vm.heap_mut()
            .add_root(RootOwner(7), RootKind::Host, reused)
            .unwrap();
        let fresh = vm.heap_mut().alloc_table().unwrap();
        vm.heap_mut()
            .add_root(RootOwner(7), RootKind::Host, fresh)
            .unwrap();
        assert!(
            vm.heap().key(reused).unwrap() < vm.heap().key(live).unwrap(),
            "slot order is the opposite of generation-first handle order"
        );
        assert!(vm.heap().handle(reused).unwrap().key > vm.heap().handle(fresh).unwrap().key);
        let mut identities = vec![live, fresh];
        for _ in 0..additional {
            identities.push(vm.heap_mut().alloc_table().unwrap());
        }
        identities.push(reused);
        let index = vm.heap_mut().alloc_array(&[]).unwrap();
        for &identity in &identities {
            vm.heap_mut().array_push(index, identity).unwrap();
        }
        let root = vm
            .heap_mut()
            .add_root(RootOwner(7), RootKind::Host, index)
            .unwrap();
        let registry = SnapshotRegistry::default();
        let mut capture = vm.snapshot_begin(&registry).unwrap();
        while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let image = capture.finish().unwrap();
        let mut restore = Vm::restore_begin(
            &image,
            program,
            crate::executor::review_tests::echo_registry(),
            VmLimits::default(),
            SessionEpoch(2),
            &registry,
        )
        .unwrap();
        let zero = restore.step(SnapshotBudget { units: 0 }).unwrap();
        assert_eq!(zero.units, 0);
        assert!(!zero.complete);
        let mut maximum = std::time::Duration::ZERO;
        let mut units = 0;
        loop {
            let start = std::time::Instant::now();
            let progress = restore.step(SnapshotBudget { units: 1 }).unwrap();
            maximum = maximum.max(start.elapsed());
            assert!(progress.units <= 1);
            units += progress.units;
            if progress.complete {
                break;
            }
        }
        let (restored, map) = restore.finish().unwrap();
        let index = restored.heap().root_value(map.root(root).unwrap()).unwrap();
        let handles: Vec<_> = (0..restored.heap().sequence_len(index).unwrap())
            .map(|i| {
                restored
                    .heap()
                    .handle(restored.heap().sequence_get(index, i).unwrap())
                    .unwrap()
                    .key
            })
            .collect();
        assert!(handles.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(
            restored
                .heap()
                .sequence_get(index, 0)
                .unwrap()
                .same_bits(map.value(live).unwrap())
        );
        eprintln!(
            "identity-restore objects={} units={units} max_single_unit_us={:.3}",
            identities.len() + 1,
            maximum.as_secs_f64() * 1e6
        );
    }
}
