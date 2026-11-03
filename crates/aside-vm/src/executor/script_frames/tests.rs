//! Direct frame publication retains the old enum assignment at every safe point.
use super::*;
use crate::snapshot::{SnapshotBudget, SnapshotRegistry, VmSnapshot};
mod execution;
mod unified;

#[derive(Debug, PartialEq)]
struct Observation {
    usage: SliceUsage,
    frames: Vec<u8>,
    caches: Vec<u8>,
    values: Vec<u8>,
    clocks: u32,
    #[cfg(feature = "execution-counts")]
    counts: ExecutionCounts,
}

fn observe(vm: &Vm, clocks: u32) -> Observation {
    let mut values = Vec::new();
    let mut caches = Vec::new();
    for frame in &vm.active.frames.data {
        let ranges = match frame {
            Frame::Script(frame) => {
                assert_snapshot_v1_field_stream(frame);
                caches.push((
                    frame.instruction_end,
                    frame.instruction_pc,
                    frame.member_symbol,
                    frame.instruction.map(|op| (op.opcode, op.operands)),
                ));
                vec![frame.regs, frame.env, frame.args, frame.bound]
            }
            Frame::Native(frame) => {
                let mut ranges = vec![frame.args, frame.bound, frame.roots];
                ranges.extend(frame.bindings);
                ranges
            }
        };
        for range in ranges {
            values.push(
                (0..vm.slots.len(range).unwrap())
                    .map(|index| vm.slots.read(at(range, index)).ok())
                    .collect::<Vec<_>>(),
            );
        }
    }
    Observation {
        usage: vm.last_usage,
        frames: postcard::to_allocvec(&vm.active.frames.data).unwrap(),
        caches: postcard::to_allocvec(&caches).unwrap(),
        values: postcard::to_allocvec(&values).unwrap(),
        clocks,
        #[cfg(feature = "execution-counts")]
        counts: {
            let mut counts = vm.execution_counts();
            counts.heap_id = 0;
            counts
        },
    }
}

fn load(case: u8, encoded: bool, legacy: bool) -> Vm {
    let mut package = if case == 1 {
        fixtures::parameter_cell_default()
    } else {
        fixtures::helper_native()
    };
    if case == 2 {
        // The invalid callee faults before either the script or native body.
        package.functions[0].code[0].operands[1] = 0;
    }
    let support = if case == 1 {
        RuntimeSupport::default()
    } else {
        review_tests::echo_registry()
    };
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
    let mut limits = VmLimits::default();
    if case == 3 {
        // Fail after the caller cursor was published, before its callee exists.
        limits.max_frames = 1;
    }
    let mut vm = Vm::load(program, support, limits, SessionEpoch(73)).unwrap();
    vm.work_probe.legacy_script_publication = legacy;
    vm.slots.checked_vm_access = legacy;
    vm
}

fn trace(
    case: u8,
    encoded: bool,
    legacy: bool,
    work: u64,
    clock: u8,
    instructions: u32,
) -> Vec<Observation> {
    trace_vm(load(case, encoded, legacy), case, work, clock, instructions)
}

fn trace_vm(mut vm: Vm, case: u8, work: u64, clock: u8, instructions: u32) -> Vec<Observation> {
    let task = vm
        .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
        .unwrap();
    let mut budget = SliceBudget {
        instructions,
        work,
        allocation_bytes: 0,
        deadline_ns: (clock != 0).then_some(1),
    };
    let mut observations = Vec::new();
    for _ in 0..5000 {
        let mut clocks = 0;
        let outcome = vm
            .run_slice(task, budget, || {
                let now = u64::from(clock == 1 && clocks != 0);
                clocks += 1;
                now
            })
            .unwrap();
        observations.push(observe(&vm, clocks));
        match outcome {
            RunOutcome::BudgetYield(usage) => {
                budget.work = work.max(usage.required_work);
                budget.allocation_bytes = usage.required_allocation;
                review_tests::gc(&mut vm);
            }
            RunOutcome::HostCall(ticket) => {
                assert_eq!(case, 0);
                assert_eq!(vm.host_arguments(ticket).unwrap()[0].as_int(), Some(41));
                vm.respond(ticket, HostResponse::Ready(Value::int(42)))
                    .unwrap();
            }
            RunOutcome::Returned(delivery) => {
                assert!(!matches!(case, 2 | 3));
                let value = vm.delivery_value(delivery).unwrap();
                if case == 0 {
                    assert_eq!(value.as_int(), Some(42));
                } else {
                    let first = vm.heap.sequence_get(value, 0).unwrap();
                    assert_eq!(vm.heap.sequence_get(first, 0).unwrap().as_int(), Some(43));
                }
                return observations;
            }
            RunOutcome::Fault(delivery) => {
                assert!(matches!(case, 2 | 3));
                assert_eq!(
                    vm.delivery_fault(delivery).unwrap().unwrap().kind,
                    if case == 2 {
                        FaultKind::Type
                    } else {
                        FaultKind::Resource
                    }
                );
                return observations;
            }
            other => panic!("unexpected publication outcome {other:?}"),
        }
    }
    panic!("publication did not make bounded progress");
}

