//! The original table cursor is the semantic and small-budget oracle.
use super::*;
use crate::snapshot::{SnapshotBudget, SnapshotRegistry};

const OWNER: RootOwner = RootOwner(121);

fn load(reference: bool, encoded: bool, call: bool) -> (Vm, TaskId, Value, Value, RuntimeSupport) {
    load_reads(reference, encoded, call, 1)
}

fn load_reads(
    reference: bool,
    encoded: bool,
    call: bool,
    reads: usize,
) -> (Vm, TaskId, Value, Value, RuntimeSupport) {
    let mut p = fixtures::helper_native();
    p.signatures[0].parameters[0].ty = TypeId(0);
    p.signatures[0].return_type = TypeId(0);
    p.signatures[1].return_type = TypeId(0);
    let symbol = SymbolId(p.symbols.len() as u32);
    p.symbols.push("sample".into());
    let f = &mut p.functions[0];
    f.register_count = 3;
    f.bindings.clear();
    f.source_map.clear();
    f.descriptors.clear();
    f.code = vec![Instruction::new(Opcode::MemberGet, 1, 0, symbol.0); reads];
    if call {
        f.code.push(Instruction::new(Opcode::Call, 2, 0, 0));
        f.descriptors.push(Descriptor::Call(CallDesc {
            target: CallTarget::Register(Reg(1)),
            arguments: Arguments::Window(RegWindow {
                start: Reg(2),
                len: 0,
            }),
            names: Vec::new(),
            contract: None,
        }));
    }
    f.code.push(Instruction::new(
        Opcode::Return,
        if call { 2 } else { 1 },
        0,
        0,
    ));
    let native = &mut p.functions[1];
    native.kind = FunctionKind::Native(NativeId(0));
    native.name = p.natives[0].symbol;
    native.register_count = 0;
    native.bindings.clear();
    native.descriptors.clear();
    native.source_map.clear();
    native.code.clear();
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
    let mut support = review_tests::echo_registry();
    support.natives[0].signature.parameters[0].ty = TypeId(0);
    support.natives[0].signature.return_type = TypeId(0);
    support.host_types.push(crate::HostTypeSupport {
        name: "test::Curve".into(),
        version: 1,
        bases: Vec::new(),
        methods: vec![crate::HostMethodSupport {
            name: "sample".into(),
            native: "host::echo".into(),
            abi_version: 1,
        }],
    });
    let mut vm = Vm::load(
        program,
        support.clone(),
        VmLimits::default(),
        SessionEpoch(121),
    )
    .unwrap();
    vm.work_probe.single_named_read_step = reference;
    let fields = vm.heap.alloc_table().unwrap();
    let host = vm.heap.alloc_host(fields, &[], None).unwrap();
    let ty = vm.heap.host_type("test::Curve").unwrap();
    vm.heap.set_host_type(host, ty).unwrap();
    let task = start(&mut vm, host);
    (vm, task, host, fields, support)
}

fn start(vm: &mut Vm, host: Value) -> TaskId {
    let task = vm
        .start(FunctionId(0), &[], OWNER, Context::Ordinary)
        .unwrap();
    let Frame::Script(mut frame) = *vm.active.frames.data.last().unwrap() else {
        unreachable!()
    };
    frame.phase = ScriptPhase::Body;
    vm.put(frame.regs, 0, host).unwrap();
    vm.publish_script_frame(vm.active.frames.data.len() - 1, &frame);
    task
}

#[derive(Debug, PartialEq)]
struct ResultObservation {
    work: u64,
    instructions: u64,
    clocks: u64,
    native_entries: u64,
    binding_hits: u64,
    binding_misses: u64,
    fault: Option<FaultKind>,
    value: Option<i64>,
}

fn finish(vm: &mut Vm, task: TaskId, work: u64, deadline: bool) -> (DeliveryId, ResultObservation) {
    let before = vm.stats();
    let mut clock_calls = 0;
    let mut budget = SliceBudget {
        instructions: 1,
        work,
        allocation_bytes: 0,
        deadline_ns: deadline.then_some(1),
    };
    for _ in 0..10000 {
        let mut clocks = 0;
        let result = vm
            .run_slice(task, budget, || {
                clocks += 1;
                u64::from(clocks > 1)
            })
            .unwrap();
        clock_calls += clocks;
        match result {
            RunOutcome::BudgetYield(usage) => {
                budget.work = work.max(usage.required_work);
                budget.allocation_bytes = usage.required_allocation;
                review_tests::gc(vm);
            }
            RunOutcome::HostCall(ticket) => {
                assert_eq!(vm.host_arguments(ticket).unwrap().len(), 1);
                assert_eq!(
                    vm.heap
                        .info(vm.host_arguments(ticket).unwrap()[0])
                        .unwrap()
                        .kind,
                    ObjectKind::Host
                );
                vm.respond(ticket, HostResponse::Ready(Value::int(42)))
                    .unwrap();
            }
            RunOutcome::Returned(delivery) | RunOutcome::Fault(delivery) => {
                let after = vm.stats();
                return (
                    delivery,
                    ResultObservation {
                        work: after.cumulative_work - before.cumulative_work,
                        instructions: after.cumulative_instructions
                            - before.cumulative_instructions,
                        clocks: clock_calls,
                        native_entries: after.native_entries - before.native_entries,
                        binding_hits: after.native_binding_hits - before.native_binding_hits,
                        binding_misses: after.native_binding_misses - before.native_binding_misses,
                        fault: vm.delivery_fault(delivery).unwrap().map(|f| f.kind),
                        value: vm.delivery_value(delivery).ok().and_then(|v| v.as_int()),
                    },
                );
            }
            other => panic!("host method: {other:?}"),
        }
    }
    panic!("host method did not finish");
}

