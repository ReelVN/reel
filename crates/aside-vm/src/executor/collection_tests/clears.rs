use super::*;

#[test]
fn scalar_clears_keep_the_general_path_deadline_batch() {
    let observations = [false, true].map(|general| {
        let mut code = vec![op(Opcode::Clear3, 0, 1, 2); 96];
        code.push(op(Opcode::Return, 3, 0, 0));
        let mut vm = load(package(vec![], code, vec![], 4));
        vm.work_probe.general_clears = general;
        let task = vm
            .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
            .unwrap();
        let Frame::Script(mut frame) = *vm.active.frames.data.last().unwrap() else {
            unreachable!()
        };
        frame.phase = ScriptPhase::Body;
        *vm.active.frames.data.last_mut().unwrap() = Frame::Script(frame);
        for register in 0..4 {
            vm.put(frame.regs, register, Value::int(41)).unwrap();
        }
        // The clock comparison covers steady instructions, after cold code
        // residency admission has completed outside this deadline window.
        vm.program.package().functions[0]
            .code
            .fetch_budgeted(0, &mut vm.code_window, &mut |_, _| true)
            .unwrap()
            .unwrap();
        let mut calls = 0;
        let result = vm
            .run_slice(
                task,
                SliceBudget {
                    instructions: 1000,
                    work: 10000,
                    allocation_bytes: 0,
                    deadline_ns: Some(1),
                },
                || {
                    let now = u64::from(calls != 0);
                    calls += 1;
                    now
                },
            )
            .unwrap();
        let RunOutcome::BudgetYield(usage) = result else {
            panic!("expected clock yield")
        };
        assert_eq!(usage.instructions, 32);
        (usage, calls, vm.task_location(task).unwrap().unwrap().pc.0)
    });
    assert_eq!(observations[0], observations[1]);
}

#[test]
fn scalar_clears_keep_general_path_budget_positions_and_initialization_bits() {
    for work in [1, 2, 3, 7, 31] {
        let traces = [false, true].map(|general| {
            let mut vm = load(package(
                vec![],
                vec![
                    op(Opcode::Clear, 0, 0, 0),
                    op(Opcode::Clear2, 2, 3, 0),
                    op(Opcode::Clear3, 4, 5, 6),
                    op(Opcode::Return, 1, 0, 0),
                ],
                vec![],
                7,
            ));
            vm.work_probe.general_clears = general;
            let task = vm
                .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
                .unwrap();
            let Frame::Script(mut frame) = *vm.active.frames.data.last().unwrap() else {
                unreachable!()
            };
            frame.phase = ScriptPhase::Body;
            *vm.active.frames.data.last_mut().unwrap() = Frame::Script(frame);
            for register in 0..7 {
                vm.put(frame.regs, register, Value::int(41)).unwrap();
            }
            let mut budget = SliceBudget {
                instructions: 1,
                work,
                allocation_bytes: 64 << 10,
                deadline_ns: None,
            };
            let mut trace = Vec::new();
            loop {
                let outcome = vm.run_slice(task, budget, || 0).unwrap();
                match outcome {
                    RunOutcome::BudgetYield(usage) => {
                        let state = vm.active.frames.data.last().map(|active| {
                            let Frame::Script(frame) = active else {
                                unreachable!()
                            };
                            (
                                frame.pc.0,
                                (0..7)
                                    .map(|register| vm.slots.read(at(frame.regs, register)).is_ok())
                                    .collect::<Vec<_>>(),
                            )
                        });
                        trace.push((usage, state));
                        budget.work = work.max(usage.required_work);
                        budget.allocation_bytes =
                            budget.allocation_bytes.max(usage.required_allocation);
                        if vm.heap.phase() == GcPhase::Idle {
                            vm.begin_collection().unwrap();
                        }
                        vm.gc_step(GcBudget {
                            units: 1,
                            ..Default::default()
                        })
                        .unwrap();
                    }
                    RunOutcome::Returned(delivery) => {
                        assert_eq!(vm.delivery_value(delivery).unwrap().as_int(), Some(41));
                        vm.discard_delivery(delivery).unwrap();
                        break;
                    }
                    other => panic!("unexpected outcome {other:?}"),
                }
            }
            trace
        });
        assert_eq!(traces[0], traces[1], "work={work}");
    }
}
