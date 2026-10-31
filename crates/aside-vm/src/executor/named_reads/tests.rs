//! Named reads retain the original table/equality cursor at every safe point.
use super::*;
use crate::snapshot::{SnapshotBudget, SnapshotRegistry, VmSnapshot};

fn load(host: bool, model: bool, long: bool, encoded: bool, reference: bool) -> (Vm, Value, Value) {
    let mut p = fixtures::helper_native();
    p.natives.clear();
    p.capabilities.clear();
    p.functions.truncate(1);
    p.signatures.truncate(1);
    p.signatures[0].parameters[0].ty = TypeId(0);
    p.signatures[0].return_type = TypeId(0);
    let symbol = SymbolId(p.symbols.len() as u32);
    let name = if long {
        "position".repeat(20)
    } else {
        "position".into()
    };
    p.symbols.push(name.clone());
    let f = &mut p.functions[0];
    f.signature = SignatureId(0);
    f.register_count = 2;
    f.bindings.clear();
    f.descriptors.clear();
    f.source_map.clear();
    f.code = vec![
        Instruction::new(Opcode::MemberGet, 1, 0, symbol.0),
        Instruction::new(Opcode::Return, 1, 0, 0),
    ];
    let program = verify(p, Limits::default()).unwrap();
    let program = if encoded {
        VerifiedProgram::decode(
            program.encode(Limits::default()).unwrap().as_bytes(),
            Limits::default(),
        )
        .unwrap()
    } else {
        program
    };
    let mut vm = Vm::load(
        program,
        RuntimeSupport::default(),
        VmLimits::default(),
        SessionEpoch(114),
    )
    .unwrap();
    vm.work_probe.single_named_read_step = reference;
    let table = vm.heap.alloc_table().unwrap();
    let bytes = vm.heap.alloc_bytes(b"position").unwrap();
    let tuple = vm.heap.alloc_tuple(&[Value::float(f64::NAN)]).unwrap();
    for (index, key) in [Value::NULL, Value::int(1), bytes, tuple]
        .into_iter()
        .enumerate()
    {
        vm.heap
            .table_store(table, index as u32, key, Value::int(-1))
            .unwrap();
    }
    let prefix = vm.heap.alloc_string(&(name.clone() + "_other")).unwrap();
    vm.heap
        .table_store(table, 4, prefix, Value::int(-1))
        .unwrap();
    let key = vm.heap.alloc_string(&name).unwrap();
    vm.heap.table_store(table, 5, key, Value::int(42)).unwrap();
    let target = if host {
        let host = vm.heap.alloc_host(table, &[], None).unwrap();
        vm.heap.observe_host_properties(host, 6, 0).unwrap();
        host
    } else {
        if model {
            vm.heap.observe_model_object(table).unwrap();
        }
        table
    };
    vm.heap
        .add_root(RootOwner(8), RootKind::Host, target)
        .unwrap();
    (vm, target, table)
}

fn run(vm: &mut Vm, target: Value) -> Value {
    let task = vm
        .start(FunctionId(0), &[target], RootOwner(1), Context::Ordinary)
        .unwrap();
    loop {
        match vm.run_slice(task, SliceBudget::default(), || 0).unwrap() {
            RunOutcome::BudgetYield(_) => {}
            RunOutcome::Returned(delivery) => {
                let value = vm.delivery_value(delivery).unwrap();
                vm.discard_delivery(delivery).unwrap();
                return value;
            }
            other => panic!("named read {other:?}"),
        }
    }
}

#[derive(Debug, PartialEq)]
struct Observation {
    usage: SliceUsage,
    frames: Vec<u8>,
    roots: Vec<u8>,
    equality: Vec<u32>,
    clocks: u32,
}

