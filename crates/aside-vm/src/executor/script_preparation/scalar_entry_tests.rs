use super::*;
use crate::snapshot::{SnapshotBudget, SnapshotRegistry};

pub(super) fn scalar_package(case: u8) -> BytecodePackage {
    let mut p = fixtures::helper_native();
    p.natives.clear();
    p.capabilities.clear();
    p.functions.truncate(2);
    p.signatures.truncate(2);
    let mut signature = p.signatures[0].clone();
    signature.parameters.clear();
    signature.rest = None;
    signature.return_type = TypeId(0);
    signature.effects = Effects::ALL;
    signature.property_bag_tail = None;
    p.signatures[0] = signature.clone();
    for (index, ty) in [
        BuiltinType::Null,
        BuiltinType::Bool,
        BuiltinType::Int,
        BuiltinType::Float,
    ]
    .into_iter()
    .enumerate()
    {
        let name = SymbolId(p.symbols.len() as u32);
        p.symbols.push(format!("argument{index}"));
        let ty_id = TypeId(p.types.len() as u32);
        p.types.push(TypeDef::Builtin(ty));
        signature.parameters.push(Parameter {
            name,
            ty: ty_id,
            mutable: false,
            default: None,
        });
    }
    p.signatures[1] = signature;
    p.constants = vec![
        Const::Null,
        Const::Bool(true),
        Const::Int(37),
        Const::FloatBits(1.5f64.to_bits()),
    ];
    for (index, f) in p.functions.iter_mut().enumerate() {
        f.kind = FunctionKind::Plain;
        f.signature = SignatureId(index as u32);
        f.captures.clear();
        f.regions.clear();
        f.source_map.clear();
        f.bindings.clear();
        f.descriptors.clear();
        f.register_count = 5;
    }
    let i = Instruction::new;
    p.functions[0].code = (0..4).map(|r| i(Opcode::LoadConst, r, r, 0)).collect();
    if !case.is_multiple_of(3) {
        p.functions[0].code[0].operands[1] = 2;
    }
    if case % 3 == 2 {
        p.functions[0].code[2] = i(Opcode::Clear, 2, 0, 0);
    }
    p.functions[0]
        .code
        .extend([i(Opcode::Call, 4, 0, 0), i(Opcode::Return, 4, 0, 0)]);
    p.functions[0].descriptors = vec![Descriptor::Call(CallDesc {
        contract: None,
        target: CallTarget::Function(FunctionId(1)),
        arguments: Arguments::Window(RegWindow {
            start: Reg(0),
            len: 4,
        }),
        names: vec![],
    })];
    p.functions[1].bindings = p.signatures[1]
        .parameters
        .iter()
        .enumerate()
        .map(|(index, p)| BindingDef {
            name: p.name,
            ty: p.ty,
            mutable: false,
            storage: BindingStorage::Value(Reg(index as u32)),
            initialization: BindingInitialization::Parameter(index as u32),
        })
        .collect();
    p.functions[1].code = vec![i(Opcode::BindRead, 4, 2, 0), i(Opcode::Return, 4, 0, 0)];
    if case >= 3 {
        p.functions[0].register_count = 6;
        p.functions[0]
            .code
            .insert(4, i(Opcode::MakeClosure, 4, 1, 0));
        p.functions[0].code[5].operands[0] = 5;
        p.functions[0].code[6].operands[0] = 5;
        let Descriptor::Call(call) = &mut p.functions[0].descriptors[0] else {
            unreachable!()
        };
        call.target = CallTarget::Register(Reg(4));
        p.functions[0].descriptors.push(Descriptor::Closure {
            function: FunctionId(1),
            captures: vec![],
        });
    }
    p
}

