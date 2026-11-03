//! The Body32 baseline and shared dispatch retain the same observable exits.
use super::*;

#[test]
fn capture_and_binding_use_the_shared_opcode_implementation_in_body_windows() {
    let run = |legacy| {
        let mut vm = load(1, true, false);
        vm.legacy_body_context = legacy;
        let task = vm
            .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
            .unwrap();
        assert!(matches!(
            vm.run_slice(
                task,
                SliceBudget {
                    instructions: u32::MAX,
                    work: u64::MAX,
                    allocation_bytes: u64::MAX,
                    deadline_ns: None,
                },
                || 0
            )
            .unwrap(),
            RunOutcome::Returned(_)
        ));
        (observe(&vm, 0), vm)
    };
    let (actual, vm) = run(false);
    let (baseline, old) = run(true);
    assert_eq!(actual, baseline);
    assert!(vm.work_probe.body_bindings.get() > 0);
    assert_eq!(old.work_probe.body_bindings.get(), 0);
    assert_eq!(old.work_probe.body_captures.get(), 0);
    println!(
        "shared-body bindings={} captures={} handoffs={}/{}",
        vm.work_probe.body_bindings.get(),
        vm.work_probe.body_captures.get(),
        vm.work_probe.body_handoffs.get(),
        old.work_probe.body_handoffs.get()
    );
    let captured = |legacy| {
        let program = verify(fixtures::cell_closures(), Limits::default()).unwrap();
        let mut vm = Vm::load(
            program,
            RuntimeSupport::default(),
            VmLimits::default(),
            SessionEpoch(87),
        )
        .unwrap();
        vm.legacy_body_context = legacy;
        let task = vm
            .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
            .unwrap();
        let budget = SliceBudget {
            instructions: u32::MAX,
            work: u64::MAX,
            allocation_bytes: u64::MAX,
            deadline_ns: None,
        };
        let RunOutcome::Returned(delivery) = vm.run_slice(task, budget, || 0).unwrap() else {
            unreachable!()
        };
        let closure = vm.delivery_value(delivery).unwrap();
        let task = vm
            .start_closure(closure, &[], RootOwner(2), Context::Ordinary)
            .unwrap();
        vm.discard_delivery(delivery).unwrap();
        let RunOutcome::Returned(delivery) = vm.run_slice(task, budget, || 0).unwrap() else {
            unreachable!()
        };
        assert_eq!(vm.delivery_value(delivery).unwrap().as_int(), Some(42));
        (observe(&vm, 0), vm.work_probe.body_captures.get())
    };
    let (actual, captures) = captured(false);
    let (baseline, old_captures) = captured(true);
    assert_eq!(actual, baseline);
    assert_eq!(captures, 1);
    assert_eq!(old_captures, 0);
}

#[test]
fn shared_body_matches_body32_at_every_budget_clock_fault_and_gc_boundary() {
    for case in 0..4 {
        for encoded in [false, true] {
            for work in [1, 12, 128] {
                for (clock, instructions) in [(0, 1), (0, 5), (0, 128), (1, 1), (1, 128), (2, 128)]
                {
                    let trace = |legacy| {
                        let mut vm = load(case, encoded, false);
                        vm.legacy_body_dispatch = legacy;
                        trace_vm(vm, case, work, clock, instructions)
                    };
                    assert_eq!(
                        trace(false),
                        trace(true),
                        "case={case} encoded={encoded} work={work} clock={clock} instructions={instructions}"
                    );
                }
            }
        }
    }
}

#[test]
fn handoff_retains_task_switch_restore_and_hard_or_cooperative_cancellation() {
    for restore in [false, true] {
        for cancel in [None, Some(false), Some(true)] {
            assert_eq!(
                task_dispatch_trace(false, false, restore, cancel),
                task_dispatch_trace(false, true, restore, cancel),
                "restore={restore} cancel={cancel:?}",
            );
        }
    }
}

#[test]
fn non_body_phases_keep_their_original_driver_classification() {
    let mut vm = load(0, true, false);
    vm.start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
        .unwrap();
    let Frame::Script(mut frame) = *vm.active.frames.data.last().unwrap() else {
        unreachable!()
    };
    frame.phase = ScriptPhase::CompactBegin;
    *vm.active.frames.data.last_mut().unwrap() = Frame::Script(frame);
    let mut meter = Meter::new(SliceBudget::default());
    assert!(matches!(
        vm.scalar_step(&mut meter).unwrap(),
        scalars::ScalarStep::General
    ));
    let Frame::Script(stored) = vm.active.frames.data.last().unwrap() else {
        unreachable!()
    };
    assert_eq!(stored.pc, frame.pc);
    assert!(stored.instruction.is_none());
    assert_eq!(meter.usage.work, 0);
    assert_eq!(vm.work_probe.body_handoffs.get(), 0);
}

