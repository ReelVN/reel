//! Differential tests against the same resumable binder with plans disabled.
use super::*;
use crate::snapshot::{SnapshotBudget, SnapshotRegistry};

fn fixture(calls: usize) -> (BytecodePackage, RuntimeSupport) {
    let mut p = fixtures::helper_native();
    let mut signature = p.signatures[0].clone();
    signature.parameters = (0..4)
        .map(|i| Parameter {
            name: SymbolId([4, 12, 13, 9][i]),
            ty: TypeId(0),
            mutable: false,
            default: (i == 1).then_some(ParameterDefault::Constant(ConstId(2))),
        })
        .collect();
    let mut support = review_tests::echo_registry();
    support.natives[0].signature = signature.clone();
    p.signatures[0] = signature;
    p.functions.truncate(1);
    let f = &mut p.functions[0];
    f.register_count = 4;
    f.source_map.clear();
    f.code = vec![
        Instruction::new(Opcode::LoadConst, 0, 1, 0),
        Instruction::new(Opcode::LoadConst, 1, 0, 0),
        Instruction::new(Opcode::LoadConst, 2, 2, 0),
    ];
    f.code
        .extend((0..calls).map(|_| Instruction::new(Opcode::Call, 3, 0, 0)));
    f.code.push(Instruction::new(Opcode::Return, 3, 0, 0));
    f.descriptors = vec![Descriptor::Call(CallDesc {
        contract: None,
        target: CallTarget::Native(NativeId(0)),
        arguments: Arguments::Window(RegWindow {
            start: Reg(0),
            len: 3,
        }),
        names: vec![
            NamedArg {
                offset: 1,
                name: SymbolId(9),
            },
            NamedArg {
                offset: 2,
                name: SymbolId(13),
            },
        ],
    })];
    (p, support)
}
fn load(generic: bool, encoded: bool, calls: usize) -> (Vm, RuntimeSupport) {
    let (p, support) = fixture(calls);
    let program = if encoded {
        VerifiedProgram::decode(&encode(&p, Limits::default()).unwrap(), Limits::default()).unwrap()
    } else {
        verify(p, Limits::default()).unwrap()
    };
    let program = if generic {
        program.without_native_call_plans()
    } else {
        program
    };
    (
        Vm::load(
            program,
            support.clone(),
            VmLimits::default(),
            SessionEpoch(1),
        )
        .unwrap(),
        support,
    )
}
fn next(vm: &mut Vm, task: TaskId) -> RunOutcome {
    loop {
        let result = vm
            .run_slice(
                task,
                SliceBudget {
                    instructions: 1,
                    work: vm.last_usage.required_work.max(1),
                    allocation_bytes: vm.last_usage.required_allocation,
                    ..Default::default()
                },
                || 0,
            )
            .unwrap();
        if !matches!(result, RunOutcome::BudgetYield(_)) {
            return result;
        }
    }
}
fn restore(vm: Vm, task: TaskId, support: RuntimeSupport) -> (Vm, TaskId) {
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
    let (vm, map) = restore.finish().unwrap();
    (vm, map.task(task).unwrap())
}
#[test]
fn fixed_binding_preserves_named_order_defaults_null_and_snapshot_at_each_copy() {
    for encoded in [false, true] {
        for stop in 0..=3 {
            let (mut vm, support) = load(false, encoded, 1);
            let mut task = vm
                .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
                .unwrap();
            loop {
                if matches!(vm.active.frames.data.last(), Some(Frame::Native(n)) if matches!(n.phase, NativePhase::PlannedCopy { index, .. } if index == stop))
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
                        || 0
                    )
                    .unwrap(),
                    RunOutcome::BudgetYield(_)
                ));
            }
            review_tests::collect(&mut vm);
            (vm, task) = restore(vm, task, support);
            let RunOutcome::HostCall(ticket) = next(&mut vm, task) else {
                panic!("native")
            };
            let n = vm.ticket(ticket).unwrap();
            assert_eq!(vm.slots.len(n.args).unwrap(), 0);
            assert_eq!(
                vm.host_arguments(ticket)
                    .unwrap()
                    .iter()
                    .map(|v| v.as_int())
                    .collect::<Vec<_>>(),
                [Some(41), Some(1), Some(1), None]
            );
            let supplied = vm.host_provided_arguments(ticket).unwrap();
            assert_eq!(
                supplied
                    .iter()
                    .map(|(i, v)| (i, v.as_int()))
                    .collect::<Vec<_>>(),
                [(0, Some(41)), (3, None), (2, Some(1))]
            );
            vm.respond(ticket, HostResponse::Ready(Value::int(42)))
                .unwrap();
            assert!(matches!(next(&mut vm, task), RunOutcome::Returned(_)));
        }
    }
}
#[test]
fn planned_and_generic_native_binding_agree() {
    for generic in [false, true] {
        let (mut vm, _) = load(generic, false, 1);
        let task = vm
            .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
            .unwrap();
        let RunOutcome::HostCall(ticket) = next(&mut vm, task) else {
            panic!("native")
        };
        let supplied = vm.host_provided_arguments(ticket).unwrap();
        assert_eq!(
            supplied
                .iter()
                .map(|(i, v)| (i, v.as_int()))
                .collect::<Vec<_>>(),
            [(0, Some(41)), (3, None), (2, Some(1))]
        );
        vm.respond(ticket, HostResponse::Ready(Value::int(42)))
            .unwrap();
        assert!(matches!(next(&mut vm, task), RunOutcome::Returned(_)));
    }
}
#[test]
#[ignore = "release differential fixed native binding measurement"]
fn fixed_native_binding_cost() {
    for generic in [true, false] {
        let (mut vm, _) = load(generic, false, 200);
        let mut work = 0;
        let mut slots = 0;
        let start = std::time::Instant::now();
        for _ in 0..100 {
            let task = vm
                .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
                .unwrap();
            loop {
                let result = vm.run_slice(task, SliceBudget::default(), || 0).unwrap();
                work += vm.last_usage.work;
                match result {
                    RunOutcome::HostCall(ticket) => {
                        let n = vm.ticket(ticket).unwrap();
                        slots = vm.slots.len(n.args).unwrap() + vm.slots.len(n.bound).unwrap();
                        vm.respond(ticket, HostResponse::Ready(Value::int(42)))
                            .unwrap();
                    }
                    RunOutcome::BudgetYield(_) => {}
                    RunOutcome::Returned(delivery) => {
                        vm.discard_delivery(delivery).unwrap();
                        break;
                    }
                    other => panic!("{other:?}"),
                }
            }
        }
        eprintln!(
            "native_binding generic={generic} calls=20000 elapsed_us={} work={work} argument_root_values={slots}",
            start.elapsed().as_micros()
        );
    }
}

