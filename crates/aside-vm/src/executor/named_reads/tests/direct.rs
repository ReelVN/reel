//! The ordinary cursor is the oracle for complete and failed direct reads.
use super::*;

#[test]
fn direct_reads_use_current_host_and_table_values_and_restore_after_commit() {
    for host in [false, true] {
        for model in [false, true] {
            for reference in [false, true] {
                let (mut vm, target, table) = load(host, model, false, true, false);
                vm.legacy_named_read_completion = reference;
                assert_eq!(run(&mut vm, target).as_int(), Some(42));
                let key = vm.heap.table_entry(table, 5).unwrap().0;
                vm.heap.table_store(table, 5, key, Value::int(93)).unwrap();
                let before = vm.work_probe.direct_named_reads.get();
                let task = vm
                    .start(FunctionId(0), &[target], RootOwner(1), Context::Ordinary)
                    .unwrap();
                let result = vm
                    .run_slice(
                        task,
                        SliceBudget {
                            instructions: 1,
                            work: u64::MAX,
                            allocation_bytes: u64::MAX,
                            deadline_ns: None,
                        },
                        || 0,
                    )
                    .unwrap();
                assert!(matches!(result, RunOutcome::BudgetYield(_)));
                let Frame::Script(frame) = *vm.active.frames.data.last().unwrap() else {
                    unreachable!()
                };
                assert!(matches!(frame.phase, ScriptPhase::Body));
                assert!(frame.table_work.is_none());
                assert_eq!(vm.value(frame.regs, 1).unwrap().as_int(), Some(93));
                assert_eq!(
                    vm.work_probe.direct_named_reads.get() - before,
                    u64::from(!reference)
                );
                assert!(vm.active.equality_cursors.data.is_empty());
                review_tests::gc(&mut vm);
                let registry = SnapshotRegistry::default();
                let mut capture = vm.snapshot_begin(&registry).unwrap();
                while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
                let image =
                    VmSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
                let mut restoring = Vm::restore_begin(
                    &image,
                    vm.program().clone(),
                    RuntimeSupport::default(),
                    vm.limits,
                    SessionEpoch(116),
                    &registry,
                )
                .unwrap();
                while !restoring
                    .step(SnapshotBudget { units: 1 })
                    .unwrap()
                    .complete
                {}
                let (mut restored, map) = restoring.finish().unwrap();
                let result = restored
                    .run_slice(map.task(task).unwrap(), SliceBudget::default(), || 0)
                    .unwrap();
                let RunOutcome::Returned(delivery) = result else {
                    panic!("restored direct read {result:?}")
                };
                assert_eq!(
                    restored.delivery_value(delivery).unwrap().as_int(),
                    Some(93)
                );
            }
        }
    }
}

#[test]
fn hard_observation_failure_keeps_original_matched_cursor_and_work() {
    let trace = |reference| {
        let (mut vm, target, table) = load(false, true, false, true, false);
        vm.legacy_named_read_completion = reference;
        assert_eq!(run(&mut vm, target).as_int(), Some(42));
        let task = vm
            .start(FunctionId(0), &[target], RootOwner(1), Context::Ordinary)
            .unwrap();
        assert!(matches!(
            vm.run_slice(
                task,
                SliceBudget {
                    instructions: 0,
                    work: u64::MAX,
                    allocation_bytes: u64::MAX,
                    deadline_ns: None,
                },
                || 0
            )
            .unwrap(),
            RunOutcome::BudgetYield(_)
        ));
        let output = vm.heap.alloc_derived(Value::NULL).unwrap();
        vm.heap
            .add_root(RootOwner(2), RootKind::Host, output)
            .unwrap();
        vm.heap.begin_derived(output).unwrap();
        for field in 0..4 {
            vm.heap.observe_read(table, Some(field)).unwrap();
        }
        let usage = vm.heap.budget_usage();
        let deny_growth = usage.reserve(usage.remaining_bytes()).unwrap();
        let error = vm
            .run_slice(
                task,
                SliceBudget {
                    instructions: 1,
                    work: u64::MAX,
                    allocation_bytes: u64::MAX,
                    deadline_ns: None,
                },
                || 0,
            )
            .unwrap_err();
        let Frame::Script(frame) = *vm.active.frames.data.last().unwrap() else {
            unreachable!()
        };
        assert!(matches!(frame.phase, ScriptPhase::TableScan));
        assert!(frame.table_work.unwrap().matched());
        let roots: Vec<_> = [frame.regs, frame.env, vm.active.equality_roots.unwrap()]
            .into_iter()
            .map(|range| {
                (0..vm.slots.len(range).unwrap())
                    .map(|i| vm.slots.read(at(range, i)).ok())
                    .collect::<Vec<_>>()
            })
            .collect();
        let result = (
            format!("{error:?}"),
            vm.last_usage,
            postcard::to_allocvec(&vm.active.frames.data).unwrap(),
            postcard::to_allocvec(&roots).unwrap(),
            vm.active.equality_cursors.data.clone(),
        );
        drop(deny_growth);
        result
    };
    assert_eq!(trace(false), trace(true));
}
