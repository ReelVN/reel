//! Bound object/any Value entries retain the ordinary resumable oracle.
use super::*;
use crate::class::State;
use crate::snapshot::{SnapshotBudget, SnapshotRegistry};

fn package(case: u8) -> BytecodePackage {
    let mut p = super::scalar_entry_tests::scalar_package(3);
    let any = TypeId(p.types.len() as u32);
    p.types.push(TypeDef::Builtin(BuiltinType::Any));
    let float = p.signatures[1].parameters[3].ty;
    let int = p.signatures[1].parameters[2].ty;
    p.signatures[1].parameters.truncate(2);
    p.signatures[1].parameters[0].ty = if case == 3 { int } else { any };
    p.signatures[1].parameters[1].ty = if matches!(case, 1 | 5) { any } else { float };
    if case == 6 {
        let tuple = TypeId(p.types.len() as u32);
        p.types.push(TypeDef::Tuple(vec![int, float]));
        p.signatures[1].parameters[1].ty = tuple;
    }
    p.functions[0].register_count = 4;
    p.functions[0].code = vec![
        Instruction::new(Opcode::Call, 3, 0, 0),
        Instruction::new(Opcode::Return, 3, 0, 0),
    ];
    p.functions[0].descriptors = vec![Descriptor::Call(CallDesc {
        contract: None,
        target: CallTarget::Register(Reg(0)),
        arguments: Arguments::Window(RegWindow {
            start: Reg(1),
            len: if case == 5 { 2 } else { 1 },
        }),
        names: vec![],
    })];
    p.functions[1].register_count = 12;
    p.functions[1].bindings.truncate(2);
    for index in 0..2 {
        p.functions[1].bindings[index].ty = p.signatures[1].parameters[index].ty;
    }
    for register in 2..11 {
        p.functions[1].bindings.push(BindingDef {
            name: p.signatures[1].parameters[0].name,
            ty: any,
            mutable: false,
            storage: BindingStorage::Value(Reg(register)),
            initialization: BindingInitialization::Explicit,
        });
    }
    p.functions[1].code = vec![Instruction::new(Opcode::Return, 1, 0, 0)];
    p.functions[1].descriptors.clear();
    p
}

fn loaded(enabled: bool, encoded: bool, case: u8) -> (Vm, TaskId) {
    let program = verify(package(case), Limits::default()).unwrap();
    assert!(!program.scalar_entry(FunctionId(1)));
    let entry = program.value_entry(FunctionId(1)).unwrap();
    assert_eq!(entry.bindings, 11);
    assert_eq!(entry.clears, 9);
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
        SessionEpoch(51),
    )
    .unwrap();
    vm.value_entries = enabled;
    vm.legacy_scalar_suffixes = true;
    let closure = vm
        .heap
        .alloc_record(
            ObjectKind::Closure,
            &[Value::int(1), Value::int(1), Value::int(0)],
        )
        .unwrap();
    let receiver = vm
        .heap
        .alloc_record(
            ObjectKind::Instance,
            &[
                Value::NULL,
                Value::NULL,
                Value::int(State::Ready as i64),
                Value::NULL,
            ],
        )
        .unwrap();
    let target = if case == 5 {
        closure
    } else {
        vm.heap
            .alloc_record(ObjectKind::BoundScript, &[closure, receiver, Value::NULL])
            .unwrap()
    };
    let argument = if case == 6 {
        vm.heap
            .alloc_tuple(&[Value::int(7), Value::float(1.5)])
            .unwrap()
    } else if matches!(case, 1 | 5) {
        vm.heap.alloc_array(&[Value::int(7)]).unwrap()
    } else if case == 2 {
        Value::int(7)
    } else {
        Value::float(1.5)
    };
    let task = vm
        .start(FunctionId(0), &[], RootOwner(1), Context::Ordinary)
        .unwrap();
    let Frame::Script(mut frame) = *vm.active.frames.data.last().unwrap() else {
        unreachable!()
    };
    frame.phase = ScriptPhase::Body;
    vm.put(frame.regs, 0, target).unwrap();
    if case != 4 {
        vm.put(frame.regs, 1, if case == 5 { receiver } else { argument })
            .unwrap();
    }
    if case == 5 {
        vm.put(frame.regs, 2, argument).unwrap();
    }
    *vm.active.frames.data.last_mut().unwrap() = Frame::Script(frame);
    (vm, task)
}

