//! Compare call plans with the original successful/error preparation sequence.
use super::*;
use crate::class::{InstanceSlot, State};
use crate::snapshot::{SnapshotBudget, SnapshotRegistry, VmSnapshot};

fn loaded(case: u8, encoded: bool, reference: bool) -> (Vm, TaskId, Value) {
    let mut p = fixtures::helper_native();
    p.signatures[0].parameters[0].ty = TypeId(0);
    p.constants.push(Const::Int(42));
    p.functions[0].register_count = 3;
    p.functions[0].code = vec![
        Instruction::new(
            if case == 6 {
                Opcode::CallNoWait
            } else {
                Opcode::Call
            },
            2,
            0,
            0,
        ),
        Instruction::new(Opcode::Return, 2, 0, 0),
    ];
    p.functions[0].source_map.clear();
    p.functions[0].descriptors = vec![Descriptor::Call(CallDesc {
        contract: None,
        target: CallTarget::Register(Reg(0)),
        arguments: if case == 8 {
            Arguments::Packed(Reg(1))
        } else {
            Arguments::Window(RegWindow {
                start: Reg(1),
                len: u32::from(matches!(case, 0 | 4 | 6 | 7)),
            })
        },
        names: vec![],
    })];
    p.functions[1].bindings[0].ty = TypeId(0);
    p.functions[1].code = vec![
        Instruction::new(Opcode::LoadConst, 1, 3, 0),
        Instruction::new(Opcode::Return, 1, 0, 0),
    ];
    p.functions[1].descriptors.clear();
    p.functions[1].source_map.clear();
    let mut native = p.functions[1].clone();
    native.kind = FunctionKind::Native(NativeId(0));
    native.register_count = 0;
    native.bindings.clear();
    native.code.clear();
    p.functions.push(native);
    let mut support = review_tests::echo_registry();
    support.natives[0].signature.parameters[0].ty = TypeId(0);
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
    let mut vm = Vm::load(program, support, VmLimits::default(), SessionEpoch(92)).unwrap();
    vm.work_probe.general_callees = reference;
    let function = if case == 4 { 2 } else { 1 };
    let closure = vm
        .heap
        .alloc_record(
            ObjectKind::Closure,
            &[Value::int(function), Value::int(1), Value::int(0)],
        )
        .unwrap();
    let receiver = if matches!(case, 1 | 5) {
        vm.heap
            .alloc_record(
                ObjectKind::Instance,
                &[
                    Value::NULL,
                    Value::NULL,
                    Value::int(if case == 5 {
                        State::Failed
                    } else {
                        State::Ready
                    } as i64),
                    Value::NULL,
                ],
            )
            .unwrap()
    } else {
        let fields = vm.heap.alloc_table().unwrap();
        vm.heap.alloc_host(fields, &[], None).unwrap()
    };
    let target = match case {
        1 | 5 => vm
            .heap
            .alloc_record(ObjectKind::BoundScript, &[closure, receiver, Value::NULL])
            .unwrap(),
        2 => vm.bind_native(receiver, FunctionId(2)).unwrap(),
        3 => {
            vm.bind_callable(receiver, FunctionId(2)).unwrap();
            receiver
        }
        7 => Value::NULL,
        _ => closure,
    };
    let task = vm
        .start(FunctionId(0), &[], RootOwner(1), Context::Ordinary)
        .unwrap();
    let Frame::Script(mut frame) = *vm.active.frames.data.last().unwrap() else {
        unreachable!()
    };
    frame.phase = ScriptPhase::Body;
    vm.put(frame.regs, 0, target).unwrap();
    let argument = if case == 8 {
        vm.heap
            .alloc_array(&[Value::int(-1), Value::int(41)])
            .unwrap()
    } else {
        Value::int(41)
    };
    vm.put(frame.regs, 1, argument).unwrap();
    *vm.active.frames.data.last_mut().unwrap() = Frame::Script(frame);
    vm.program.package().functions[0]
        .code
        .fetch_budgeted(0, &mut vm.code_window, &mut |_, _| true)
        .unwrap()
        .unwrap();
    (vm, task, receiver)
}