#[test]
fn planned_binding_type_fault_keeps_the_generic_call_location() {
    let mut observed = Vec::new();
    for generic in [true, false] {
        let (mut p, mut support) = fixture(1);
        p.signatures[0].parameters[3].ty = TypeId(1);
        support.natives[0].signature.parameters[3].ty = TypeId(1);
        let program = verify(p, Limits::default()).unwrap();
        let program = if generic {
            program.without_native_call_plans()
        } else {
            program
        };
        let mut vm = Vm::load(program, support, VmLimits::default(), SessionEpoch(1)).unwrap();
        let task = vm
            .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
            .unwrap();
        let RunOutcome::Fault(id) = next(&mut vm, task) else {
            panic!("type fault")
        };
        let fault = vm.delivery_fault(id).unwrap().unwrap();
        observed.push((fault.code, fault.location));
    }
    assert_eq!(observed[0], observed[1]);
    assert_eq!(observed[0].0.kind, FaultKind::Type);
}
#[test]
fn cancellation_releases_each_partial_planned_argument_window() {
    for stop in 0..=3 {
        let (mut vm, _) = load(false, false, 1);
        let task = vm
            .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
            .unwrap();
        loop {
            if matches!(vm.active.frames.data.last(), Some(Frame::Native(n)) if matches!(n.phase, NativePhase::PlannedCopy { index, .. } if index == stop))
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
                    || 0
                )
                .unwrap(),
                RunOutcome::BudgetYield(_)
            ));
        }
        vm.cancel_task(task).unwrap();
        assert!(matches!(
            vm.run_slice(task, SliceBudget::default(), || 0),
            Err(VmError::Idle)
        ));
        assert!(vm.active.frames.data.is_empty());
        review_tests::collect(&mut vm);
    }
}

#[test]
fn restore_rejects_a_valid_argument_plan_from_another_native() {
    let (mut p, mut support) = fixture(1);
    let symbol = SymbolId(p.symbols.len() as u32);
    p.symbols.push("host::other".into());
    let mut native = p.natives[0].clone();
    native.symbol = symbol;
    p.natives.push(native);
    let mut registration = support.natives[0].clone();
    registration.symbol = "host::other".into();
    support.natives.push(registration);
    let Descriptor::Call(mut other) = p.functions[0].descriptors[0].clone() else {
        unreachable!()
    };
    other.target = CallTarget::Native(NativeId(1));
    p.functions[0].descriptors.push(Descriptor::Call(other));
    let program = verify(p, Limits::default()).unwrap();
    let other_plan = program
        .native_call_plan(FunctionId(0), DescriptorId(1))
        .unwrap();
    let mut vm = Vm::load(
        program,
        support.clone(),
        VmLimits::default(),
        SessionEpoch(1),
    )
    .unwrap();
    let task = vm
        .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
        .unwrap();
    let RunOutcome::HostCall(_) = next(&mut vm, task) else {
        panic!("native")
    };
    let Frame::Native(frame) = vm.active.frames.data.last_mut().unwrap() else {
        unreachable!()
    };
    frame.argument_plan = Some(NativeArgumentPlan::Fixed(other_plan));
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
    loop {
        match restore.step(SnapshotBudget { units: 1 }) {
            Err(crate::snapshot::SnapshotError::Invalid(message)) => {
                assert_eq!(message, "native argument plan callee");
                break;
            }
            Ok(progress) => assert!(!progress.complete, "foreign plan was accepted"),
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn fixed_binding_checks_all_argument_reads_before_parameter_types() {
    let mut observed = Vec::new();
    for generic in [true, false] {
        let (mut p, mut support) = fixture(1);
        p.signatures[0].parameters[3].ty = TypeId(1);
        support.natives[0].signature.parameters[3].ty = TypeId(1);
        p.functions[0].code[2] = Instruction::new(Opcode::Clear, 2, 0, 0);
        let program = verify(p, Limits::default()).unwrap();
        let program = if generic {
            program.without_native_call_plans()
        } else {
            program
        };
        let mut vm = Vm::load(program, support, VmLimits::default(), SessionEpoch(1)).unwrap();
        let task = vm
            .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
            .unwrap();
        let RunOutcome::Fault(delivery) = next(&mut vm, task) else {
            panic!("argument read fault")
        };
        let info = vm.delivery_fault(delivery).unwrap().unwrap();
        observed.push((info.kind, info.location));
    }
    assert_eq!(observed[0], observed[1]);
    assert_eq!(observed[0].0, FaultKind::Uninitialized);
}
