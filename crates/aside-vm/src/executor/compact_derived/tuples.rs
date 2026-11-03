use super::*;

fn assert_tuple(vm: &Vm, output: Value, len: u32) {
    let value = vm
        .heap
        .host_property(crate::PropertyId {
            host: output,
            field: 0,
        })
        .unwrap();
    assert_eq!(vm.heap.info(value).unwrap().kind, ObjectKind::Tuple);
    assert_eq!(vm.heap.sequence_len(value).unwrap(), len);
    for index in 0..len {
        assert_eq!(
            vm.heap.sequence_get(value, index).unwrap().as_int(),
            Some(20)
        );
    }
    assert!(!vm.heap.reactive_evaluating());
}

fn changed_tuple(generic: bool, len: u32) -> (Vm, Value, TaskId, RuntimeSupport) {
    let (mut vm, output, source, support) = prepared_body(generic, false, false, true, Some(len));
    vm.heap
        .publish_host_property(
            crate::PropertyId {
                host: source,
                field: 0,
            },
            Value::int(2),
        )
        .unwrap();
    let task = vm.start_derived(output, RootOwner(1)).unwrap();
    (vm, output, task, support)
}

#[test]
fn tuple_derives_preserve_budgeted_construction_and_skip_callback_entry() {
    for len in [0, 2, 17] {
        for generic in [true, false] {
            let (mut vm, output, task, _) = changed_tuple(generic, len);
            let entries = vm.native_entries;
            assert!(matches!(
                drive(&mut vm, task, true).0,
                RunOutcome::Returned(_)
            ));
            assert_tuple(&vm, output, len);
            assert_eq!(vm.native_entries - entries, u64::from(generic));
        }
    }
}

#[test]
fn compact_tuple_resumes_mid_construction_after_snapshot_and_gc() {
    let (mut vm, _, task, support) = changed_tuple(false, 17);
    loop {
        if matches!(vm.active.frames.data.last(), Some(Frame::Script(script))
            if matches!(script.phase, ScriptPhase::Sequence { index: 8, tuple: true, .. }))
        {
            break;
        }
        assert!(matches!(
            vm.run_slice(
                task,
                SliceBudget {
                    instructions: 1,
                    work: vm.last_usage.required_work.max(1),
                    allocation_bytes: vm.last_usage.required_allocation,
                    ..Default::default()
                },
                || 0,
            )
            .unwrap(),
            RunOutcome::BudgetYield(_)
        ));
    }
    review_tests::collect(&mut vm);
    let registry = SnapshotRegistry::default();
    let mut capture = vm.snapshot_begin(&registry).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let image = capture.finish().unwrap();
    let mut restore = Vm::restore_begin(
        &image,
        vm.program().clone(),
        support,
        vm.limits,
        SessionEpoch(2),
        &registry,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let (mut vm, map) = restore.finish().unwrap();
    let task = map.task(task).unwrap();
    let output = vm.reactive_task_output(task).unwrap().unwrap();
    assert!(matches!(
        drive(&mut vm, task, true).0,
        RunOutcome::Returned(_)
    ));
    assert_tuple(&vm, output, 17);
}
