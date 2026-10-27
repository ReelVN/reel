//! Budgeted collection copies and atomic mutation.
use super::*;

#[test]
fn packed_spread_and_script_rest_capture_fresh_outer_arrays() {
    let mut vm = load(spread_rest_package());
    let first = vm
        .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
        .unwrap();
    let a = returned(&mut vm, first);
    let second = vm
        .start(FunctionId(0), &[], RootOwner(10), Context::Ordinary)
        .unwrap();
    let b = returned(&mut vm, second);
    assert!(!a.same_bits(b));
    for array in [a, b] {
        assert_eq!(vm.heap().sequence_len(array).unwrap(), 2);
        assert_eq!(vm.heap().sequence_get(array, 0).unwrap().as_int(), Some(10));
        assert_eq!(vm.heap().sequence_get(array, 1).unwrap().as_int(), Some(20));
    }
}
#[test]
fn spread_detects_overwrite_between_capture_steps() {
    let mut vm = load(spread_rest_package());
    let task = vm
        .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
        .unwrap();
    let mut source = None;
    for _ in 0..1000 {
        if let Some(Frame::Script(s)) = vm.active.frames.data.last().copied()
            && matches!(s.phase, ScriptPhase::Body)
            && s.pc == test_pc(&vm, s.code.function.0, 4)
        {
            vm.run_slice(
                task,
                SliceBudget {
                    instructions: 1,
                    work: 1,
                    ..SliceBudget::default()
                },
                || 0,
            )
            .unwrap();
            let Frame::Script(s) = *vm.active.frames.data.last().unwrap() else {
                panic!()
            };
            assert!(matches!(s.phase, ScriptPhase::Spread { .. }));
            source = Some(vm.value(s.env, 2).unwrap());
            break;
        }
        // Exactly one instruction admission ends at the spread phase.
        tick(&mut vm, task);
    }
    let source = source.expect("spread pause");
    vm.heap_mut().set_field(source, 1, Value::int(999)).unwrap();
    fault(&mut vm, task, FaultKind::ConcurrentModification);
}
#[test]
fn forged_odd_packed_destinations_fail_before_any_half_pair_or_allocation() {
    for opcode in [Opcode::ArgsPush, Opcode::ArgsNamed, Opcode::ArgsSpread] {
        let mut vm = load(packed_destination_package(63, opcode));
        let task = vm
            .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
            .unwrap();
        let packed = reach_packed_append(&mut vm, task, 63);
        let before = vm.heap().info(packed).unwrap();
        let chunks = vm.heap().stats().chunk_allocations;
        let RunOutcome::BudgetYield(usage) = vm
            .run_slice(
                task,
                SliceBudget {
                    instructions: 1,
                    work: 2,
                    allocation_bytes: 0,
                    ..SliceBudget::default()
                },
                || 0,
            )
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(usage.allocation_bytes, 0);
        assert_eq!(vm.heap().stats().chunk_allocations, chunks);
        let after = vm.heap().info(packed).unwrap();
        assert_eq!(after.len, 63);
        assert_eq!(after.content_version, before.content_version);
        fault(&mut vm, task, FaultKind::Type);
    }
}
#[test]
fn packed_pair_allocation_is_admitted_before_either_element_is_published() {
    for len in [crate::payload::INLINE_VALUES as u32, 32, 96] {
        let allocation = Values::growth_bytes(len.into(), 2);
        for opcode in [Opcode::ArgsPush, Opcode::ArgsNamed, Opcode::ArgsSpread] {
            let mut vm = load(packed_destination_package(len, opcode));
            let task = vm
                .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
                .unwrap();
            let packed = reach_packed_append(&mut vm, task, len);
            let version = vm.heap().info(packed).unwrap().content_version;
            for _ in 0..3 {
                let RunOutcome::BudgetYield(usage) = vm
                    .run_slice(
                        task,
                        SliceBudget {
                            instructions: 1,
                            work: 3,
                            allocation_bytes: allocation - 1,
                            ..SliceBudget::default()
                        },
                        || 0,
                    )
                    .unwrap()
                else {
                    panic!()
                };
                assert_eq!(usage.allocation_bytes, 0);
                assert_eq!(usage.required_allocation, allocation);
                let info = vm.heap().info(packed).unwrap();
                assert_eq!(info.len, len);
                assert_eq!(info.content_version, version);
                gc(&mut vm);
            }
            let RunOutcome::BudgetYield(usage) = vm
                .run_slice(
                    task,
                    SliceBudget {
                        instructions: 1,
                        work: 2,
                        allocation_bytes: allocation,
                        ..SliceBudget::default()
                    },
                    || 0,
                )
                .unwrap()
            else {
                panic!()
            };
            assert_eq!(usage.allocation_bytes, allocation);
            assert_eq!(vm.heap().info(packed).unwrap().len, len + 2);
            assert_eq!(
                vm.heap().sequence_get(packed, len).unwrap().as_int(),
                Some(if opcode == Opcode::ArgsNamed { 4 } else { -1 })
            );
            assert!(vm.heap().sequence_get(packed, len + 1).is_ok());
        }
    }
}
#[test]
fn spread_destination_overwrite_and_odd_append_invalidate_resume() {
    for append in [false, true] {
        let mut vm = load(spread_rest_package());
        let task = vm
            .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
            .unwrap();
        let mut packed = None;
        for _ in 0..1000 {
            let Some(Frame::Script(s)) = vm.active.frames.data.last().copied() else {
                panic!()
            };
            if s.pc == test_pc(&vm, s.code.function.0, 4) && matches!(s.phase, ScriptPhase::Body) {
                vm.run_slice(
                    task,
                    SliceBudget {
                        instructions: 1,
                        work: 3,
                        ..SliceBudget::default()
                    },
                    || 0,
                )
                .unwrap();
                let Frame::Script(s) = *vm.active.frames.data.last().unwrap() else {
                    panic!()
                };
                assert!(matches!(
                    s.phase,
                    ScriptPhase::Spread(collections::SpreadCapture { index: 1, .. })
                ));
                packed = Some(vm.value(s.env, 1).unwrap());
                break;
            }
            tick(&mut vm, task);
        }
        let packed = packed.expect("spread capture pause");
        if append {
            vm.heap_mut().array_push(packed, Value::int(99)).unwrap();
        } else {
            vm.heap_mut().set_field(packed, 1, Value::int(99)).unwrap();
        }
        let root = vm
            .heap_mut()
            .add_root(RootOwner(9), RootKind::Host, packed)
            .unwrap();
        let before = vm.heap().info(packed).unwrap();
        fault(&mut vm, task, FaultKind::ConcurrentModification);
        let after = vm.heap().info(packed).unwrap();
        assert_eq!(after.len, before.len);
        assert_eq!(after.content_version, before.content_version);
        vm.heap_mut().release_root(root).unwrap();
    }
}