#[test]
fn empty_hosts_keep_work_clock_native_binding_and_small_budget_results() {
    for encoded in [false, true] {
        for work in [1, 2, 3, 7, 31, 64, 1024] {
            for deadline in [false, true] {
                let observations = [false, true].map(|reference| {
                    let (mut vm, task, _, _, _) = load(reference, encoded, true);
                    let (_, observation) = finish(&mut vm, task, work, deadline);
                    assert_eq!(observation.fault, None);
                    assert_eq!(observation.value, Some(42));
                    observation
                });
                assert_eq!(
                    observations[0], observations[1],
                    "encoded={encoded} work={work} deadline={deadline}"
                );
            }
        }
    }
}

#[test]
fn method_values_keep_weak_identity_own_field_priority_and_retired_receiver_rules() {
    for reference in [false, true] {
        let (mut vm, task, host, fields, _) = load(reference, true, false);
        let (first, _) = finish(&mut vm, task, 1024, false);
        let value = vm.delivery_value(first).unwrap();
        assert_eq!(vm.closure_function(value).unwrap(), FunctionId(1));
        assert!(
            vm.callable_receiver(value)
                .unwrap()
                .unwrap()
                .same_bits(host)
        );
        let second_task = start(&mut vm, host);
        let (second, _) = finish(&mut vm, second_task, 1024, false);
        assert!(vm.delivery_value(second).unwrap().same_bits(value));
        let key = vm.heap.alloc_string("sample").unwrap();
        vm.heap.table_store(fields, 0, key, Value::int(7)).unwrap();
        let shadowed_task = start(&mut vm, host);
        let (shadowed, _) = finish(&mut vm, shadowed_task, 1024, false);
        assert_eq!(vm.delivery_value(shadowed).unwrap().as_int(), Some(7));
        vm.heap.retire_host(host).unwrap();
        let retired_task = start(&mut vm, host);
        let (retired, _) = finish(&mut vm, retired_task, 1024, false);
        assert!(vm.delivery_value(retired).unwrap().same_bits(value));
    }
}

#[test]
fn member_binding_precedes_later_mutation_and_restores_before_call() {
    for reference in [false, true] {
        let (mut vm, task, host, fields, support) = load(reference, true, true);
        loop {
            let outcome = vm
                .run_slice(
                    task,
                    SliceBudget {
                        instructions: 1,
                        ..Default::default()
                    },
                    || 0,
                )
                .unwrap();
            assert!(matches!(outcome, RunOutcome::BudgetYield(_)));
            if matches!(vm.active.frames.data.last(), Some(Frame::Script(f)) if f.pc != Pc(0) && matches!(f.phase, ScriptPhase::Body))
            {
                break;
            }
        }
        let key = vm.heap.alloc_string("sample").unwrap();
        vm.heap.table_store(fields, 0, key, Value::int(-1)).unwrap();
        review_tests::collect(&mut vm);
        let registry = SnapshotRegistry::default();
        let mut capture = vm.snapshot_begin(&registry).unwrap();
        while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let image = capture.finish().unwrap();
        let mut restoring = Vm::restore_begin(
            &image,
            vm.program().clone(),
            support,
            vm.limits,
            SessionEpoch(122),
            &registry,
        )
        .unwrap();
        while !restoring
            .step(SnapshotBudget { units: 1 })
            .unwrap()
            .complete
        {}
        let (mut restored, map) = restoring.finish().unwrap();
        let (_, result) = finish(&mut restored, map.task(task).unwrap(), 1024, false);
        assert_eq!(result.value, Some(42));
        assert_eq!(result.fault, None);
        // The original VM also retains the pre-mutation binding and receiver.
        assert!(vm.heap.info(host).is_ok());
    }
}

