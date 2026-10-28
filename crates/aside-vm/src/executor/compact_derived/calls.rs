//! Differential checks for the call and branch shapes used by animated bindings.
use super::*;

fn sample_ticket(vm: &mut Vm, task: TaskId) -> HostTicket {
    loop {
        match vm.run_slice(task, SliceBudget::default(), || 0).unwrap() {
            RunOutcome::HostCall(ticket) if ticket.native == NativeId(1) => return ticket,
            RunOutcome::HostCall(ticket) => vm.respond_derive(ticket).unwrap(),
            RunOutcome::BudgetYield(_) => {}
            other => panic!("sample call: {other:?}"),
        }
    }
}

#[test]
fn compact_dynamic_calls_and_branches_keep_budgeted_results_and_dependency_reads() {
    for generic in [true, false] {
        let (mut vm, output, source, _) = prepared_expression(generic, true);
        for (input, expected, called) in [(2, 20, true), (-1, -41, false), (1, 41, true)] {
            vm.heap
                .publish_host_property(
                    crate::PropertyId {
                        host: source,
                        field: 0,
                    },
                    Value::int(input),
                )
                .unwrap();
            assert!(vm.heap.next_derived().unwrap().unwrap().same_bits(output));
            let entries = vm.native_entries;
            let task = vm.start_derived(output, RootOwner(1)).unwrap();
            let RunOutcome::Returned(delivery) = drive(&mut vm, task, true).0 else {
                panic!("derive");
            };
            assert_eq!(
                vm.heap
                    .host_property(crate::PropertyId {
                        host: output,
                        field: 0
                    })
                    .unwrap()
                    .as_int(),
                Some(expected)
            );
            assert_eq!(
                vm.native_entries - entries,
                u64::from(generic) + u64::from(called)
            );
            vm.discard_delivery(delivery).unwrap();
            assert!(!vm.heap.reactive_evaluating());
        }
    }
}

#[test]
fn compact_nested_native_snapshot_restores_receiver_output_and_continuation() {
    for generic in [true, false] {
        let (mut vm, output, _, support) = prepared_expression(generic, true);
        let task = vm.start_derived(output, RootOwner(1)).unwrap();
        let ticket = sample_ticket(&mut vm, task);
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
        let ticket = map.ticket(ticket).unwrap();
        let output = vm.reactive_task_output(task).unwrap().unwrap();
        let args = vm.host_arguments(ticket).unwrap();
        assert_eq!(vm.heap.info(args[0]).unwrap().kind, ObjectKind::Host);
        let value = args[1];
        vm.respond(ticket, HostResponse::Ready(value)).unwrap();
        assert!(matches!(
            drive(&mut vm, task, true).0,
            RunOutcome::Returned(_)
        ));
        assert_eq!(
            vm.heap
                .host_property(crate::PropertyId {
                    host: output,
                    field: 0
                })
                .unwrap()
                .as_int(),
            Some(41)
        );
        assert!(!vm.heap.reactive_evaluating());
    }
}

#[test]
fn compact_nested_native_fault_keeps_source_stack_and_cancellation_ends_capture() {
    let mut stacks = Vec::new();
    for generic in [true, false] {
        let (mut vm, output, _, _) = prepared_expression(generic, true);
        let task = vm.start_derived(output, RootOwner(1)).unwrap();
        let ticket = sample_ticket(&mut vm, task);
        vm.respond_language_error(ticket, FaultKind::Type, Value::NULL)
            .unwrap();
        let RunOutcome::Fault(delivery) = drive(&mut vm, task, true).0 else {
            panic!("nested fault");
        };
        let info = vm.delivery_fault(delivery).unwrap().unwrap();
        let mut cursor = vm.heap.fault_stack(info.id).unwrap();
        let mut entries = Vec::new();
        while let Some(entry) = cursor.next(&vm.heap).unwrap() {
            entries.push((entry.location, entry.module, entry.kind));
        }
        stacks.push((info.code, info.location, entries));
        assert!(!vm.heap.reactive_evaluating());
        vm.discard_delivery(delivery).unwrap();
        let task = vm.start_derived(output, RootOwner(1)).unwrap();
        sample_ticket(&mut vm, task);
        assert!(vm.heap.reactive_evaluating());
        vm.cancel_task(task).unwrap();
        loop {
            match vm.run_slice(task, SliceBudget::default(), || 0) {
                Err(VmError::Idle) => break,
                Ok(RunOutcome::BudgetYield(_)) => {}
                other => panic!("cancel: {other:?}"),
            }
        }
        assert!(!vm.heap.reactive_evaluating());
        review_tests::collect(&mut vm);
        assert!(vm.active.frames.data.is_empty());
    }
    assert_eq!(stacks[0], stacks[1]);
}