#[test]
fn payload_publication_preserves_each_slice_cache_roots_work_and_clock() {
    for case in 0..4 {
        for encoded in [false, true] {
            for work in [1, 12, 128] {
                for (clock, instructions) in [(0, 1), (0, 5), (0, 128), (1, 1), (1, 128), (2, 128)]
                {
                    assert_eq!(
                        trace(case, encoded, false, work, clock, instructions),
                        trace(case, encoded, true, work, clock, instructions),
                        "case={case} encoded={encoded} work={work} clock={clock} instructions={instructions}"
                    );
                }
            }
        }
    }
}

#[test]
fn publication_preserves_full_variant_replacement_when_destination_is_native() {
    let mut vm = load(0, false, false);
    let task = vm
        .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
        .unwrap();
    let Frame::Script(source) = vm.active.frames.data[0] else {
        unreachable!()
    };
    loop {
        match vm.run_slice(task, SliceBudget::default(), || 0).unwrap() {
            RunOutcome::BudgetYield(_) => {}
            RunOutcome::HostCall(_) => break,
            other => panic!("expected native call {other:?}"),
        }
    }
    let index = vm.active.frames.data.len() - 1;
    let original = vm.active.frames.data[index];
    assert!(matches!(original, Frame::Native(_)));
    vm.publish_script_frame(index, &source);
    assert_eq!(
        postcard::to_allocvec(&vm.active.frames.data[index]).unwrap(),
        postcard::to_allocvec(&Frame::Script(source)).unwrap()
    );
    vm.active.frames.data[index] = original;
}

fn task_trace(legacy: bool, restore: bool, cancel: Option<bool>) -> Vec<Observation> {
    task_dispatch_trace(legacy, false, restore, cancel)
}