#[test]
fn table_remove_cancel_and_concurrent_write_preserve_the_original_until_commit() {
    use crate::snapshot::{SnapshotBudget, SnapshotRegistry, VmSnapshot};
    for observed in [false, true] {
        for action in 0..3 {
            let mut p = method_package("remove");
            let key_name = SymbolId(p.symbols.len() as u32);
            p.symbols.push("key".into());
            p.signatures[0].parameters.push(Parameter {
                name: key_name,
                ty: TypeId(0),
                mutable: false,
                default: None,
            });
            p.functions[0].register_count = 4;
            p.functions[0].code[0].operands[0] = 2;
            p.functions[0].code[1].operands[0] = 3;
            p.functions[0].code[2].operands[0] = 3;
            let Descriptor::Call(call) = &mut p.functions[0].descriptors[0] else {
                unreachable!()
            };
            call.target = CallTarget::Register(Reg(2));
            call.arguments = Arguments::Window(RegWindow {
                start: Reg(1),
                len: 1,
            });
            let mut vm = load(p);
            let mut table = vm.heap_mut().alloc_table().unwrap();
            for n in 0..130 {
                let child = vm.heap_mut().alloc_array(&[Value::int(n)]).unwrap();
                vm.heap_mut()
                    .table_store(table, n as u32, Value::int(n), child)
                    .unwrap();
            }
            if observed {
                vm.heap_mut().observe_model_object(table).unwrap();
            }
            let mut root = vm
                .heap_mut()
                .add_root(RootOwner(8), RootKind::Host, table)
                .unwrap();
            let mut task = vm
                .start(
                    FunctionId(0),
                    &[table, Value::int(64)],
                    RootOwner(9),
                    Context::Ordinary,
                )
                .unwrap();
            loop {
                let result = tick(&mut vm, task);
                assert!(matches!(result, RunOutcome::BudgetYield(_)));
                let Frame::Script(s) = *vm.active.frames.data.last().unwrap() else {
                    panic!()
                };
                if matches!(s.phase, ScriptPhase::Bulk(_))
                    && vm.heap().info(vm.value(s.env, 2).unwrap()).unwrap().len >= 65
                {
                    assert_eq!(vm.heap().table_len(table).unwrap(), 130);
                    break;
                }
            }
            if action == 0 {
                let registry = SnapshotRegistry::default();
                let mut capture = vm.snapshot_begin(&registry).unwrap();
                while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
                let image = capture.finish().unwrap();
                let image = VmSnapshot::decode(&image.encode().unwrap()).unwrap();
                let mut restore = Vm::restore_begin(
                    &image,
                    vm.program().clone(),
                    RuntimeSupport {
                        host_types: Vec::new(),
                        opcodes: vec![],
                        capabilities: vec![],
                        natives: vec![],
                    },
                    VmLimits::default(),
                    SessionEpoch(82),
                    &registry,
                )
                .unwrap();
                while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
                let (restored, map) = restore.finish().unwrap();
                table = map.value(table).unwrap();
                root = map.root(root).unwrap();
                task = map.task(task).unwrap();
                vm = restored;
            }
            match action {
                0 => {
                    let RunOutcome::Returned(id) = terminal(&mut vm, task) else {
                        panic!()
                    };
                    let removed = vm.delivery_value(id).unwrap();
                    assert_eq!(
                        vm.heap().sequence_get(removed, 0).unwrap().as_int(),
                        Some(64)
                    );
                    vm.discard_delivery(id).unwrap();
                }
                1 => {
                    vm.heap_mut()
                        .table_store(table, 0, Value::int(0), Value::NULL)
                        .unwrap();
                    fault(&mut vm, task, FaultKind::ConcurrentModification);
                }
                _ => {
                    vm.cancel(RootOwner(9)).unwrap();
                    while let Ok(outcome) = vm.run_slice(task, SliceBudget::default(), || 0) {
                        assert!(matches!(outcome, RunOutcome::BudgetYield(_)));
                    }
                }
            }
            let len = if action == 0 { 129 } else { 130 };
            for n in 0..len {
                let source = n + u32::from(action == 0 && n >= 64);
                let (key, value) = vm.heap().table_entry(table, n).unwrap();
                assert_eq!(key.as_int(), Some(i64::from(source)));
                if action == 1 && n == 0 {
                    assert!(value.same_bits(Value::NULL));
                } else {
                    assert_eq!(
                        vm.heap().sequence_get(value, 0).unwrap().as_int(),
                        Some(i64::from(source))
                    );
                }
            }
            vm.heap_mut().release_root(root).unwrap();
        }
    }
}