fn scalar_trace(
    legacy: bool,
    encoded: bool,
    case: u8,
    work: u64,
    deadline: bool,
) -> Vec<Observation> {
    let program = verify(scalar_package(case), Limits::default()).unwrap();
    assert!(program.scalar_entry(FunctionId(1)));
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
    vm.legacy_scalar_entry = legacy;
    let task = vm
        .start(FunctionId(0), &[], RootOwner(1), Context::Ordinary)
        .unwrap();
    let mut budget = SliceBudget {
        instructions: 64,
        work,
        allocation_bytes: 0,
        deadline_ns: deadline.then_some(1),
    };
    let mut observations = Vec::new();
    for _ in 0..1000 {
        let mut clocks = 0;
        let result = vm
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
            // Reads/writes/borrows are the operations this transaction removes.
            // All logical work, opcode and preparation counters remain compared.
            row.counts.slots = crate::frame_slots::SlotCounts::default();
        }
        observations.push(row);
        match result {
            RunOutcome::BudgetYield(usage) => {
                budget.work = work.max(usage.required_work);
                budget.allocation_bytes = usage.required_allocation;
                review_tests::gc(&mut vm);
            }
            RunOutcome::Returned(delivery) => {
                assert_eq!(case % 3, 0);
                if !legacy && work == 4096 {
                    assert!(vm.work_probe.scalar_entries.get() != 0);
                }
                assert_eq!(vm.delivery_value(delivery).unwrap().as_int(), Some(37));
                return observations;
            }
            RunOutcome::Fault(delivery) => {
                assert_eq!(
                    vm.delivery_fault(delivery).unwrap().unwrap().kind,
                    if case % 3 == 2 {
                        FaultKind::Uninitialized
                    } else {
                        FaultKind::Type
                    }
                );
                return observations;
            }
            other => panic!("{other:?}"),
        }
    }
    panic!("scalar entry did not progress")
}

#[test]
fn scalar_entry_preserves_budget_clock_fault_source_and_canonical_roots() {
    for encoded in [false, true] {
        for case in 0..6 {
            for work in [1, 7, 31, 32, 64, 4096] {
                for deadline in [false, true] {
                    assert_eq!(
                        scalar_trace(false, encoded, case, work, deadline),
                        scalar_trace(true, encoded, case, work, deadline),
                        "encoded={encoded} case={case} work={work} deadline={deadline}"
                    );
                }
            }
        }
    }
}

#[cfg(feature = "execution-counts")]
#[test]
fn scalar_entry_removes_the_second_bound_write_for_every_parameter() {
    let mut rows = Vec::new();
    for legacy in [true, false] {
        let program = verify(scalar_package(3), Limits::default()).unwrap();
        let mut vm = Vm::load(
            program,
            RuntimeSupport::default(),
            VmLimits::default(),
            SessionEpoch(51),
        )
        .unwrap();
        vm.legacy_scalar_entry = legacy;
        vm.legacy_scalar_suffixes = true;
        let task = vm
            .start(FunctionId(0), &[], RootOwner(1), Context::Ordinary)
            .unwrap();
        let RunOutcome::Returned(delivery) =
            vm.run_slice(task, SliceBudget::default(), || 0).unwrap()
        else {
            panic!("complete scalar call");
        };
        assert_eq!(vm.delivery_value(delivery).unwrap().as_int(), Some(37));
        assert_eq!(vm.work_probe.scalar_entries.get(), u64::from(!legacy));
        rows.push(vm.execution_counts());
    }
    assert_eq!(rows[0].instructions, rows[1].instructions);
    assert_eq!(rows[0].work, rows[1].work);
    assert_eq!(rows[0].script_steps, rows[1].script_steps);
    assert_eq!(rows[0].parameters_prepared, 4);
    assert_eq!(rows[1].parameters_prepared, 4);
    assert_eq!(rows[0].slots.write_calls - rows[1].slots.write_calls, 4);
    assert!(rows[0].slots.read_calls > rows[1].slots.read_calls);
}

#[test]
fn scalar_entry_restores_and_cancels_before_and_after_parameter_installation() {
    for installed in [false, true] {
        for cancel in [false, true] {
            let program = verify(scalar_package(3), Limits::default()).unwrap();
            let mut vm = Vm::load(
                program,
                RuntimeSupport::default(),
                VmLimits::default(),
                SessionEpoch(51),
            )
            .unwrap();
            let task = vm
                .start(FunctionId(0), &[], RootOwner(1), Context::Ordinary)
                .unwrap();
            // Warm code residency and stop immediately before the Call. Its
            // own bulk grant, rather than the initial page load, is the clock
            // boundary used to capture the pending scalar entry below.
            assert!(matches!(
                vm.run_slice(
                    task,
                    SliceBudget {
                        instructions: 5,
                        ..SliceBudget::default()
                    },
                    || 0
                )
                .unwrap(),
                RunOutcome::BudgetYield(_)
            ));
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
                assert!(
                    matches!(vm.active.frames.data.last(), Some(Frame::Script(s))
                    if matches!(s.phase, ScriptPhase::Body))
                );
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
                    panic!("restored scalar call");
                };
                assert_eq!(vm.delivery_value(delivery).unwrap().as_int(), Some(37));
                assert_eq!(vm.work_probe.scalar_entries.get(), u64::from(!installed));
            }
        }
    }
}