#[test]
fn general_handoff_has_one_decode_no_opcode_charge_and_no_scalar_window() {
    assert!(size_of::<scalars::ScalarStep>() <= 4);
    for legacy in [false, true] {
        let mut vm = load(0, true, false);
        vm.legacy_body_dispatch = legacy;
        vm.start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
            .unwrap();
        let Frame::Script(mut frame) = *vm.active.frames.data.last().unwrap() else {
            unreachable!()
        };
        frame.phase = ScriptPhase::Body;
        frame.pc = Pc(vm.program.package().functions[0]
            .code
            .indexed_iter()
            .find_map(|entry| {
                let (pc, instruction) = entry.unwrap();
                (instruction.opcode == Opcode::Call as u16).then_some(pc as u32)
            })
            .unwrap());
        vm.put(frame.regs, 0, Value::int(41)).unwrap();
        *vm.active.frames.data.last_mut().unwrap() = Frame::Script(frame);
        let mut meter = Meter::new(SliceBudget {
            instructions: 0,
            work: 1,
            allocation_bytes: 0,
            deadline_ns: None,
        });
        let handoff = vm.scalar_step(&mut meter).unwrap();
        assert_eq!(meter.usage.instructions, 0);
        assert_eq!(meter.usage.work, 0);
        assert_eq!(meter.usage.required_work, 1);
        assert_eq!(vm.work_probe.scalar_windows.get(), u64::from(legacy));
        if legacy {
            assert!(matches!(handoff, scalars::ScalarStep::General));
        } else {
            let scalars::ScalarStep::Body(opcode) = handoff else {
                panic!("missing fetched Body instruction")
            };
            assert_eq!(opcode, Opcode::Call);
            let Frame::Script(mut working) = *vm.active.frames.data.last().unwrap() else {
                unreachable!()
            };
            assert_eq!(working.pc, frame.pc);
            assert_eq!(working.instruction_pc, frame.pc);
            assert!(working.instruction_end.0 > working.pc.0);
            assert_eq!(working.instruction.unwrap().opcode, opcode as u16);
            // The instruction is fetched, but the original callee admission
            // still fails with zero instruction grant at this same source PC.
            assert!(
                !vm.script_body_decoded_step(&mut working, &mut meter, opcode)
                    .unwrap()
            );
            assert_eq!(working.pc, frame.pc);
            assert_eq!(meter.usage.instructions, 0);
            assert!(meter.usage.required_work > 1);
        }
    }
}

fn page_boundary_vm(encoded: bool, legacy: bool, fault: bool) -> (Vm, TaskId) {
    let mut package = fixtures::helper_native();
    package.natives.clear();
    package.capabilities.clear();
    package.functions.truncate(1);
    package.signatures.truncate(1);
    package.signatures[0].parameters.clear();
    package.signatures[0].return_type = TypeId(0);
    package.constants = vec![Const::Int(41)];
    let function = &mut package.functions[0];
    function.signature = SignatureId(0);
    function.register_count = 302;
    function.bindings.clear();
    function.descriptors.clear();
    function.regions.clear();
    function.source_map.clear();
    let mut code = vec![Instruction::new(Opcode::Move, 0, 0, 0); 8191];
    code.extend([
        Instruction::new(Opcode::Eq, 0, 300, if fault { 301 } else { 0 }),
        Instruction::new(Opcode::Return, 0, 0, 0),
    ]);
    function.code = code;
    let program = verify(package, Limits::default()).unwrap();
    let program = if encoded {
        VerifiedProgram::decode(
            program.encode(Limits::default()).unwrap().as_bytes(),
            Limits::default(),
        )
        .unwrap()
    } else {
        program
    };
    let (pc, instruction) = program.package().functions[0]
        .code
        .indexed_iter()
        .find_map(|entry| {
            let (pc, instruction) = entry.unwrap();
            (instruction.opcode == Opcode::Eq as u16).then_some((pc, instruction))
        })
        .unwrap();
    assert_eq!(pc, 8191);
    assert_eq!(instruction.operands[1], 300);
    let mut vm = Vm::load(
        program,
        RuntimeSupport::default(),
        VmLimits::default(),
        SessionEpoch(86),
    )
    .unwrap();
    vm.legacy_body_dispatch = legacy;
    let task = vm
        .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
        .unwrap();
    let Frame::Script(mut frame) = *vm.active.frames.data.last().unwrap() else {
        unreachable!()
    };
    frame.phase = ScriptPhase::Body;
    frame.pc = Pc(pc as u32);
    for register in [0, 300] {
        vm.put(frame.regs, register, Value::int(41)).unwrap();
    }
    *vm.active.frames.data.last_mut().unwrap() = Frame::Script(frame);
    (vm, task)
}

#[test]
fn general_handoff_crosses_code_pages_without_losing_fault_pc_or_required_work() {
    for encoded in [false, true] {
        for fault in [false, true] {
            for work in [1, 31, 4096] {
                let run = |legacy| {
                    let (mut vm, task) = page_boundary_vm(encoded, legacy, fault);
                    let mut budget = SliceBudget {
                        instructions: 1,
                        work,
                        allocation_bytes: 0,
                        deadline_ns: Some(1),
                    };
                    let mut trace = Vec::new();
                    for _ in 0..200 {
                        let mut clocks = 0;
                        let outcome = vm
                            .run_slice(task, budget, || {
                                clocks += 1;
                                0
                            })
                            .unwrap();
                        trace.push(observe(&vm, clocks));
                        match outcome {
                            RunOutcome::BudgetYield(usage) => {
                                budget.work = work.max(usage.required_work);
                                budget.allocation_bytes = usage.required_allocation;
                                review_tests::gc(&mut vm);
                            }
                            RunOutcome::Returned(id) => {
                                assert!(!fault);
                                assert_eq!(vm.delivery_value(id).unwrap().as_bool(), Some(true));
                                return trace;
                            }
                            RunOutcome::Fault(id) => {
                                assert!(fault);
                                let info = vm.delivery_fault(id).unwrap().unwrap();
                                assert_eq!(info.kind, FaultKind::Uninitialized);
                                assert_eq!(info.location.pc, Pc(8191));
                                return trace;
                            }
                            other => panic!("page handoff outcome {other:?}"),
                        }
                    }
                    panic!("page handoff did not complete")
                };
                assert_eq!(
                    run(false),
                    run(true),
                    "encoded={encoded} fault={fault} work={work}"
                );
            }
        }
    }
}