#[test]
fn allocation_effect_failure_keeps_original_fault_and_work() {
    let observations = [false, true].map(|reference| {
        let (mut vm, task, _, _, _) = load(reference, true, true);
        let Frame::Script(frame) = vm.active.frames.data.last_mut().unwrap() else {
            unreachable!()
        };
        frame.effects = Effects(Effects::ALL.0 & !Effects::ALLOCATE);
        let (_, observation) = finish(&mut vm, task, 1024, false);
        assert_eq!(observation.fault, Some(FaultKind::Effect));
        observation
    });
    assert_eq!(observations[0], observations[1]);
}

#[test]
fn insufficient_admission_keeps_the_versioned_cursor_and_cancellation_roots() {
    for reference in [false, true] {
        for cancel in [false, true] {
            let (mut vm, task, _, fields, _) = load(reference, true, true);
            vm.program.package().functions[0]
                .code
                .fetch_budgeted(0, &mut vm.code_window, &mut |_, _| true)
                .unwrap()
                .unwrap();
            assert!(matches!(
                vm.run_slice(
                    task,
                    SliceBudget {
                        instructions: 1,
                        work: 1,
                        ..Default::default()
                    },
                    || 0
                )
                .unwrap(),
                RunOutcome::BudgetYield(_)
            ));
            assert!(
                matches!(vm.active.frames.data.last(), Some(Frame::Script(f)) if matches!(f.phase, ScriptPhase::TableValidate) && f.table_work.is_some())
            );
            if cancel {
                vm.cancel_task(task).unwrap();
                loop {
                    match vm.run_slice(task, SliceBudget::default(), || 0) {
                        Err(VmError::Idle) => break,
                        Ok(RunOutcome::BudgetYield(_)) => {}
                        other => panic!("cancelling lookup: {other:?}"),
                    }
                }
                assert!(vm.active.frames.data.is_empty());
                assert!(vm.active.equality_cursors.data.is_empty());
            } else {
                let key = vm.heap.alloc_string("sample").unwrap();
                vm.heap.table_store(fields, 0, key, Value::int(7)).unwrap();
                let (_, outcome) = finish(&mut vm, task, 1024, false);
                assert_eq!(outcome.fault, Some(FaultKind::ConcurrentModification));
            }
        }
    }
}

#[test]
fn fused_lookups_stop_at_the_same_soft_clock_boundary() {
    let observations = [false, true].map(|reference| {
        let (mut vm, task, _, _, _) = load_reads(reference, true, false, 96);
        vm.program.package().functions[0]
            .code
            .fetch_budgeted(0, &mut vm.code_window, &mut |_, _| true)
            .unwrap()
            .unwrap();
        let mut clocks = 0;
        let RunOutcome::BudgetYield(usage) = vm
            .run_slice(
                task,
                SliceBudget {
                    instructions: 1000,
                    work: 100_000,
                    deadline_ns: Some(1),
                    ..Default::default()
                },
                || {
                    clocks += 1;
                    u64::from(clocks > 1)
                },
            )
            .unwrap()
        else {
            panic!("clock yield")
        };
        let Frame::Script(frame) = vm.active.frames.data.last().unwrap() else {
            unreachable!()
        };
        (
            usage,
            clocks,
            frame.pc,
            postcard::to_allocvec(&frame.phase).unwrap(),
            vm.native_binding_hits,
            vm.native_binding_misses,
        )
    });
    assert_eq!(observations[0], observations[1]);
}

#[cfg(feature = "execution-counts")]
#[test]
fn completed_empty_lookups_remove_only_the_cursor_and_scratch_root_operations() {
    let observed = [false, true].map(|reference| {
        let (mut vm, task, _, _, _) = load(reference, true, false);
        let before = vm.execution_counts();
        let heap_before = vm.heap.stats();
        let (_, result) = finish(&mut vm, task, 1024, false);
        let counts = vm.execution_counts().checked_delta(&before).unwrap();
        (
            result,
            counts,
            vm.heap.stats().live_objects - heap_before.live_objects,
        )
    });
    let (fast_result, fast, fast_objects) = &observed[0];
    let (slow_result, slow, slow_objects) = &observed[1];
    assert_eq!(fast_result, slow_result);
    assert_eq!(fast.named_table_starts, 0);
    assert_eq!(slow.named_table_starts, 1);
    assert_eq!(fast.named_host_methods, slow.named_host_methods);
    assert_eq!(fast.native_binding_misses, slow.native_binding_misses);
    assert_eq!(fast.slots.open_calls, slow.slots.open_calls);
    assert_eq!(fast.slots.open_values, slow.slots.open_values);
    assert_eq!(fast_objects, slow_objects);
    assert_eq!(fast.slots.write_calls + 4, slow.slots.write_calls);
    assert_eq!(fast.slots.clear_calls + 2, slow.slots.clear_calls);
}
