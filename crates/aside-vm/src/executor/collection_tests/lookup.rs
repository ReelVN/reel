//! Incremental collection lookup and shape changes.
use super::*;

#[test]
fn host_own_string_lookup_is_bounded_and_does_not_confuse_null_with_missing() {
    let mut vm = load(table_package(
        vec![Const::Utf8("ab".into()), Const::Utf8("ac".into())],
        vec![Const::Int(1), Const::Null],
    ));
    let task = vm
        .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
        .unwrap();
    let table = returned(&mut vm, task);
    for query in ["ac", "missing"] {
        let mut cursor = vm.heap().table_own_string_lookup(table, query).unwrap();
        let zero = vm.heap().table_own_string_step(&mut cursor, 0).unwrap();
        assert_eq!(zero.work, 0);
        assert!(matches!(zero.status, TableLookupStatus::Pending));
        let mut complete = false;
        for _ in 0..30 {
            let step = vm.heap().table_own_string_step(&mut cursor, 1).unwrap();
            assert!(step.work <= 1);
            if let TableLookupStatus::Complete(value) = step.status {
                assert_eq!(
                    value.map(Value::tag),
                    if query == "ac" {
                        Some(ValueTag::Null)
                    } else {
                        None
                    }
                );
                complete = true;
                break;
            }
            gc(&mut vm);
        }
        assert!(complete);
    }
    let mut cursor = vm.heap().table_own_string_lookup(table, "ab").unwrap();
    vm.heap_mut()
        .table_store(table, 0, Value::NULL, Value::int(9))
        .unwrap();
    assert_eq!(
        vm.heap().table_own_string_step(&mut cursor, 1).unwrap_err(),
        HeapError::ConcurrentModification
    );
}
#[test]
fn table_lookup_structure_change_and_cooperative_cancel_release_key_stack() {
    for cancel in [false, true] {
        let mut p = table_package(vec![Const::Utf8("x".repeat(256))], vec![Const::Int(7)]);
        p.functions[0].code.pop();
        p.functions[0]
            .code
            .extend([op(Opcode::IndexGet, 1, 2, 0), op(Opcode::Return, 1, 0, 0)]);
        p.functions[0].source_map.clear();
        let mut vm = load(p);
        let task = vm
            .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
            .unwrap();
        let mut target = None;
        for _ in 0..1000 {
            if let Some(Frame::Script(s)) = vm.active.frames.data.last().copied()
                && s.table_work.is_some()
                && matches!(s.phase, ScriptPhase::Equality { .. })
            {
                target = Some(vm.value(s.env, 1).unwrap());
                break;
            }
            tick(&mut vm, task);
        }
        let table = target.expect("lookup must yield in its long string comparison");
        if cancel {
            vm.cancel_cooperatively(task).unwrap();
        } else {
            vm.heap_mut()
                .table_store(table, 1, Value::NULL, Value::int(8))
                .unwrap();
        }
        fault(
            &mut vm,
            task,
            if cancel {
                FaultKind::Cancelled
            } else {
                FaultKind::ConcurrentModification
            },
        );
        assert!(vm.active.equality_cursors.data.is_empty());
    }
}

#[test]
fn member_position_hints_recheck_each_shape_and_host_write_validator() {
    for host in [false, true] {
        for write in [false, true] {
            let mut vm = load(point_lookup_package(false, write));
            for position in [2, 0, 1] {
                let fields = vm.heap_mut().alloc_table().unwrap();
                for index in 0..3 {
                    let name = if index == position {
                        "position".into()
                    } else {
                        format!("other{index}")
                    };
                    let key = vm.heap_mut().alloc_string(&name).unwrap();
                    vm.heap_mut()
                        .table_store(fields, index, key, Value::int(42))
                        .unwrap();
                }
                let target = if host {
                    vm.heap_mut().alloc_host(fields, &[], None).unwrap()
                } else {
                    fields
                };
                let before = vm.work_probe.member_hint_hits.get();
                assert_eq!(
                    warm_point_lookup(&mut vm, target).as_int(),
                    Some(if write { 126 } else { 42 })
                );
                // A different object's key at the stale position cannot select its value.
                assert_eq!(vm.work_probe.member_hint_hits.get(), before);
                assert_eq!(
                    warm_point_lookup(&mut vm, target).as_int(),
                    Some(if write { 126 } else { 42 })
                );
                assert_eq!(vm.work_probe.member_hint_hits.get(), before + 1);
                let (key, _) = vm.heap().table_entry(fields, position).unwrap();
                vm.heap_mut()
                    .table_store(fields, position, key, Value::int(84))
                    .unwrap();
                assert_eq!(
                    warm_point_lookup(&mut vm, target).as_int(),
                    Some(if write { 126 } else { 84 })
                );
            }
        }
    }
    let mut vm = load(point_lookup_package(false, true));
    let fields = vm.heap_mut().alloc_table().unwrap();
    let key = vm.heap_mut().alloc_string("position").unwrap();
    vm.heap_mut()
        .table_store(fields, 0, key, Value::int(42))
        .unwrap();
    let host = vm
        .heap_mut()
        .alloc_host(fields, &[], Some(|_, _, _, _| Err(HeapError::TypeMismatch)))
        .unwrap();
    let root = vm
        .heap_mut()
        .add_root(RootOwner(8), RootKind::Host, host)
        .unwrap();
    for expected_hits in 0..2 {
        let task = vm
            .start(FunctionId(0), &[host], RootOwner(9), Context::Ordinary)
            .unwrap();
        let RunOutcome::Fault(delivery) = terminal(&mut vm, task) else {
            panic!("write validator was bypassed")
        };
        assert_eq!(
            vm.delivery_fault(delivery).unwrap().unwrap().kind,
            FaultKind::Type
        );
        vm.discard_delivery(delivery).unwrap();
        assert_eq!(vm.work_probe.member_hint_hits.get(), expected_hits);
        assert_eq!(
            vm.heap().table_entry(fields, 0).unwrap().1.as_int(),
            Some(42)
        );
    }
    vm.heap_mut().release_root(root).unwrap();
}