#[allow(clippy::too_many_arguments)]
fn trace(
    host: bool,
    model: bool,
    long: bool,
    encoded: bool,
    reference: bool,
    warm: bool,
    work: u64,
    deadline: bool,
) -> Vec<Observation> {
    let (mut vm, target, _) = load(host, model, long, encoded, reference);
    if warm {
        assert_eq!(run(&mut vm, target).as_int(), Some(42));
    }
    let task = vm
        .start(FunctionId(0), &[target], RootOwner(1), Context::Ordinary)
        .unwrap();
    let mut budget = SliceBudget {
        instructions: 1,
        work,
        allocation_bytes: 0,
        deadline_ns: deadline.then_some(1),
    };
    let mut result = Vec::new();
    for _ in 0..20000 {
        let mut clocks = 0;
        let outcome = vm
            .run_slice(task, budget, || {
                let now = u64::from(clocks != 0);
                clocks += 1;
                now
            })
            .unwrap();
        let values = vm
            .active
            .frames
            .data
            .iter()
            .map(|frame| {
                let Frame::Script(frame) = frame else {
                    unreachable!()
                };
                [frame.regs, frame.env, frame.args, frame.bound].map(|range| {
                    (0..vm.slots.len(range).unwrap())
                        .map(|i| vm.slots.read(at(range, i)).ok())
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();
        result.push(Observation {
            usage: vm.last_usage,
            frames: postcard::to_allocvec(&vm.active.frames.data).unwrap(),
            roots: postcard::to_allocvec(&values).unwrap(),
            equality: vm.active.equality_cursors.data.clone(),
            clocks,
        });
        match outcome {
            RunOutcome::BudgetYield(usage) => {
                budget.work = work.max(usage.required_work);
                budget.allocation_bytes = usage.required_allocation;
                review_tests::gc(&mut vm);
            }
            RunOutcome::Returned(delivery) => {
                assert_eq!(vm.delivery_value(delivery).unwrap().as_int(), Some(42));
                return result;
            }
            other => panic!("named trace {other:?}"),
        }
    }
    panic!("named read did not progress");
}

#[test]
fn named_blocks_preserve_cold_warm_mixed_key_root_work_and_clock_boundaries() {
    for host in [false, true] {
        for model in [false, true] {
            for long in [false, true] {
                for encoded in [false, true] {
                    for warm in [false, true] {
                        for work in [1, 3, 7, 31, 32, 128, 4096] {
                            for deadline in [false, true] {
                                assert_eq!(
                                    trace(host, model, long, encoded, false, warm, work, deadline),
                                    trace(host, model, long, encoded, true, warm, work, deadline),
                                    "host={host} model={model} long={long} encoded={encoded} warm={warm} work={work} deadline={deadline}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn snapshots_keep_the_original_partial_equality_cursor() {
    for reference in [false, true] {
        let (mut vm, target, _) = load(false, false, true, true, reference);
        let task = vm
            .start(FunctionId(0), &[target], RootOwner(1), Context::Ordinary)
            .unwrap();
        let mut budget = SliceBudget {
            instructions: 1,
            work: 1,
            allocation_bytes: 0,
            deadline_ns: None,
        };
        loop {
            let RunOutcome::BudgetYield(usage) = vm.run_slice(task, budget, || 0).unwrap() else {
                panic!("must reach equality");
            };
            if vm
                .active
                .equality_cursors
                .data
                .last()
                .is_some_and(|offset| *offset > 5)
            {
                break;
            }
            budget.work = usage.required_work.max(1);
            budget.allocation_bytes = usage.required_allocation;
        }
        let cursors = vm.active.equality_cursors.data.clone();
        let registry = SnapshotRegistry::default();
        let mut capture = vm.snapshot_begin(&registry).unwrap();
        while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let image = VmSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
        let mut restoring = Vm::restore_begin(
            &image,
            vm.program().clone(),
            RuntimeSupport::default(),
            vm.limits,
            SessionEpoch(115),
            &registry,
        )
        .unwrap();
        while !restoring
            .step(SnapshotBudget { units: 1 })
            .unwrap()
            .complete
        {}
        let (mut restored, map) = restoring.finish().unwrap();
        assert_eq!(restored.active.equality_cursors.data, cursors);
        loop {
            match restored
                .run_slice(map.task(task).unwrap(), SliceBudget::default(), || 0)
                .unwrap()
            {
                RunOutcome::BudgetYield(_) => {}
                RunOutcome::Returned(delivery) => {
                    assert_eq!(
                        restored.delivery_value(delivery).unwrap().as_int(),
                        Some(42)
                    );
                    break;
                }
                other => panic!("restored named lookup {other:?}"),
            }
        }
    }
}

#[test]
fn resumed_points_read_current_values_and_reject_changed_structure_or_retired_hosts() {
    for reference in [false, true] {
        for mutation in 0..3 {
            let host = mutation == 2;
            let (mut vm, target, table) = load(host, true, false, true, reference);
            assert_eq!(run(&mut vm, target).as_int(), Some(42));
            let task = vm
                .start(FunctionId(0), &[target], RootOwner(1), Context::Ordinary)
                .unwrap();
            let mut budget = SliceBudget {
                instructions: 1,
                work: 1,
                allocation_bytes: 0,
                deadline_ns: None,
            };
            loop {
                let RunOutcome::BudgetYield(usage) = vm.run_slice(task, budget, || 0).unwrap()
                else {
                    panic!("must stop before commit");
                };
                if matches!(vm.active.frames.data.last(), Some(Frame::Script(s)) if matches!(s.phase, ScriptPhase::TableScan) && s.table_work.is_some_and(|work| work.matched()))
                {
                    break;
                }
                budget.work = usage.required_work.max(1);
                budget.allocation_bytes = usage.required_allocation;
            }
            match mutation {
                0 => {
                    let key = vm.heap.table_entry(table, 5).unwrap().0;
                    vm.heap.table_store(table, 5, key, Value::int(99)).unwrap();
                }
                1 => {
                    let key = vm.heap.alloc_string("new field").unwrap();
                    vm.heap.table_store(table, 6, key, Value::NULL).unwrap();
                }
                2 => vm.heap.retire_host(target).unwrap(),
                _ => unreachable!(),
            }
            loop {
                match vm.run_slice(task, SliceBudget::default(), || 0).unwrap() {
                    RunOutcome::BudgetYield(_) => {}
                    RunOutcome::Returned(delivery) if mutation == 0 => {
                        assert_eq!(vm.delivery_value(delivery).unwrap().as_int(), Some(99));
                        break;
                    }
                    RunOutcome::Fault(delivery) if mutation != 0 => {
                        assert_eq!(
                            vm.delivery_fault(delivery).unwrap().unwrap().kind,
                            if mutation == 1 {
                                FaultKind::ConcurrentModification
                            } else {
                                FaultKind::InvalidState
                            }
                        );
                        break;
                    }
                    other => panic!("resumed named point {mutation}: {other:?}"),
                }
            }
        }
    }
}

#[test]
fn cancellation_before_a_named_cursor_retry_never_commits_the_read() {
    for reference in [false, true] {
        for hard in [false, true] {
            let (mut vm, target, _) = load(false, false, true, true, reference);
            let task = vm
                .start(FunctionId(0), &[target], RootOwner(1), Context::Ordinary)
                .unwrap();
            let mut budget = SliceBudget {
                instructions: 1,
                work: 1,
                allocation_bytes: 0,
                deadline_ns: None,
            };
            loop {
                let RunOutcome::BudgetYield(usage) = vm.run_slice(task, budget, || 0).unwrap()
                else {
                    panic!("must stop during lookup");
                };
                if !vm.active.equality_cursors.data.is_empty() {
                    break;
                }
                budget.work = usage.required_work.max(1);
                budget.allocation_bytes = usage.required_allocation;
            }
            if hard {
                vm.cancel_task(task).unwrap();
            } else {
                vm.cancel_cooperatively(task).unwrap();
            }
            loop {
                match vm.run_slice(task, SliceBudget::default(), || 0) {
                    Ok(RunOutcome::BudgetYield(_)) => {}
                    Err(VmError::Idle) if hard => break,
                    Ok(RunOutcome::Fault(delivery)) if !hard => {
                        assert_eq!(
                            vm.delivery_fault(delivery).unwrap().unwrap().kind,
                            FaultKind::Cancelled
                        );
                        vm.discard_delivery(delivery).unwrap();
                        break;
                    }
                    other => panic!("cancelled named lookup {other:?}"),
                }
            }
            assert!(vm.active.equality_cursors.data.is_empty());
            assert_eq!(run(&mut vm, target).as_int(), Some(42));
        }
    }
}

#[cfg(feature = "execution-counts")]
#[test]
fn named_counts_distinguish_cold_trusted_and_cross_receiver_rejected_hints() {
    let (mut vm, target, _) = load(false, false, false, true, false);
    let before = vm.execution_counts();
    run(&mut vm, target);
    let cold = vm.execution_counts().checked_delta(&before).unwrap();
    assert_eq!(cold.member_get_table_costs, 1);
    assert_eq!(cold.named_table_starts, 1);
    assert_eq!(cold.named_table_hint_absent, 1);
    assert_eq!(cold.named_table_candidates, 6);
    assert!(cold.named_table_equality_steps > 0);
    let before = vm.execution_counts();
    run(&mut vm, target);
    let warm = vm.execution_counts().checked_delta(&before).unwrap();
    assert_eq!(warm.named_table_hint_trusted, 1);
    assert_eq!(warm.named_table_steps, 3);
    assert_eq!(warm.named_table_candidates, 0);
    assert_eq!(warm.named_table_equality_steps, 0);
    let other = vm.heap.alloc_table().unwrap();
    for (index, name) in ["position", "one", "two", "three", "four", "wrongkey"]
        .into_iter()
        .enumerate()
    {
        let key = vm.heap.alloc_string(name).unwrap();
        vm.heap
            .table_store(other, index as u32, key, Value::int(42))
            .unwrap();
    }
    let before = vm.execution_counts();
    run(&mut vm, other);
    let rejected = vm.execution_counts().checked_delta(&before).unwrap();
    assert_eq!(rejected.named_table_hint_rejected, 1);
    assert_eq!(rejected.named_table_candidates, 1);
    assert_eq!(rejected.named_table_starts, 1);
}