#[test]
#[ignore = "release differential branch and bound-native derived measurement"]
fn compact_called_derived_cost() {
    for generic in [true, false] {
        let (mut vm, output, _, _) = prepared_expression(generic, true);
        let entries = vm.native_entries;
        let mut work = 0;
        let mut instructions = 0;
        let before = vm.heap.stats();
        let start = std::time::Instant::now();
        for _ in 0..20000 {
            let task = vm.start_derived(output, RootOwner(1)).unwrap();
            let (result, w, i, _, _, _) = drive(&mut vm, task, false);
            work += w;
            instructions += i;
            let RunOutcome::Returned(delivery) = result else {
                panic!("derive");
            };
            vm.discard_delivery(delivery).unwrap();
        }
        let elapsed = start.elapsed().as_micros();
        assert_eq!(
            vm.native_entries - entries,
            if generic { 40000 } else { 20000 }
        );
        eprintln!(
            "compact_called_derived generic={generic} evaluations=20000 elapsed_us={elapsed} work={work} instructions={instructions} native_entries={} added_heap_bytes={} added_heap_chunks={}",
            vm.native_entries - entries,
            vm.heap.stats().charged_bytes - before.charged_bytes,
            vm.heap.stats().chunk_allocations - before.chunk_allocations
        );
    }
}
#[test]
fn compact_dynamic_builtin_keeps_output_root_during_sliced_search() {
    for generic in [true, false] {
        let (mut vm, output, source, _) = prepared_expression(generic, true);
        let values = (0..128).map(Value::int).collect::<Vec<_>>();
        let array = vm.heap.alloc_array(&values).unwrap();
        let method = vm
            .bound_builtin(array, Builtin::Find, Effects::ALL)
            .unwrap();
        let fields = vm.heap.host_fields(source).unwrap();
        let key = vm.heap.table_entry(fields, 1).unwrap().0;
        vm.heap.table_store(fields, 1, key, method).unwrap();
        let entries = vm.native_entries;
        let task = vm.start_derived(output, RootOwner(1)).unwrap();
        let RunOutcome::Returned(delivery) = drive(&mut vm, task, true).0 else {
            panic!("builtin result");
        };
        assert_eq!(
            vm.heap
                .host_property(crate::PropertyId {
                    host: output,
                    field: 0
                })
                .unwrap()
                .as_int(),
            Some(41)
        );
        assert_eq!(vm.native_entries - entries, u64::from(generic));
        vm.discard_delivery(delivery).unwrap();
        assert!(!vm.heap.reactive_evaluating());
    }
}

#[test]
#[ignore = "release repeated native receiver binding measurement"]
fn native_binding_working_set_cost() {
    for count in [64, 128, 256] {
        let (mut vm, _, _, _) = prepared_expression(false, true);
        let mut receivers = Vec::new();
        for _ in 0..count {
            let fields = vm.heap.alloc_table().unwrap();
            receivers.push(vm.heap.alloc_host(fields, &[], None).unwrap());
        }
        for &receiver in &receivers {
            vm.bind_native(receiver, FunctionId(4)).unwrap();
        }
        let before = vm.heap.stats();
        let hits = vm.native_binding_hits;
        let misses = vm.native_binding_misses;
        let start = std::time::Instant::now();
        for _ in 0..128 {
            for &receiver in &receivers {
                vm.bind_native(receiver, FunctionId(4)).unwrap();
            }
        }
        eprintln!(
            "native_bindings count={count} rounds=128 elapsed_us={} hits={} misses={} added_objects={} added_bytes={}",
            start.elapsed().as_micros(),
            vm.native_binding_hits - hits,
            vm.native_binding_misses - misses,
            vm.heap.stats().live_objects - before.live_objects,
            vm.heap.stats().charged_bytes - before.charged_bytes
        );
    }
}

#[test]
fn native_binding_set_remains_weak_and_rejects_collected_generations() {
    let (mut vm, _, _, _) = prepared_expression(false, true);
    let fields = vm.heap.alloc_table().unwrap();
    let receiver = vm.heap.alloc_host(fields, &[], None).unwrap();
    let root = vm
        .heap
        .add_root(RootOwner(1), RootKind::Host, receiver)
        .unwrap();
    let first = vm.bind_native(receiver, FunctionId(4)).unwrap();
    assert!(
        vm.bind_native(receiver, FunctionId(4))
            .unwrap()
            .same_bits(first)
    );
    review_tests::collect(&mut vm);
    assert!(vm.heap.info(first).is_err());
    let next = vm.bind_native(receiver, FunctionId(4)).unwrap();
    assert!(!first.same_bits(next));
    assert!(
        vm.bind_native(receiver, FunctionId(4))
            .unwrap()
            .same_bits(next)
    );
    vm.heap.release_root(root).unwrap();
    review_tests::collect(&mut vm);
    assert!(vm.heap.info(receiver).is_err());
    assert!(vm.bind_native(receiver, FunctionId(4)).is_err());
}