#[test]
fn bulk_edit_commit_cancel_and_concurrent_write_preserve_identity_and_gc_edges() {
    for action in 0..3 {
        let mut vm = load(method_package("reverse"));
        let source = vm.heap_mut().alloc_array(&[]).unwrap();
        for n in 0..130 {
            let child = vm.heap_mut().alloc_array(&[Value::int(n)]).unwrap();
            vm.heap_mut().array_push(source, child).unwrap();
        }
        let root = vm
            .heap_mut()
            .add_root(RootOwner(8), RootKind::Host, source)
            .unwrap();
        let task = vm
            .start(FunctionId(0), &[source], RootOwner(9), Context::Ordinary)
            .unwrap();
        let mut admitted = 6;
        loop {
            let outcome = vm
                .run_slice(
                    task,
                    SliceBudget {
                        instructions: 1,
                        work: admitted,
                        allocation_bytes: VALUE_CHUNK_BYTES,
                        deadline_ns: None,
                    },
                    || 0,
                )
                .unwrap();
            let RunOutcome::BudgetYield(usage) = outcome else {
                panic!("{outcome:?}");
            };
            admitted = usage.required_work.max(6);
            let Frame::Script(s) = *vm.active.frames.data.last().unwrap() else {
                panic!()
            };
            if matches!(s.phase, ScriptPhase::Bulk(_))
                && vm.heap().info(vm.value(s.env, 2).unwrap()).unwrap().len >= 65
            {
                for n in 0..130 {
                    let child = vm.heap().sequence_get(source, n).unwrap();
                    assert_eq!(
                        vm.heap().sequence_get(child, 0).unwrap().as_int(),
                        Some(n as i64)
                    );
                }
                break;
            }
        }
        // Begin a new mark cycle after part of the private copy exists. A commit
        // must not move still-unscanned edges behind a completed object cursor.
        vm.begin_collection().unwrap();
        match action {
            0 => loop {
                match tick(&mut vm, task) {
                    RunOutcome::BudgetYield(_) => {}
                    RunOutcome::Returned(id) => {
                        assert!(vm.delivery_value(id).unwrap().same_bits(source));
                        vm.discard_delivery(id).unwrap();
                        break;
                    }
                    other => panic!("{other:?}"),
                }
            },
            1 => {
                vm.heap_mut().set_field(source, 0, Value::NULL).unwrap();
                fault(&mut vm, task, FaultKind::ConcurrentModification);
            }
            _ => {
                vm.cancel(RootOwner(9)).unwrap();
                while let Ok(outcome) = vm.run_slice(task, SliceBudget::default(), || 0) {
                    assert!(matches!(outcome, RunOutcome::BudgetYield(_)));
                }
            }
        }
        while vm.heap().phase() != GcPhase::Idle {
            gc(&mut vm);
        }
        vm.begin_collection().unwrap();
        while vm.heap().phase() != GcPhase::Idle {
            gc(&mut vm);
        }
        for n in 0..130 {
            let child = vm.heap().sequence_get(source, n).unwrap();
            if action == 1 && n == 0 {
                assert!(child.same_bits(Value::NULL));
            } else {
                assert_eq!(
                    vm.heap().sequence_get(child, 0).unwrap().as_int(),
                    Some(if action == 0 { 129 - n } else { n } as i64)
                );
            }
        }
        assert_eq!(vm.stats().frames, 0);
        vm.heap_mut().release_root(root).unwrap();
    }
}

