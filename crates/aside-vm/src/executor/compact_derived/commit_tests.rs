use super::*;

fn ready() -> (Vm, TaskId, Value, RuntimeSupport) {
    let (mut vm, output, source, mut support) = prepared(false);
    support.natives[0].standard_derived_commit = true;
    vm.native_parameters.data[0].standard_derived_commit = true;
    vm.heap
        .publish_host_property(
            crate::PropertyId {
                host: source,
                field: 0,
            },
            Value::int(2),
        )
        .unwrap();
    assert!(vm.heap.next_derived().unwrap().unwrap().same_bits(output));
    let task = vm.start_derived(output, RootOwner(1)).unwrap();
    loop {
        if matches!(vm.active.frames.data.last(), Some(Frame::Native(n)) if matches!(n.phase, NativePhase::Resume(NativeInput::CallbackReturned)))
        {
            break;
        }
        let budget = SliceBudget {
            instructions: 1,
            work: vm.last_usage.required_work.max(1),
            allocation_bytes: vm.last_usage.required_allocation,
            ..Default::default()
        };
        assert!(matches!(
            vm.run_slice(task, budget, || 0).unwrap(),
            RunOutcome::BudgetYield(_)
        ));
    }
    (vm, task, output, support)
}

fn value(vm: &Vm, output: Value) -> i64 {
    vm.heap
        .host_property(crate::PropertyId {
            host: output,
            field: 0,
        })
        .unwrap()
        .as_int()
        .unwrap()
}

fn commit(vm: &mut Vm, task: TaskId) {
    let mut calls = 0;
    let budget = SliceBudget {
        deadline_ns: Some(1),
        ..Default::default()
    };
    assert!(matches!(
        vm.run_slice(task, budget, || {
            let now = calls;
            calls += 1;
            now
        })
        .unwrap(),
        RunOutcome::BudgetYield(_)
    ));
    assert_eq!(calls, 2);
    assert!(
        matches!(vm.active.frames.data.last(), Some(Frame::Native(n)) if matches!(n.phase, NativePhase::Commit(false)))
    );
    assert!(!vm.heap.reactive_evaluating());
}

#[test]
fn inline_commit_reserves_atomic_cost_before_mutation_and_checks_time_afterward() {
    let (mut vm, task, output, _) = ready();
    let previous = value(&vm, output);
    let resumes = vm.native_resumes;
    let outcome = vm
        .run_slice(
            task,
            SliceBudget {
                work: 1,
                ..Default::default()
            },
            || 0,
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::BudgetYield(_)));
    assert_eq!(vm.last_usage.required_work, 2);
    assert_eq!(value(&vm, output), previous);
    assert!(vm.heap.reactive_evaluating());
    commit(&mut vm, task);
    assert_eq!(value(&vm, output), 20);
    assert!(matches!(
        drive(&mut vm, task, true).0,
        RunOutcome::Returned(_)
    ));
    assert_eq!(vm.native_resumes, resumes);
}

#[test]
fn inline_commit_gc_and_restore_preserve_both_sides_of_publication() {
    for committed in [false, true] {
        let (mut vm, mut task, output, support) = ready();
        if committed {
            commit(&mut vm, task);
        }
        let before = value(&vm, output);
        let resumes = vm.native_resumes;
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
        let (restored, map) = restore.finish().unwrap();
        vm = restored;
        task = map.task(task).unwrap();
        let output = vm.reactive_task_output(task).unwrap().unwrap();
        assert_eq!(value(&vm, output), before);
        assert!(matches!(
            drive(&mut vm, task, true).0,
            RunOutcome::Returned(_)
        ));
        assert_eq!(value(&vm, output), 20);
        assert_eq!(vm.native_resumes, resumes);
        assert!(!vm.heap.reactive_evaluating());
    }
}

#[test]
fn inline_commit_cancellation_preserves_the_last_published_value() {
    for committed in [false, true] {
        let (mut vm, task, output, _) = ready();
        let before = value(&vm, output);
        if committed {
            commit(&mut vm, task);
        }
        vm.cancel_task(task).unwrap();
        assert!(matches!(
            vm.run_slice(task, SliceBudget::default(), || 0),
            Err(VmError::Idle)
        ));
        assert_eq!(value(&vm, output), if committed { 20 } else { before });
        assert!(!vm.heap.reactive_evaluating());
        assert!(vm.active.frames.data.is_empty());
        review_tests::collect(&mut vm);
    }
}

#[test]
fn inline_commit_publication_failure_preserves_connected_properties() {
    use crate::{HeapError, HostPropertyValidator, PropertyId, PropertyRead};

    static SELECTION: HostPropertyValidator =
        HostPropertyValidator::new("test.selection", 1, selection);
    fn selection(view: &PropertyRead<'_>, host: Value, _: u32, _: Value) -> Result<(), HeapError> {
        let length = view.read(PropertyId { host, field: 0 })?.as_int().unwrap();
        let caret = view.read(PropertyId { host, field: 1 })?.as_int().unwrap();
        if 0 <= caret && caret <= length {
            Ok(())
        } else {
            Err(HeapError::TypeMismatch)
        }
    }

    let (mut vm, task, output, _) = ready();
    let previous = value(&vm, output);
    let fields = vm.heap.alloc_table().unwrap();
    for (field, value) in [(0, previous), (1, 30)] {
        vm.heap
            .table_store(fields, field, Value::int(field.into()), Value::int(value))
            .unwrap();
    }
    let follower = vm.heap.alloc_host(fields, &[], Some(&SELECTION)).unwrap();
    vm.heap.observe_host_properties(follower, 2, 0).unwrap();
    let length = PropertyId {
        host: follower,
        field: 0,
    };
    let reference = vm
        .heap
        .alloc_property_ref(
            PropertyId {
                host: output,
                field: 0,
            },
            false,
        )
        .unwrap();
    vm.heap.bind_host_property(length, reference).unwrap();
    let resumes = vm.native_resumes;
    let RunOutcome::Fault(delivery) = drive(&mut vm, task, true).0 else {
        panic!("derived publication must reject caret beyond length");
    };
    assert_eq!(
        vm.delivery_fault(delivery).unwrap().unwrap().code.kind,
        FaultKind::Type
    );
    assert_eq!(value(&vm, output), previous);
    assert_eq!(
        vm.heap.host_property(length).unwrap().as_int(),
        Some(previous)
    );
    assert!(!vm.heap.reactive_evaluating());
    assert_eq!(vm.native_resumes, resumes);
}
