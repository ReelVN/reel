//! Author checks of cancellation at real, internally observable preparation stages.
use super::*;

#[test]
fn cancelled_preparation_stages_route_at_caller_without_entering_callee_pc_zero_catch() {
    for function in [1, 2] {
        for target in [
            "copy",
            "bind",
            "defaults",
            "default-call",
            "parameter",
            "install",
        ] {
            if function == 2 && target == "default-call" {
                continue;
            }
            let mut p = fixtures::parameter_cell_default();
            let f = &mut p.functions[function as usize];
            let i = Instruction::new;
            f.code = vec![
                i(Opcode::BindRead, 2, 0, 0),
                i(Opcode::Leave, 4, 0, 0),
                i(Opcode::LoadConst, 2, 2, 0),
                i(Opcode::Return, 2, 0, 0),
                i(Opcode::Return, 2, 0, 0),
            ];
            f.regions = vec![ExceptionRegion {
                protected: PcRange {
                    start: Pc(0),
                    end: Pc(2),
                },
                catch: Some(CatchRegion {
                    range: PcRange {
                        start: Pc(2),
                        end: Pc(4),
                    },
                    exception: Reg(3),
                }),
                finally: None,
            }];
            let mut vm = Vm::load(
                verify(p, Limits::default()).unwrap(),
                RuntimeSupport::default(),
                VmLimits::default(),
                SessionEpoch(3),
            )
            .unwrap();
            let t = vm
                .start(FunctionId(0), &[], RootOwner(1), Context::Ordinary)
                .unwrap();
            let mut work = 1;
            let mut allocation_bytes = 0;
            let mut reached = false;
            for _ in 0..5000 {
                let out = vm
                    .run_slice(
                        t,
                        SliceBudget {
                            instructions: 1,
                            work,
                            allocation_bytes,
                            ..SliceBudget::default()
                        },
                        || 0,
                    )
                    .unwrap();
                if vm.heap().phase() == GcPhase::Idle {
                    vm.begin_collection().unwrap();
                }
                vm.gc_step(GcBudget {
                    units: 1,
                    ..GcBudget::default()
                })
                .unwrap();
                if let Some(Frame::Script(s)) = vm.active.frames.data.last() {
                    let phase = match s.phase {
                        ScriptPhase::Copy { .. } => "copy",
                        ScriptPhase::Bind { .. } => "bind",
                        ScriptPhase::Defaults { .. } => "defaults",
                        ScriptPhase::DefaultCall { .. } => "default-call",
                        ScriptPhase::Parameter { .. } => "parameter",
                        ScriptPhase::Install { .. } => "install",
                        _ => "body/exit",
                    };
                    if s.code.function == FunctionId(function) && phase == target {
                        reached = true;
                        break;
                    }
                }
                let RunOutcome::BudgetYield(u) = out else {
                    panic!("{target}: {out:?}")
                };
                work = u.required_work.max(1);
                allocation_bytes = u.required_allocation;
            }
            assert!(reached, "{target}");
            vm.cancel_cooperatively(t).unwrap();
            let d = loop {
                match vm
                    .run_slice(
                        t,
                        SliceBudget {
                            instructions: 1,
                            ..SliceBudget::default()
                        },
                        || 0,
                    )
                    .unwrap()
                {
                    RunOutcome::BudgetYield(_) => {}
                    RunOutcome::Fault(d) => break d,
                    other => panic!("{target}: {other:?}"),
                }
            };
            let info = vm.delivery_fault(d).unwrap().unwrap();
            assert_eq!(info.kind, FaultKind::Cancelled, "{target}");
            assert_eq!(
                (info.location.code.function, info.location.pc),
                (FunctionId(0), Pc(1)),
                "{target}"
            );
            assert_eq!(vm.stats().comparison_depth, 0);
            vm.discard_delivery(d).unwrap();
            assert!(vm.is_idle());
        }
    }
}

#[test]
fn cancellation_in_an_entered_default_body_uses_its_real_catch() {
    let mut p = fixtures::parameter_cell_default();
    let f = &mut p.functions[2];
    let i = Instruction::new;
    f.code = vec![
        i(Opcode::LoadConst, 1, 1, 0),
        i(Opcode::Leave, 4, 0, 0),
        i(Opcode::LoadConst, 1, 2, 0),
        i(Opcode::Return, 1, 0, 0),
        i(Opcode::Return, 1, 0, 0),
    ];
    f.regions = vec![ExceptionRegion {
        protected: PcRange {
            start: Pc(0),
            end: Pc(2),
        },
        catch: Some(CatchRegion {
            range: PcRange {
                start: Pc(2),
                end: Pc(4),
            },
            exception: Reg(2),
        }),
        finally: None,
    }];
    let mut vm = Vm::load(
        verify(p, Limits::default()).unwrap(),
        RuntimeSupport::default(),
        VmLimits::default(),
        SessionEpoch(3),
    )
    .unwrap();
    let t = vm
        .start(
            FunctionId(1),
            &[Value::int(41)],
            RootOwner(1),
            Context::Ordinary,
        )
        .unwrap();
    let mut work = 1;
    let mut allocation_bytes = 0;
    loop {
        let out = vm
            .run_slice(
                t,
                SliceBudget {
                    instructions: 1,
                    work,
                    allocation_bytes,
                    ..SliceBudget::default()
                },
                || 0,
            )
            .unwrap();
        if let Some(Frame::Script(s)) = vm.active.frames.data.last()
            && s.code.function == FunctionId(2)
            && matches!(s.phase, ScriptPhase::Body)
            && s.pc == Pc(0)
        {
            break;
        }
        let RunOutcome::BudgetYield(u) = out else {
            panic!("{out:?}")
        };
        work = u.required_work.max(1);
        allocation_bytes = u.required_allocation;
    }
    vm.cancel_cooperatively(t).unwrap();
    let d = loop {
        match vm
            .run_slice(
                t,
                SliceBudget {
                    instructions: 1,
                    ..SliceBudget::default()
                },
                || 0,
            )
            .unwrap()
        {
            RunOutcome::BudgetYield(_) => {}
            RunOutcome::Returned(d) => break d,
            other => panic!("{other:?}"),
        }
    };
    let tuple = vm.delivery_value(d).unwrap();
    assert_eq!(vm.heap().get_field(tuple, 0).unwrap().as_int(), Some(42));
    assert_eq!(vm.heap().get_field(tuple, 1).unwrap().as_int(), Some(1));
}