#[test]
fn bulk_edit_work_grows_with_elements_and_commit_has_no_payload_allocation() {
    let mut costs = Vec::new();
    for len in [1024, 4096, 16384] {
        let mut vm = load(method_package("reverse"));
        let source = copy_source(&mut vm, false, len);
        let task = vm
            .start(FunctionId(0), &[source], RootOwner(9), Context::Ordinary)
            .unwrap();
        let before = vm.heap().stats().chunk_allocations;
        let mut work = 0;
        loop {
            let result = vm.run_slice(task, SliceBudget::default(), || 0).unwrap();
            work += vm.stats().usage.work;
            match result {
                RunOutcome::BudgetYield(_) => {}
                RunOutcome::Returned(id) => {
                    assert!(vm.delivery_value(id).unwrap().same_bits(source));
                    assert_eq!(
                        vm.heap().sequence_get(source, 0).unwrap().as_int(),
                        Some(len as i64 - 1)
                    );
                    assert_eq!(
                        vm.heap().sequence_get(source, len - 1).unwrap().as_int(),
                        Some(0)
                    );
                    break;
                }
                other => panic!("{other:?}"),
            }
        }
        let chunks = vm.heap().stats().chunk_allocations - before;
        assert_eq!(chunks, Values::chunks_for_len(len.into()));
        println!("reverse len={len} work={work} chunks={chunks}");
        costs.push(work);
    }
    assert_eq!(costs[1] - costs[0], 6 * (4096 - 1024));
    assert_eq!(costs[2] - costs[1], 6 * (16384 - 4096));
}