#[test]
fn point_lookup_survives_value_updates_but_rejects_structural_changes() {
    check_point_lookup_value_and_structure_changes(false);
}

#[test]
fn point_lookup_snapshot_preserves_structure_version_and_rejects_unsupported_format() {
    check_point_lookup_value_and_structure_changes(true);
}

#[test]
fn derived_point_lookup_observes_the_field_read_at_commit() {
    let mut vm = load(point_lookup_package(false, false));
    let table = vm.heap_mut().alloc_table().unwrap();
    let position = vm.heap_mut().alloc_string("position").unwrap();
    let repeat = vm.heap_mut().alloc_string("repeat").unwrap();
    vm.heap_mut()
        .table_store(table, 0, position, Value::int(42))
        .unwrap();
    vm.heap_mut()
        .table_store(table, 1, repeat, Value::bool(false))
        .unwrap();
    vm.heap_mut().observe_model_object(table).unwrap();
    warm_point_lookup(&mut vm, table);
    let output = vm.heap_mut().alloc_derived(Value::NULL).unwrap();
    let root = vm
        .heap_mut()
        .add_root(RootOwner(8), RootKind::Host, output)
        .unwrap();
    vm.heap_mut().begin_derived(output).unwrap();
    let task = pause_point_lookup(&mut vm, table, Context::Derived);
    pause_matched_lookup(&mut vm, task);
    assert!(vm.work_probe.member_hint_hits.get() > 0);
    vm.heap_mut()
        .table_store(table, 0, position, Value::int(84))
        .unwrap();
    let value = returned(&mut vm, task);
    assert_eq!(value.as_int(), Some(84));
    vm.heap_mut().commit_derived(output, value).unwrap();
    vm.heap_mut()
        .table_store(table, 1, repeat, Value::bool(true))
        .unwrap();
    assert!(
        !vm.heap().derived_pending(),
        "an unrelated value became a dependency"
    );
    vm.heap_mut()
        .table_store(table, 0, position, Value::int(126))
        .unwrap();
    assert!(
        vm.heap_mut()
            .next_derived()
            .unwrap()
            .unwrap()
            .same_bits(output)
    );
    vm.heap_mut().release_root(root).unwrap();
}

#[test]
fn derived_host_lookup_rejects_changed_structure_before_resolving_old_entry() {
    for indexed in [false, true] {
        for reorder in [false, true] {
            let mut vm = load(point_lookup_package(indexed, false));
            let fields = vm.heap_mut().alloc_table().unwrap();
            let position = vm.heap_mut().alloc_string("position").unwrap();
            let repeat = vm.heap_mut().alloc_string("repeat").unwrap();
            vm.heap_mut()
                .table_store(fields, 0, position, Value::int(42))
                .unwrap();
            vm.heap_mut()
                .table_store(fields, 1, repeat, Value::bool(false))
                .unwrap();
            let host = vm.heap_mut().alloc_host(fields, &[], None).unwrap();
            vm.heap_mut().observe_host_properties(host, 0, 2).unwrap();
            if !indexed {
                warm_point_lookup(&mut vm, host);
            }
            let output = vm.heap_mut().alloc_derived(Value::NULL).unwrap();
            let root = vm
                .heap_mut()
                .add_root(RootOwner(8), RootKind::Host, output)
                .unwrap();
            vm.heap_mut().begin_derived(output).unwrap();
            let task = pause_point_lookup(&mut vm, host, Context::Derived);
            for _ in 0..1000 {
                if matches!(vm.active.frames.data.last(),Some(Frame::Script(s)) if s.table_work.is_some_and(|work|work.matched()))
                {
                    break;
                }
                let result = vm
                    .run_slice(
                        task,
                        SliceBudget {
                            instructions: 1,
                            work: vm.last_usage.required_work.max(1),
                            ..Default::default()
                        },
                        || 0,
                    )
                    .unwrap();
                assert!(matches!(result, RunOutcome::BudgetYield(_)));
            }
            assert!(
                matches!(vm.active.frames.data.last(),Some(Frame::Script(s)) if s.table_work.is_some_and(|work|work.matched()))
            );
            vm.heap_mut().clear_collection(fields).unwrap();
            if reorder {
                vm.heap_mut()
                    .table_store(fields, 0, repeat, Value::bool(false))
                    .unwrap();
                vm.heap_mut()
                    .table_store(fields, 1, position, Value::int(42))
                    .unwrap();
            }
            fault(&mut vm, task, FaultKind::ConcurrentModification);
            vm.heap_mut().end_derived(output).unwrap();
            vm.heap_mut().release_root(root).unwrap();
        }
    }
}