fn task_dispatch_trace(
    legacy: bool,
    dispatch: bool,
    restore: bool,
    cancel: Option<bool>,
) -> Vec<Observation> {
    let mut vm = load(0, true, legacy);
    vm.legacy_body_dispatch = dispatch;
    let mut tasks = [
        vm.start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
            .unwrap(),
        vm.start(fixtures::ENTRY, &[], RootOwner(2), Context::Ordinary)
            .unwrap(),
    ];
    let mut budgets = [SliceBudget {
        instructions: 1,
        work: 128,
        allocation_bytes: 0,
        deadline_ns: None,
    }; 2];
    let mut done = [false; 2];
    let mut restored = false;
    let mut cancelled = false;
    let mut observations = Vec::new();
    for step in 0..2000 {
        let index = step % 2;
        if done[index] {
            continue;
        }
        let outcome = vm.run_slice(tasks[index], budgets[index], || 0);
        observations.push(observe(&vm, 0));
        match outcome {
            Ok(RunOutcome::BudgetYield(usage)) => {
                budgets[index].work = 128.max(usage.required_work);
                budgets[index].allocation_bytes = usage.required_allocation;
                if restore && !restored && usage.instructions != 0 {
                    let registry = SnapshotRegistry::default();
                    let mut capture = vm.snapshot_begin(&registry).unwrap();
                    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
                    let image =
                        VmSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
                    let mut restoring = Vm::restore_begin(
                        &image,
                        vm.program().clone(),
                        review_tests::echo_registry(),
                        vm.limits,
                        SessionEpoch(74),
                        &registry,
                    )
                    .unwrap();
                    while !restoring
                        .step(SnapshotBudget { units: 1 })
                        .unwrap()
                        .complete
                    {}
                    let (next, map) = restoring.finish().unwrap();
                    tasks = tasks.map(|task| map.task(task).unwrap());
                    vm = next;
                    vm.legacy_body_dispatch = dispatch;
                    vm.work_probe.legacy_script_publication = legacy;
                    vm.slots.checked_vm_access = legacy;
                    restored = true;
                }
                if index == 0
                    && !cancelled
                    && (!restore || restored)
                    && let Some(hard) = cancel
                {
                    if hard {
                        vm.cancel_task(tasks[0]).unwrap();
                    } else {
                        vm.cancel_cooperatively(tasks[0]).unwrap();
                    }
                    cancelled = true;
                }
                review_tests::gc(&mut vm);
            }
            Ok(RunOutcome::HostCall(ticket)) => {
                assert_eq!(vm.host_arguments(ticket).unwrap()[0].as_int(), Some(41));
                vm.respond(ticket, HostResponse::Ready(Value::int(42)))
                    .unwrap();
            }
            Ok(RunOutcome::Returned(delivery)) => {
                assert_eq!(vm.delivery_value(delivery).unwrap().as_int(), Some(42));
                assert!(index == 1 || cancel.is_none());
                vm.discard_delivery(delivery).unwrap();
                done[index] = true;
            }
            Ok(RunOutcome::Fault(delivery)) => {
                assert_eq!((index, cancel), (0, Some(false)));
                assert_eq!(
                    vm.delivery_fault(delivery).unwrap().unwrap().kind,
                    FaultKind::Cancelled
                );
                vm.discard_delivery(delivery).unwrap();
                done[index] = true;
            }
            Err(VmError::Idle) => {
                assert_eq!((index, cancel), (0, Some(true)));
                done[index] = true;
            }
            other => panic!("task publication {other:?}"),
        }
        if done == [true; 2] {
            assert!(!restore || restored);
            return observations;
        }
    }
    panic!("interleaved task publication did not complete");
}

#[test]
fn body_publication_retains_task_switch_snapshot_and_cancellation_boundaries() {
    for restore in [false, true] {
        for cancel in [None, Some(false), Some(true)] {
            assert_eq!(
                task_trace(false, restore, cancel),
                task_trace(true, restore, cancel),
                "restore={restore} cancel={cancel:?}"
            );
        }
    }
}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct SnapshotV1ScriptFrame {
    // The original native bridge keeps its logical stack identity while its roots
    // are deferred until the standard derived commit boundary. env[3] owns output.
    derived_bridge: Option<(FrameId, ModuleInstanceId, Effects)>,
    id: FrameId,
    code: CodeRef,
    module: ModuleInstanceId,
    pc: Pc,
    #[serde(skip)]
    instruction_end: Pc,
    #[serde(skip)]
    member_symbol: Option<SymbolId>,
    #[serde(skip)]
    instruction: Option<Instruction>,
    #[serde(skip)]
    instruction_pc: Pc,
    regs: RootRangeToken,
    env: RootRangeToken,
    args: RootRangeToken,
    bound: RootRangeToken,
    argument_count: u32,
    closure: bool,
    shared_parameters: bool,
    // Shared prefix Cells belong to the binder that prepared them, while this
    // frame's code, captures, locals and diagnostics retain its lexical module.
    parameter_module: ModuleInstanceId,
    effects: Effects,
    context: ContextFlags,
    owner: RootOwner,
    ret: ReturnTo,
    phase: ScriptPhase,
    table_work: Option<TableWork>,
    generator: bool,
}