#[test]
fn collection_copy_cost_is_linear_and_allocates_only_payload_chunks() {
    for table in [false, true] {
        let mut measurements = vec![];
        for len in [1024, 4096, 16384] {
            let mut vm = load(collection_copy(table));
            let source = copy_source(&mut vm, table, len);
            let task = vm
                .start(FunctionId(0), &[source], RootOwner(9), Context::Ordinary)
                .unwrap();
            let before = vm.heap().stats().chunk_allocations;
            let now = std::time::Instant::now();
            let mut work = 0;
            loop {
                let outcome = vm.run_slice(task, SliceBudget::default(), || 0).unwrap();
                work += vm.stats().usage.work;
                match outcome {
                    RunOutcome::BudgetYield(_) => {}
                    RunOutcome::Returned(delivery) => {
                        let copy = vm.delivery_value(delivery).unwrap();
                        assert!(!copy.same_bits(source));
                        assert_eq!(vm.heap().info(copy).unwrap().len, len);
                        for index in 0..len {
                            if table {
                                let (key, value) = vm.heap().table_entry(copy, index).unwrap();
                                assert_eq!(key.as_int(), Some(index as i64));
                                assert_eq!(value.as_int(), Some(index as i64));
                            } else {
                                assert_eq!(
                                    vm.heap().sequence_get(copy, index).unwrap().as_int(),
                                    Some(index as i64)
                                );
                            }
                        }
                        break;
                    }
                    other => panic!("{other:?}"),
                }
            }
            let chunks = vm.heap().stats().chunk_allocations - before;
            assert_eq!(
                chunks,
                Values::chunks_for_len(u64::from(len) * if table { 2 } else { 1 })
            );
            println!(
                "collection_copy table={table} len={len} work={work} chunks={chunks} elapsed={:?}",
                now.elapsed()
            );
            measurements.push(work);
        }
        assert_eq!(measurements[1] - measurements[0], 2 * (4096 - 1024));
        assert_eq!(measurements[2] - measurements[1], 2 * (16384 - 4096));
    }
}

#[test]
fn collection_spread_detects_source_and_destination_mutation_and_cancels() {
    for mode in 0..3 {
        let table = mode != 0;
        for action in 0..3 {
            let mut vm = load(collection_copy(table));
            let source = copy_source(&mut vm, table, 130);
            let task = vm
                .start(FunctionId(0), &[source], RootOwner(9), Context::Ordinary)
                .unwrap();
            let builder = loop {
                assert!(matches!(tick(&mut vm, task), RunOutcome::BudgetYield(_)));
                let Frame::Script(s) = *vm.active.frames.data.last().unwrap() else {
                    panic!()
                };
                if mode == 2
                    && s.pc == test_pc(&vm, s.code.function.0, 1)
                    && matches!(s.phase, ScriptPhase::Body)
                {
                    // Force the merge path and suspend inside its key scan.
                    let builder = vm.value(s.regs, 1).unwrap();
                    vm.heap_mut()
                        .table_store(builder, 0, Value::int(-1), Value::NULL)
                        .unwrap();
                }
                if matches!(
                    s.phase,
                    ScriptPhase::Spread(_) | ScriptPhase::TableSpread(_)
                ) || (mode == 2 && s.table_work.is_some())
                {
                    let builder = vm.value(s.env, 1).unwrap();
                    if vm.heap().info(builder).unwrap().len > 0 {
                        break builder;
                    }
                }
            };
            if action == 2 {
                vm.cancel(RootOwner(9)).unwrap();
                loop {
                    match vm.run_slice(task, SliceBudget::default(), || 0) {
                        Ok(RunOutcome::BudgetYield(_)) => gc(&mut vm),
                        Err(VmError::Idle) => break,
                        other => panic!("cancel: {other:?}"),
                    }
                }
                assert_eq!(vm.stats().frames, 0);
            } else {
                let target = if action == 0 { source } else { builder };
                if table {
                    vm.heap_mut()
                        .table_store(target, 0, Value::int(0), Value::NULL)
                        .unwrap();
                } else {
                    vm.heap_mut().set_field(target, 0, Value::NULL).unwrap();
                }
                fault(&mut vm, task, FaultKind::ConcurrentModification);
                assert_eq!(vm.stats().frames, 0);
            }
            while vm.heap().phase() != GcPhase::Idle {
                gc(&mut vm);
            }
            vm.begin_collection().unwrap();
            while vm.heap().phase() != GcPhase::Idle {
                gc(&mut vm);
            }
            assert!(
                vm.heap().info(builder).is_err(),
                "partial copy must not escape"
            );
        }
    }
}