fn trace(enabled: bool, encoded: bool, case: u8, work: u64, deadline: bool) -> Vec<Observation> {
    let (mut vm, task) = loaded(enabled, encoded, case);
    let mut budget = SliceBudget {
        instructions: 1,
        work,
        allocation_bytes: 0,
        deadline_ns: deadline.then_some(1),
    };
    let mut rows = Vec::new();
    for _ in 0..5000 {
        let mut clocks = 0;
        let outcome = vm
            .run_slice(task, budget, || {
                let now = u64::from(clocks != 0);
                clocks += 1;
                now
            })
            .unwrap();
        #[allow(unused_mut)]
        let mut row = observe(&vm, clocks);
        #[cfg(feature = "execution-counts")]
        {
            row.counts.slots = crate::frame_slots::SlotCounts::default();
        }
        rows.push(row);
        match outcome {
            RunOutcome::BudgetYield(usage) => {
                budget.work = work.max(usage.required_work);
                budget.allocation_bytes = usage.required_allocation;
                review_tests::gc(&mut vm);
            }
            RunOutcome::Returned(delivery) => {
                assert!(!matches!(case, 2..=4));
                let result = vm.delivery_value(delivery).unwrap();
                if matches!(case, 1 | 5 | 6) {
                    assert!(vm.heap.read_object(result).is_ok());
                } else {
                    assert_eq!(result.as_float(), Some(1.5));
                }
                if enabled && work == 4096 && !deadline {
                    assert_eq!(vm.work_probe.scalar_entries.get(), 1);
                }
                return rows;
            }
            RunOutcome::Fault(delivery) => {
                assert!(matches!(case, 2..=4));
                assert_eq!(
                    vm.delivery_fault(delivery).unwrap().unwrap().kind,
                    if case == 4 {
                        FaultKind::Uninitialized
                    } else {
                        FaultKind::Type
                    }
                );
                return rows;
            }
            other => panic!("{other:?}"),
        }
    }
    panic!("Value entry did not progress")
}

#[test]
fn value_entry_preserves_each_budget_clock_gc_and_fault_boundary() {
    for encoded in [false, true] {
        for case in 0..7 {
            for work in [1, 7, 31, 32, 64, 4096] {
                for deadline in [false, true] {
                    assert_eq!(
                        trace(false, encoded, case, work, deadline),
                        trace(true, encoded, case, work, deadline),
                        "encoded={encoded} case={case} work={work} deadline={deadline}"
                    );
                }
            }
        }
    }
}

fn pending() -> (Vm, TaskId) {
    let (mut vm, task) = loaded(true, false, 1);
    vm.program.package().functions[0]
        .code
        .fetch_budgeted(0, &mut vm.code_window, &mut |_, _| true)
        .unwrap()
        .unwrap();
    let mut clocks = 0;
    assert!(matches!(
        vm.run_slice(
            task,
            SliceBudget {
                deadline_ns: Some(1),
                ..SliceBudget::default()
            },
            || {
                let now = u64::from(clocks != 0);
                clocks += 1;
                now
            }
        )
        .unwrap(),
        RunOutcome::BudgetYield(_)
    ));
    assert!(
        matches!(vm.active.frames.data.last(), Some(Frame::Script(s))
        if s.code.function == FunctionId(1) && matches!(s.phase, ScriptPhase::Copy { index: 0, .. }))
    );
    (vm, task)
}

#[test]
fn value_entry_restores_and_cancels_on_both_sides_of_root_installation() {
    for installed in [false, true] {
        for cancel in [false, true] {
            let (mut vm, task) = pending();
            if installed {
                assert!(matches!(
                    vm.run_slice(
                        task,
                        SliceBudget {
                            instructions: 0,
                            ..SliceBudget::default()
                        },
                        || 0
                    )
                    .unwrap(),
                    RunOutcome::BudgetYield(_)
                ));
                assert_eq!(vm.work_probe.scalar_entries.get(), 1);
                let Some(Frame::Script(s)) = vm.active.frames.data.last() else {
                    unreachable!()
                };
                assert!(matches!(s.phase, ScriptPhase::Body));
                for local in 2..11 {
                    assert!(matches!(
                        vm.slots.read_vm(at(s.regs, local)),
                        Err(HeapError::UninitializedSlot)
                    ));
                }
            }
            review_tests::collect(&mut vm);
            let registry = SnapshotRegistry::default();
            let mut capture = vm.snapshot_begin(&registry).unwrap();
            while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
            let image = capture.finish().unwrap();
            let mut restore = Vm::restore_begin(
                &image,
                vm.program().clone(),
                RuntimeSupport::default(),
                vm.limits,
                SessionEpoch(52),
                &registry,
            )
            .unwrap();
            while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
            let (mut vm, map) = restore.finish().unwrap();
            vm.value_entries = true;
            let task = map.task(task).unwrap();
            if cancel {
                vm.cancel_task(task).unwrap();
                assert!(matches!(
                    vm.run_slice(task, SliceBudget::default(), || 0),
                    Err(VmError::Idle)
                ));
                assert!(vm.active.frames.data.is_empty());
                review_tests::collect(&mut vm);
            } else {
                let RunOutcome::Returned(delivery) =
                    vm.run_slice(task, SliceBudget::default(), || 0).unwrap()
                else {
                    panic!("restored Value call")
                };
                let result = vm.delivery_value(delivery).unwrap();
                assert_eq!(vm.heap.sequence_get(result, 0).unwrap().as_int(), Some(7));
                assert_eq!(vm.work_probe.scalar_entries.get(), u64::from(!installed));
            }
        }
    }
}