// Retained execution must keep the original snapshot v1 field stream,
// including cache fields that reset when an execution frame is restored.
fn assert_snapshot_v1_field_stream(frame: &ScriptFrame) {
    let original_fields = SnapshotV1ScriptFrame {
        derived_bridge: frame.derived_bridge,
        id: frame.id,
        code: frame.code,
        module: frame.module,
        pc: frame.pc,
        instruction_end: frame.instruction_end,
        member_symbol: frame.member_symbol,
        instruction: frame.instruction,
        instruction_pc: frame.instruction_pc,
        regs: frame.regs,
        env: frame.env,
        args: frame.args,
        bound: frame.bound,
        argument_count: frame.argument_count,
        closure: frame.closure,
        shared_parameters: frame.shared_parameters,
        parameter_module: frame.parameter_module,
        effects: frame.effects,
        context: frame.context,
        owner: frame.owner,
        ret: frame.ret,
        phase: frame.phase,
        table_work: frame.table_work,
        generator: frame.generator,
    };
    let original = postcard::to_allocvec(&original_fields).unwrap();
    assert_eq!(postcard::to_allocvec(frame).unwrap(), original);
    let original_restored: SnapshotV1ScriptFrame = postcard::from_bytes(&original).unwrap();
    assert!(original_restored.instruction.is_none());
    assert!(original_restored.member_symbol.is_none());
    assert_eq!(original_restored.instruction_end, Pc(0));
    assert_eq!(original_restored.instruction_pc, Pc(0));
    let restored: ScriptFrame = postcard::from_bytes(&original).unwrap();
    assert_eq!(postcard::to_allocvec(&restored).unwrap(), original);
    assert!(restored.instruction.is_none());
    assert!(restored.member_symbol.is_none());
    assert_eq!(restored.instruction_end, Pc(0));
    assert_eq!(restored.instruction_pc, Pc(0));
}

#[test]
#[ignore = "execution working-frame layout measurement"]
fn execution_working_layout() {
    println!(
        "execution-layout frame={} script={} v1_script={} phase={} table={} native={}",
        size_of::<Frame>(),
        size_of::<ScriptFrame>(),
        size_of::<SnapshotV1ScriptFrame>(),
        size_of::<ScriptPhase>(),
        size_of::<Option<TableWork>>(),
        size_of::<NativeFrame>(),
    );
}

#[test]
fn retained_body_reuses_working_frame() {
    let run = |legacy, deadline_ns| {
        let mut vm = load(1, true, legacy);
        let task = vm
            .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
            .unwrap();
        let budget = SliceBudget {
            instructions: u32::MAX,
            work: u64::MAX,
            allocation_bytes: u64::MAX,
            deadline_ns,
        };
        assert!(matches!(
            vm.run_slice(task, budget, || 0).unwrap(),
            RunOutcome::Returned(_)
        ));
        (observe(&vm, 0), vm.work_probe.script_working_captures.get())
    };
    for deadline_ns in [None, Some(1)] {
        let (retained, captures) = run(false, deadline_ns);
        let (legacy, old_captures) = run(true, deadline_ns);
        assert_eq!(retained, legacy);
        assert!(
            captures < old_captures,
            "retained={captures} legacy={old_captures}"
        );
        println!("script-working-captures retained={captures} legacy={old_captures}");
    }
}