#[test]
fn collection_self_spread_uses_the_initial_sequence_or_existing_table_slots() {
    for table in [false, true] {
        let mut p = collection_copy(table);
        p.functions[0].code = vec![
            op(
                if table {
                    Opcode::TableSpread
                } else {
                    Opcode::ArraySpread
                },
                0,
                0,
                0,
            ),
            op(Opcode::Return, 0, 0, 0),
        ];
        p.functions[0].source_map.clear();
        let mut vm = load(p);
        let source = copy_source(&mut vm, table, 3);
        let task = vm
            .start(FunctionId(0), &[source], RootOwner(9), Context::Ordinary)
            .unwrap();
        let result = returned(&mut vm, task);
        assert!(result.same_bits(source));
        assert_eq!(
            vm.heap().info(result).unwrap().len,
            if table { 3 } else { 6 }
        );
        if !table {
            for index in 0..6 {
                assert_eq!(
                    vm.heap().sequence_get(result, index).unwrap().as_int(),
                    Some((index % 3) as i64)
                );
            }
        }
    }
}

#[test]
fn table_merge_scans_only_the_original_prefix_and_retains_overwrite_order() {
    let mut measurements = vec![];
    for len in [1024, 4096, 16384] {
        let mut p = collection_copy(true);
        let name = SymbolId(p.symbols.len() as u32);
        p.symbols.push("target".into());
        p.signatures[0].parameters.push(Parameter {
            name,
            ty: TypeId(0),
            mutable: false,
            default: None,
        });
        p.functions[0].code.remove(0);
        p.functions[0].source_map.clear();
        let mut vm = load(p);
        let source = copy_source(&mut vm, true, len);
        let target = vm.heap_mut().alloc_table().unwrap();
        // One overlapping key and one prefix-only key. Both keep their order.
        vm.heap_mut()
            .table_store(target, 0, Value::int((len - 1) as i64), Value::NULL)
            .unwrap();
        vm.heap_mut()
            .table_store(target, 1, Value::int(-1), Value::NULL)
            .unwrap();
        let task = vm
            .start(
                FunctionId(0),
                &[source, target],
                RootOwner(9),
                Context::Ordinary,
            )
            .unwrap();
        let mut work = 0;
        loop {
            let outcome = vm.run_slice(task, SliceBudget::default(), || 0).unwrap();
            work += vm.stats().usage.work;
            match outcome {
                RunOutcome::BudgetYield(_) => {}
                RunOutcome::Returned(delivery) => {
                    assert!(vm.delivery_value(delivery).unwrap().same_bits(target));
                    break;
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(vm.heap().table_len(target).unwrap(), len + 1);
        assert_eq!(
            vm.heap().table_entry(target, 0).unwrap().1.as_int(),
            Some((len - 1) as i64)
        );
        assert_eq!(
            vm.heap().table_entry(target, 1).unwrap().0.as_int(),
            Some(-1)
        );
        for index in 0..len - 1 {
            let (key, value) = vm.heap().table_entry(target, index + 2).unwrap();
            assert_eq!(key.as_int(), Some(index as i64));
            assert_eq!(value.as_int(), Some(index as i64));
        }
        println!("table_merge prefix=2 len={len} work={work}");
        measurements.push(work);
    }
    assert_eq!(
        measurements[2] - measurements[1],
        4 * (measurements[1] - measurements[0])
    );
}
#[test]
fn packed_pair_barrier_failure_keeps_payload_versions_and_charge_unchanged() {
    let mut heap = Heap::new(SessionEpoch(91), HeapLimits::default()).unwrap();
    let mut foreign = Heap::new(SessionEpoch(91), HeapLimits::default()).unwrap();
    let packed = heap.alloc_array(&[Value::NULL; 64]).unwrap();
    let invalid = foreign.alloc_array(&[]).unwrap();
    let before = heap.info(packed).unwrap();
    let bytes = heap.stats().charged_bytes;
    assert_eq!(
        heap.packed_pair_push(packed, Value::int(-1), invalid),
        Err(HeapError::WrongHeap)
    );
    let after = heap.info(packed).unwrap();
    assert_eq!(after.len, before.len);
    assert_eq!(after.content_version, before.content_version);
    assert_eq!(after.structure_version, before.structure_version);
    assert_eq!(heap.stats().charged_bytes, bytes);
}