#[derive(Debug, PartialEq)]
struct Observation {
    usage: SliceUsage,
    frames: Vec<u8>,
    values: Vec<u8>,
    clocks: u32,
}

fn trace(
    case: u8,
    encoded: bool,
    reference: bool,
    work: u64,
    deadline: bool,
    quota: bool,
) -> Vec<Observation> {
    let (mut vm, task, _) = loaded(case, encoded, reference);
    if quota {
        vm.limits.max_frames = 1;
    }
    let mut budget = SliceBudget {
        instructions: 1,
        work,
        allocation_bytes: 0,
        deadline_ns: deadline.then_some(1),
    };
    let mut trace = Vec::new();
    for _ in 0..10000 {
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
                let ranges = match frame {
                    Frame::Script(s) => [s.regs, s.env, s.args, s.bound],
                    Frame::Native(n) => [n.roots, n.args, n.bound, n.roots],
                };
                ranges.map(|range| {
                    (0..vm.slots.len(range).unwrap())
                        .map(|i| vm.slots.read(at(range, i)).ok())
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();
        trace.push(Observation {
            usage: vm.last_usage,
            frames: postcard::to_allocvec(&vm.active.frames.data).unwrap(),
            values: postcard::to_allocvec(&values).unwrap(),
            clocks,
        });
        match outcome {
            RunOutcome::BudgetYield(usage) => {
                budget.work = work.max(usage.required_work);
                budget.allocation_bytes = usage.required_allocation;
                review_tests::gc(&mut vm);
            }
            RunOutcome::HostCall(ticket) => {
                assert!(matches!(case, 2..=4));
                vm.respond(ticket, HostResponse::Ready(Value::int(42)))
                    .unwrap();
            }
            RunOutcome::Returned(delivery) => {
                assert!(!quota && case != 5 && case != 7);
                assert_eq!(vm.delivery_value(delivery).unwrap().as_int(), Some(42));
                return trace;
            }
            RunOutcome::Fault(delivery) => {
                let expected = match case {
                    5 => FaultKind::ConstructionFailed,
                    7 => FaultKind::Type,
                    _ if quota => FaultKind::Resource,
                    _ => panic!("unexpected call fault"),
                };
                assert_eq!(vm.delivery_fault(delivery).unwrap().unwrap().kind, expected);
                return trace;
            }
            other => panic!("call outcome {other:?}"),
        }
    }
    panic!("call did not progress");
}

#[test]
fn callee_plans_preserve_each_budget_root_clock_native_and_fault_boundary() {
    for encoded in [false, true] {
        for case in 0..=8 {
            for work in [1, 7, 31, 64, 4096] {
                for deadline in [false, true] {
                    for quota in [false, true] {
                        assert_eq!(
                            trace(case, encoded, false, work, deadline, quota),
                            trace(case, encoded, true, work, deadline, quota),
                            "case={case} encoded={encoded} work={work} deadline={deadline} quota={quota}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn only_valid_non_initializer_calls_prepare_and_quotas_precede_receiver_errors() {
    let (mut vm, _, _) = loaded(1, true, false);
    let Frame::Script(frame) = *vm.active.frames.data.last().unwrap() else {
        unreachable!()
    };
    let mut plan = InstructionPlan::None;
    vm.instruction_cost(&frame, Opcode::Call, 2, 0, 0, &mut plan)
        .unwrap();
    assert!(matches!(plan, InstructionPlan::Callee(_)));
    vm.init_symbol = Some(vm.program.package().functions[1].name);
    assert!(
        vm.prepare_callee(
            vm.value(frame.regs, 0).unwrap(),
            vm.read_callee(vm.value(frame.regs, 0).unwrap()).unwrap(),
        )
        .is_none()
    );
    // Failed speculative decoding does not replace the original admission error.
    vm.limits.max_frames = 1;
    assert!(matches!(
        vm.push_script(
            FunctionId(2),
            Some(Value::NULL),
            ArgSource::None,
            0,
            frame.owner,
            frame.context,
            frame.effects,
            frame.ret
        ),
        Err(VmError::Limit)
    ));
    vm.limits.max_frames = 10;
    vm.limits.max_arguments = 0;
    assert!(matches!(
        vm.push_script(
            FunctionId(2),
            Some(Value::NULL),
            ArgSource::None,
            1,
            frame.owner,
            frame.context,
            frame.effects,
            frame.ret
        ),
        Err(VmError::Limit)
    ));
}

#[test]
fn budget_and_snapshot_retry_recheck_receiver_state_before_execution() {
    for restore in [false, true] {
        let (mut vm, task, receiver) = loaded(1, true, false);
        let outcome = vm
            .run_slice(
                task,
                SliceBudget {
                    instructions: 1,
                    work: 1,
                    allocation_bytes: 0,
                    deadline_ns: None,
                },
                || 0,
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::BudgetYield(usage) if usage.instructions == 0));
        let (mut vm, task, receiver) = if restore {
            let registry = SnapshotRegistry::default();
            let mut capture = vm.snapshot_begin(&registry).unwrap();
            while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
            let image = VmSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
            // The fixture's native parameter uses Any instead of the stock Int.
            let mut support = review_tests::echo_registry();
            support.natives[0].signature.parameters[0].ty = TypeId(0);
            let mut restoring = Vm::restore_begin(
                &image,
                vm.program().clone(),
                support,
                vm.limits,
                SessionEpoch(93),
                &registry,
            )
            .unwrap();
            while !restoring
                .step(SnapshotBudget { units: 1 })
                .unwrap()
                .complete
            {}
            let (restored, map) = restoring.finish().unwrap();
            let Frame::Script(frame) = *restored.active.frames.data.last().unwrap() else {
                unreachable!()
            };
            let closure = restored.value(frame.regs, 0).unwrap();
            let receiver = restored.heap.get_field(closure, 1).unwrap();
            (restored, map.task(task).unwrap(), receiver)
        } else {
            (vm, task, receiver)
        };
        vm.heap
            .set_field(
                receiver,
                InstanceSlot::State as u32,
                Value::int(State::Failed as i64),
            )
            .unwrap();
        loop {
            match vm.run_slice(task, SliceBudget::default(), || 0).unwrap() {
                RunOutcome::BudgetYield(_) => {}
                RunOutcome::Fault(delivery) => {
                    assert_eq!(
                        vm.delivery_fault(delivery).unwrap().unwrap().kind,
                        FaultKind::ConstructionFailed
                    );
                    break;
                }
                other => panic!("stale prepared call after retry: {other:?}"),
            }
        }
    }
}

#[test]
fn cancellation_before_retry_releases_the_call_without_entering_its_callee() {
    for reference in [false, true] {
        for hard in [false, true] {
            let (mut vm, task, _) = loaded(2, true, reference);
            let outcome = vm
                .run_slice(
                    task,
                    SliceBudget {
                        instructions: 1,
                        work: 1,
                        allocation_bytes: 0,
                        deadline_ns: None,
                    },
                    || 0,
                )
                .unwrap();
            assert!(matches!(outcome, RunOutcome::BudgetYield(usage) if usage.instructions == 0));
            let entries = vm.native_entries;
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
                    other => panic!("cancelled Call outcome {other:?}"),
                }
            }
            assert_eq!(vm.native_entries, entries);
        }
    }
}

// The original accessor sequence remains independent of ObjectRead decoding.
fn original_function(vm: &Vm, mut closure: Value) -> Result<FunctionId, VmError> {
    if vm.heap.info(closure)?.kind == ObjectKind::BoundScript {
        let receiver = vm.heap.get_field(closure, 1)?;
        if !receiver.same_bits(Value::NULL) {
            let slot = match vm
                .heap
                .info(receiver)
                .map_err(|_| HeapError::ConstructionFailed)?
                .kind
            {
                ObjectKind::Class => crate::class::ClassSlot::State as u32,
                ObjectKind::Instance => InstanceSlot::State as u32,
                _ => return Err(HeapError::ConstructionFailed.into()),
            };
            if vm
                .heap
                .get_field(receiver, slot)
                .map_err(|_| HeapError::ConstructionFailed)?
                .as_int()
                == Some(State::Failed as i64)
            {
                return Err(HeapError::ConstructionFailed.into());
            }
        }
        closure = vm.heap.get_field(closure, 0)?;
    }
    let info = vm.heap.info(closure)?;
    if info.kind == ObjectKind::Host {
        return Ok(vm.heap.host_call(closure)?);
    }
    let bound = info.kind == ObjectKind::BoundNative;
    if info.kind != ObjectKind::Closure && !bound {
        return Err(VmError::Contract("callee is not a closure"));
    }
    let fi = vm
        .heap
        .get_field(closure, 0)?
        .as_int()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or(VmError::Contract("invalid closure code"))?;
    let version = vm
        .heap
        .get_field(closure, 1)?
        .as_int()
        .ok_or(VmError::Contract("invalid closure version"))? as u64;
    let f = vm
        .program
        .package()
        .functions
        .get(fi as usize)
        .ok_or(VmError::Contract("foreign closure code"))?;
    if version != vm.program.package().content_version
        || vm.heap.get_field(closure, 2)?.as_int() != Some(f.module.0 as i64)
        || info.len != f.captures.len() as u32 + 3 + u32::from(bound)
        || (bound && !matches!(f.kind, FunctionKind::Native(_)))
    {
        return Err(VmError::Contract("foreign closure environment/version"));
    }
    Ok(FunctionId(fi))
}

#[test]
fn borrowed_callee_decode_matches_original_errors_and_receiver_values() {
    fn compare(vm: &Vm, value: Value) {
        assert_eq!(
            format!("{:?}", vm.closure_function(value)),
            format!("{:?}", original_function(vm, value))
        );
        if let Ok(read) = vm.read_callee(value) {
            let function = read.function;
            let expected = if matches!(
                vm.program.package().functions[function.index()].kind,
                FunctionKind::Native(_)
            ) {
                match vm.heap.info(value).unwrap().kind {
                    ObjectKind::BoundNative => Some(vm.heap.get_field(value, 3).unwrap()),
                    ObjectKind::Host => {
                        vm.heap.host_call(value).unwrap();
                        Some(value)
                    }
                    _ => None,
                }
            } else if vm.heap.info(value).unwrap().kind == ObjectKind::BoundScript {
                let receiver = vm.heap.get_field(value, 1).unwrap();
                (!receiver.same_bits(Value::NULL)).then_some(receiver)
            } else {
                None
            };
            let actual = vm.prepare_callee(value, read).unwrap().receiver;
            assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
        }
    }
    for case in 0..=8 {
        let (vm, _, _) = loaded(case, true, false);
        let Frame::Script(frame) = vm.active.frames.data.last().unwrap() else {
            unreachable!()
        };
        compare(&vm, vm.value(frame.regs, 0).unwrap());
    }
    let (mut vm, _, _) = loaded(0, false, false);
    for kind in [ObjectKind::Closure, ObjectKind::BoundNative] {
        for fields in [
            vec![],
            vec![Value::NULL],
            vec![Value::int(-1), Value::int(1)],
            vec![Value::int(1), Value::NULL],
            vec![Value::int(99), Value::int(1)],
            vec![Value::int(1), Value::int(2)],
            vec![Value::int(1), Value::int(1), Value::int(8)],
            vec![Value::int(1), Value::int(1), Value::int(0)],
            vec![Value::int(2), Value::int(1), Value::int(0), Value::NULL],
        ] {
            let value = vm.heap.alloc_record(kind, &fields).unwrap();
            compare(&vm, value);
        }
    }
    let wrapper = vm
        .heap
        .alloc_record(ObjectKind::BoundScript, &[Value::NULL, Value::NULL])
        .unwrap();
    assert!(vm.script_closure(wrapper).unwrap().same_bits(Value::NULL));
    compare(&vm, wrapper);
    let (_, bits) = wrapper.object_parts().unwrap();
    compare(&vm, Value::object(vm.heap.id(), bits ^ (1 << 32)));
    compare(&vm, Value::object(crate::HeapId(0), bits));
}