#[test]
fn value_entry_falls_back_before_partial_preparation_or_a_due_clock() {
    for prefix in [0, 1] {
        let (mut vm, _) = pending();
        let Some(Frame::Script(s)) = vm.active.frames.data.last().copied() else {
            unreachable!()
        };
        let mut preparation = PreparationFrame::capture(&s);
        let mut meter = Meter::new(SliceBudget {
            deadline_ns: Some(1),
            ..SliceBudget::default()
        });
        meter.clock_steps = 19;
        let before = observe(&vm, 0);
        assert!(!vm.scalar_entry_step(&mut preparation, &mut meter));
        #[allow(unused_mut)]
        let mut after = observe(&vm, 0);
        #[cfg(feature = "execution-counts")]
        {
            after.counts.slots = before.counts.slots;
        }
        assert_eq!(after, before);
        meter.clock_steps = 31;
        // Receiver is the sole initialized bound slot; a duplicate explicit
        // slot retains the ordinary DuplicateArgument fault prefix.
        if prefix == 1 {
            vm.put(s.bound, 1, Value::int(9)).unwrap();
        } else {
            preparation.phase = PreparationPhase::Copy {
                source: match preparation.phase {
                    PreparationPhase::Copy { source, .. } => source,
                    _ => unreachable!(),
                },
                index: 1,
            };
        }
        let before = observe(&vm, 0);
        assert!(!vm.scalar_entry_step(&mut preparation, &mut meter));
        // Physical reads/borrows are diagnostic-only; no root/frame or Meter
        // grant has been published by rejected speculation.
        #[allow(unused_mut)]
        let mut after = observe(&vm, 0);
        #[cfg(feature = "execution-counts")]
        {
            after.counts.slots = before.counts.slots;
        }
        assert_eq!(after, before);
        assert_eq!(meter.usage.work, 0);
    }
}

#[test]
fn value_entry_clears_explicit_locals_and_keeps_parameter_roots() {
    for enabled in [false, true] {
        let (mut vm, task) = pending();
        vm.value_entries = enabled;
        let Some(Frame::Script(s)) = vm.active.frames.data.last().copied() else {
            unreachable!()
        };
        let stale = vm.heap.alloc_array(&[Value::int(88)]).unwrap();
        for local in 2..11 {
            vm.put(s.regs, local, stale).unwrap();
        }
        assert!(matches!(
            vm.run_slice(
                task,
                SliceBudget {
                    instructions: 0,
                    ..SliceBudget::default()
                },
                || 0
            )
            .unwrap(),
            RunOutcome::BudgetYield(_)
        ));
        let receiver = vm.slots.read_vm(at(s.regs, 0)).unwrap();
        let value = vm.slots.read_vm(at(s.regs, 1)).unwrap();
        assert_eq!(
            vm.heap.read_object(receiver).unwrap().kind(),
            ObjectKind::Instance
        );
        assert_eq!(vm.heap.sequence_get(value, 0).unwrap().as_int(), Some(7));
        for local in 2..11 {
            assert!(matches!(
                vm.slots.read_vm(at(s.regs, local)),
                Err(HeapError::UninitializedSlot)
            ));
        }
        review_tests::collect(&mut vm);
        assert!(matches!(
            vm.heap.read_object(stale),
            Err(HeapError::InvalidHandle)
        ));
        assert!(vm.heap.read_object(receiver).is_ok());
        assert!(vm.heap.read_object(value).is_ok());
        assert_eq!(vm.work_probe.scalar_entries.get(), u64::from(enabled));
    }
}

#[test]
fn value_entry_metadata_excludes_cells_defaults_and_parameter_local_aliases() {
    for case in 0..4 {
        let mut p = package(1);
        match case {
            0 => p.functions[1].bindings[0].storage = BindingStorage::Cell(Reg(0)),
            1 => {
                p.signatures[1].parameters[0].default = Some(ParameterDefault::Constant(ConstId(0)))
            }
            2 => p.functions[1].bindings[2].storage = BindingStorage::Value(Reg(0)),
            3 => {
                p.functions[1].register_count = 65;
                p.functions[1].bindings[2].storage = BindingStorage::Value(Reg(64));
            }
            _ => unreachable!(),
        }
        let program = verify(p, Limits::default()).unwrap();
        assert!(program.value_entry(FunctionId(1)).is_none(), "case={case}");
    }
}