#[test]
fn call_cursor_commits_once_before_push_and_restores_the_origin_on_failure() {
    for native in [false, true] {
        for fail in [false, true] {
            let mut package = fixtures::helper_native();
            if native {
                let Descriptor::Call(call) = &mut package.functions[0].descriptors[0] else {
                    unreachable!()
                };
                call.target = CallTarget::Native(fixtures::ECHO_NATIVE);
            }
            let mut limits = VmLimits::default();
            if fail {
                limits.max_frames = 1;
            }
            let mut vm = Vm::load(
                verify(package, Limits::default()).unwrap(),
                review_tests::echo_registry(),
                limits,
                SessionEpoch(81),
            )
            .unwrap();
            let task = vm
                .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
                .unwrap();
            let budget = SliceBudget {
                instructions: 1,
                work: u64::MAX,
                allocation_bytes: u64::MAX,
                deadline_ns: None,
            };
            assert!(matches!(
                vm.run_slice(task, budget, || 0).unwrap(),
                RunOutcome::BudgetYield(_)
            ));
            let Frame::Script(source) = vm.active.frames.data[0] else {
                unreachable!()
            };
            let (instruction, next) = vm.program.package().functions[source.code.function.index()]
                .code
                .fetch(source.pc.index())
                .unwrap();
            assert_eq!(instruction.opcode, Opcode::Call as u16);
            vm.work_probe.script_call_cursor_publications.set(0);
            vm.work_probe.script_body_cursor_publications.set(0);
            let mut working = source;
            let mut meter = Meter::new(budget);
            assert!(vm.script_body_step(&mut working, &mut meter).unwrap());
            assert_eq!(meter.usage.instructions, 1);
            assert_eq!(vm.work_probe.script_call_cursor_publications.get(), 1);
            assert_eq!(vm.work_probe.script_body_cursor_publications.get(), 0);
            assert_eq!(
                postcard::to_allocvec(&vm.active.frames.data[0]).unwrap(),
                postcard::to_allocvec(&Frame::Script(working)).unwrap()
            );
            assert_eq!(vm.value(source.regs, 0).unwrap().as_int(), Some(41));
            if fail {
                assert_eq!(vm.active.frames.data.len(), 1);
                assert_eq!(working.pc, source.pc);
                let ScriptPhase::Exit {
                    origin,
                    reason: ExitReason::Throw(seed),
                    ..
                } = working.phase
                else {
                    panic!("push failure must publish a complete exit")
                };
                assert_eq!(origin, source.pc);
                assert_eq!(seed.kind, FaultKind::Resource);
                assert_eq!(seed.location, vm.loc(source.code, source.pc));
            } else {
                assert_eq!(vm.active.frames.data.len(), 2);
                assert_eq!(working.pc, Pc(next as u32));
                let ret = match vm.active.frames.data[1] {
                    Frame::Script(callee) => {
                        assert!(!native);
                        callee.ret
                    }
                    Frame::Native(callee) => {
                        assert!(native);
                        callee.ret
                    }
                };
                assert!(
                    matches!(ret, ReturnTo::Register { frame, call, .. } if frame == source.id && call == source.pc)
                );
            }
        }
    }
}

#[test]
fn vm_access_matches_checked_roots_at_budget_gc_and_clock_boundaries() {
    for case in 0..4 {
        for (clock, instructions, work) in [
            (0, 1, 1),
            (0, 5, 128),
            (0, 128, 128),
            (1, 128, 128),
            (2, 128, 128),
        ] {
            let scoped = load(case, true, false);
            let mut checked = load(case, true, false);
            checked.slots.checked_vm_access = true;
            assert_eq!(
                trace_vm(scoped, case, work, clock, instructions),
                trace_vm(checked, case, work, clock, instructions),
                "case={case} clock={clock} instructions={instructions} work={work}"
            );
        }
    }
}

#[test]
fn vm_access_reuses_provider_proof_across_real_execution() {
    let run = |checked| {
        let mut vm = load(1, true, false);
        vm.slots.checked_vm_access = checked;
        let task = vm
            .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
            .unwrap();
        vm.slots.vm_access_stats.provider_checks.set(0);
        vm.slots.vm_access_stats.heap_proofs.set(0);
        vm.slots.vm_access_stats.window_proofs.set(0);
        let budget = SliceBudget {
            instructions: u32::MAX,
            work: u64::MAX,
            allocation_bytes: u64::MAX,
            deadline_ns: None,
        };
        assert!(matches!(
            vm.run_slice(task, budget, || 0).unwrap(),
            RunOutcome::Returned(_)
        ));
        let counts = (
            vm.slots.vm_access_stats.provider_checks.get(),
            vm.slots.vm_access_stats.heap_proofs.get(),
            vm.slots.vm_access_stats.window_proofs.get(),
        );
        (observe(&vm, 0), counts)
    };
    let (scoped, new) = run(false);
    let (checked, old) = run(true);
    assert_eq!(scoped, checked);
    assert!(new.0 < old.0, "scoped={new:?} checked={old:?}");
    assert!(new.1 > 0 && new.2 > 0);
    println!(
        "slot-access provider-checks scoped={} checked={} heap-proofs={} window-proofs={}",
        new.0, old.0, new.1, new.2
    );
}
