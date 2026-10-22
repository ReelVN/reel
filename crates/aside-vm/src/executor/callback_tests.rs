//! Callback retirement reuses native resume roots and ordinary sliced cleanup.
use super::*;
use crate::snapshot::{SnapshotBudget, SnapshotRegistry};

fn program(encoded: bool) -> (VerifiedProgram, RuntimeSupport) {
    let mut p = fixtures::helper_native();
    for signature in &mut p.signatures {
        signature.return_type = TypeId(0);
    }
    let i = Instruction::new;
    let main = &mut p.functions[0];
    main.register_count = 3;
    main.code = vec![
        i(Opcode::LoadConst, 0, 1, 0),
        i(Opcode::Call, 1, 0, 0),
        i(Opcode::Leave, 7, 0, 0),
        i(Opcode::LoadConst, 1, 2, 0),
        i(Opcode::Leave, 7, 0, 0),
        i(Opcode::Call, 2, 1, 0),
        i(Opcode::EndFinally, 0, 0, 0),
        i(Opcode::Return, 1, 0, 0),
    ];
    main.descriptors.push(Descriptor::Call(CallDesc {
        contract: None,
        target: CallTarget::Native(NativeId(0)),
        arguments: Arguments::Window(RegWindow {
            start: Reg(0),
            len: 1,
        }),
        names: vec![],
    }));
    main.regions = vec![ExceptionRegion {
        protected: PcRange {
            start: Pc(0),
            end: Pc(3),
        },
        catch: Some(CatchRegion {
            range: PcRange {
                start: Pc(3),
                end: Pc(5),
            },
            exception: Reg(2),
        }),
        finally: Some(PcRange {
            start: Pc(5),
            end: Pc(7),
        }),
    }];
    let mut callback = p.functions[1].clone();
    callback.code = vec![
        i(Opcode::Call, 1, 0, 0),
        i(Opcode::Leave, 5, 0, 0),
        i(Opcode::LoadConst, 1, 1, 0),
        i(Opcode::Throw, 1, 0, 0),
        i(Opcode::EndFinally, 0, 0, 0),
        i(Opcode::Return, 1, 0, 0),
    ];
    callback.regions = vec![ExceptionRegion {
        protected: PcRange {
            start: Pc(0),
            end: Pc(2),
        },
        catch: None,
        // Region retirement must skip this faulting inner finally, while the
        // caller's finally above must still execute exactly once.
        finally: Some(PcRange {
            start: Pc(2),
            end: Pc(5),
        }),
    }];
    p.functions.push(callback);
    let support = RuntimeSupport {
        host_types: vec![],
        capabilities: vec!["host.echo".into()],
        opcodes: vec![],
        natives: vec![NativeSupport {
            symbol: "host::echo".into(),
            abi_version: 1,
            signature: p.signatures[0].clone(),
            required_capabilities: vec!["host.echo".into()],
        }],
    };
    let program = if encoded {
        VerifiedProgram::decode(&encode(&p, Limits::default()).unwrap(), Limits::default()).unwrap()
    } else {
        verify(p, Limits::default()).unwrap()
    };
    (program, support)
}

fn load(encoded: bool) -> (Vm, RuntimeSupport) {
    let (program, support) = program(encoded);
    let vm = Vm::load(
        program,
        support.clone(),
        VmLimits::default(),
        SessionEpoch(1),
    )
    .unwrap();
    (vm, support)
}

fn collect(vm: &mut Vm) {
    while vm.heap.phase() != GcPhase::Idle {
        vm.gc_step(Default::default()).unwrap();
    }
    vm.begin_collection().unwrap();
    while vm.heap.phase() != GcPhase::Idle {
        vm.gc_step(Default::default()).unwrap();
    }
}

fn tick(vm: &mut Vm, task: TaskId) -> Result<RunOutcome, VmError> {
    let out = vm.run_slice(
        task,
        SliceBudget {
            instructions: 1,
            work: vm.last_usage.required_work.max(1),
            allocation_bytes: vm.last_usage.required_allocation,
            ..Default::default()
        },
        || 0,
    );
    if vm.heap.phase() == GcPhase::Idle {
        vm.begin_collection().unwrap();
    }
    vm.gc_step(GcBudget {
        units: 1,
        ..Default::default()
    })
    .unwrap();
    out
}

fn ticket(vm: &mut Vm, task: TaskId) -> HostTicket {
    for _ in 0..10000 {
        match tick(vm, task).unwrap() {
            RunOutcome::BudgetYield(_) => {}
            RunOutcome::HostCall(ticket) => return ticket,
            other => panic!("expected a host callback: {other:?}"),
        }
    }
    panic!("host callback did not settle")
}

fn callback(vm: &mut Vm, ticket: HostTicket) {
    let closure = vm
        .heap_mut()
        .alloc_record(
            ObjectKind::Closure,
            &[Value::int(2), Value::int(1), Value::int(0)],
        )
        .unwrap();
    vm.respond(
        ticket,
        HostResponse::CallScript {
            closure,
            arguments: &[Value::int(41)],
            continuation: Continuation {
                version: 1,
                stage: 7,
                values: &[],
                faults: FaultRetention::Preserve,
            },
        },
    )
    .unwrap();
}

fn waiting(vm: &mut Vm, task: TaskId, ticket: HostTicket) -> WaitId {
    let wait = ticket.wait_id();
    vm.respond(ticket, HostResponse::Pending(wait)).unwrap();
    for _ in 0..100 {
        match tick(vm, task).unwrap() {
            RunOutcome::BudgetYield(_) => {}
            RunOutcome::Waiting(actual) => {
                assert_eq!(actual, wait);
                return wait;
            }
            other => panic!("expected a pending inner native: {other:?}"),
        }
    }
    panic!("inner wait did not settle")
}

fn finish_caller(vm: &mut Vm, task: TaskId) -> DeliveryId {
    let outer_finally = ticket(vm, task);
    assert_eq!(outer_finally.input, NativeInput::Entry);
    assert_eq!(outer_finally.location.code.function, FunctionId(0));
    assert_eq!(outer_finally.location.pc, Pc(5));
    vm.respond(outer_finally, HostResponse::Ready(Value::NULL))
        .unwrap();
    for _ in 0..10000 {
        match tick(vm, task).unwrap() {
            RunOutcome::BudgetYield(_) => {}
            RunOutcome::Returned(delivery) => return delivery,
            other => panic!("outer caller must execute its finally once: {other:?}"),
        }
    }
    panic!("caller did not settle")
}

#[test]
fn finish_retains_value_identity_through_sliced_cleanup_gc_asb_and_restore() {
    for encoded in [false, true] {
        for restored in [false, true] {
            let (mut vm, support) = load(encoded);
            let task = vm
                .start(FunctionId(0), &[], RootOwner(1), Context::Ordinary)
                .unwrap();
            let outer = ticket(&mut vm, task);
            callback(&mut vm, outer);
            let inner = ticket(&mut vm, task);
            let wait = waiting(&mut vm, task, inner);
            let shared = vm.heap_mut().alloc_table().unwrap();
            let result = vm.heap_mut().alloc_array(&[shared, shared]).unwrap();
            let before = vm.heap.stats();
            let ranges = vm.slots.stats().active_ranges;
            let frames = vm.active.frames.data.len();
            let allocation = allocation_counter::measure(|| {
                assert!(vm.finish_host_callback(task, outer.frame, result).unwrap());
                assert!(
                    !vm.finish_host_callback(task, outer.frame, Value::NULL)
                        .unwrap()
                );
                assert!(!vm.cancel_host_callback(task, outer.frame).unwrap());
            });
            assert_eq!(allocation.count_total, 0);
            assert_eq!(vm.heap.stats().charged_bytes, before.charged_bytes);
            assert_eq!(vm.slots.stats().active_ranges, ranges);
            assert_eq!(vm.active.frames.data.len(), frames);
            assert!(matches!(
                vm.complete(wait, Completion::Ready(Value::NULL)),
                Err(VmError::InvalidWait)
            ));
            assert!(matches!(
                vm.respond(inner, HostResponse::Ready(Value::NULL)),
                Err(VmError::InvalidTicket)
            ));
            let RunOutcome::BudgetYield(usage) = vm
                .run_slice(
                    task,
                    SliceBudget {
                        instructions: 0,
                        work: 0,
                        allocation_bytes: 0,
                        ..Default::default()
                    },
                    || 0,
                )
                .unwrap()
            else {
                panic!("retirement must wait for its work grant")
            };
            assert!(usage.required_work > 0);
            assert_eq!(vm.active.frames.data.len(), frames);
            assert!(matches!(
                vm.run_slice(
                    task,
                    SliceBudget {
                        instructions: 0,
                        work: usage.required_work,
                        allocation_bytes: 0,
                        ..Default::default()
                    },
                    || 0,
                )
                .unwrap(),
                RunOutcome::BudgetYield(_)
            ));
            assert_eq!(vm.active.frames.data.len(), frames - 1);
            collect(&mut vm);
            assert_eq!(vm.heap.info(result).unwrap().kind, ObjectKind::Array);
            let (mut vm, task, result) = if restored {
                let registry = SnapshotRegistry::default();
                let mut capture = vm.snapshot_begin(&registry).unwrap();
                while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
                let image = capture.finish().unwrap();
                let mut restore = Vm::restore_begin(
                    &image,
                    vm.program().clone(),
                    support,
                    VmLimits::default(),
                    SessionEpoch(2),
                    &registry,
                )
                .unwrap();
                loop {
                    let progress = restore.step(SnapshotBudget { units: 1 }).unwrap();
                    assert!(progress.units <= 1);
                    if progress.complete {
                        break;
                    }
                }
                let (vm, map) = restore.finish().unwrap();
                (vm, map.task(task).unwrap(), map.value(result).unwrap())
            } else {
                (vm, task, result)
            };
            let resumed = ticket(&mut vm, task);
            assert_eq!(resumed.frame, outer.frame);
            assert_eq!(resumed.input, NativeInput::CallbackReturned);
            assert_eq!(resumed.continuation.unwrap().stage, 7);
            assert!(vm.host_resume_value(resumed).unwrap().same_bits(result));
            assert!(vm.host_callback_fault(resumed).unwrap().is_none());
            vm.respond(resumed, HostResponse::Ready(result)).unwrap();
            let delivery = finish_caller(&mut vm, task);
            assert!(
                !vm.finish_host_callback(task, outer.frame, Value::NULL)
                    .unwrap()
            );
            let value = vm.delivery_value(delivery).unwrap();
            assert!(value.same_bits(result));
            assert!(
                vm.heap
                    .get_field(value, 0)
                    .unwrap()
                    .same_bits(vm.heap.get_field(value, 1).unwrap())
            );
            assert_eq!(vm.native_entries, 3, "no callback entrance was replayed");
            vm.discard_delivery(delivery).unwrap();
            collect(&mut vm);
            assert_eq!(vm.heap.stats().live_objects, 0);
            assert_eq!(vm.slots.stats().active_ranges, vm.pooled_root_ranges());
        }
    }
}

#[test]
fn outer_retirement_overrides_inner_outcome_and_hard_cancel_overrides_finish() {
    for inner_return in [false, true] {
        for outer_return in [false, true] {
            let (mut vm, _) = load(false);
            let task = vm
                .start(FunctionId(0), &[], RootOwner(1), Context::Ordinary)
                .unwrap();
            let outer = ticket(&mut vm, task);
            callback(&mut vm, outer);
            let inner = ticket(&mut vm, task);
            callback(&mut vm, inner);
            let deepest = ticket(&mut vm, task);
            let wait = waiting(&mut vm, task, deepest);
            assert!(
                if inner_return {
                    vm.finish_host_callback(task, inner.frame, Value::int(7))
                } else {
                    vm.cancel_host_callback(task, inner.frame)
                }
                .unwrap()
            );
            assert!(
                if outer_return {
                    vm.finish_host_callback(task, outer.frame, Value::int(9))
                } else {
                    vm.cancel_host_callback(task, outer.frame)
                }
                .unwrap()
            );
            assert!(
                !vm.finish_host_callback(task, inner.frame, Value::NULL)
                    .unwrap()
            );
            assert!(!vm.cancel_host_callback(task, inner.frame).unwrap());
            assert!(matches!(
                vm.complete(wait, Completion::Ready(Value::NULL)),
                Err(VmError::InvalidWait)
            ));
            let resumed = ticket(&mut vm, task);
            assert_eq!(resumed.frame, outer.frame);
            if outer_return {
                assert_eq!(resumed.input, NativeInput::CallbackReturned);
                let result = vm.host_resume_value(resumed).unwrap();
                assert_eq!(result.as_int(), Some(9));
                vm.respond(resumed, HostResponse::Ready(result)).unwrap();
            } else {
                assert_eq!(resumed.input, NativeInput::CallbackCancelled);
                vm.respond_language_error(resumed, FaultKind::Cancelled, Value::NULL)
                    .unwrap();
            }
            let delivery = finish_caller(&mut vm, task);
            assert_eq!(
                vm.delivery_value(delivery).unwrap().as_int(),
                Some(if outer_return { 9 } else { 1 })
            );
            vm.discard_delivery(delivery).unwrap();
            collect(&mut vm);
            assert_eq!(vm.heap.stats().live_objects, 0);
        }
    }

    let (mut vm, _) = load(false);
    let task = vm
        .start(FunctionId(0), &[], RootOwner(1), Context::Ordinary)
        .unwrap();
    let outer = ticket(&mut vm, task);
    callback(&mut vm, outer);
    let inner = ticket(&mut vm, task);
    waiting(&mut vm, task, inner);
    let value = vm.heap_mut().alloc_array(&[]).unwrap();
    assert!(vm.finish_host_callback(task, outer.frame, value).unwrap());
    vm.cancel_task(task).unwrap();
    assert!(
        !vm.finish_host_callback(task, outer.frame, Value::NULL)
            .unwrap()
    );
    loop {
        match tick(&mut vm, task) {
            Ok(RunOutcome::BudgetYield(_)) => {}
            Err(VmError::Idle) => break,
            other => panic!("hard cancellation must retire the whole task: {other:?}"),
        }
    }
    collect(&mut vm);
    assert_eq!(vm.heap.stats().live_objects, 0);
    assert_eq!(vm.slots.stats().active_ranges, vm.pooled_root_ranges());
}

#[test]
fn finish_checks_result_before_request_and_reuses_the_existing_boundary_storage() {
    assert_eq!(
        size_of::<Option<CallbackBoundary>>(),
        size_of::<Option<FrameId>>()
    );
    let (mut vm, _) = load(false);
    let task = vm
        .start(FunctionId(0), &[], RootOwner(1), Context::Ordinary)
        .unwrap();
    let outer = ticket(&mut vm, task);
    callback(&mut vm, outer);
    let inner = ticket(&mut vm, task);
    let mut foreign = Heap::new(SessionEpoch(1), Default::default()).unwrap();
    let value = foreign.alloc_array(&[]).unwrap();
    assert!(vm.finish_host_callback(task, outer.frame, value).is_err());
    assert!(!vm.active.cancelling);
    assert!(vm.active.cancel_boundary.is_none());
    assert_eq!(vm.host_arguments(inner).unwrap()[0].as_int(), Some(41));
    assert!(
        vm.finish_host_callback(task, outer.frame, Value::NULL)
            .unwrap()
    );
    let resumed = ticket(&mut vm, task);
    assert_eq!(resumed.input, NativeInput::CallbackReturned);
    assert_eq!(vm.host_resume_value(resumed).unwrap().tag(), ValueTag::Null);
}

#[test]
fn restore_rejects_missing_non_native_inactive_and_unrooted_callback_boundaries() {
    for encoded in [false, true] {
        for forged in ["missing", "script", "inactive", "result", "running"] {
            let (mut vm, support) = load(encoded);
            let task = vm
                .start(FunctionId(0), &[], RootOwner(1), Context::Ordinary)
                .unwrap();
            let outer = ticket(&mut vm, task);
            callback(&mut vm, outer);
            ticket(&mut vm, task);
            let result = vm.heap_mut().alloc_array(&[]).unwrap();
            assert!(vm.finish_host_callback(task, outer.frame, result).unwrap());
            match forged {
                "missing" => {
                    vm.active.cancel_boundary = Some(CallbackBoundary::Returned(FrameId(u64::MAX)));
                }
                "script" => {
                    vm.active.cancel_boundary =
                        Some(CallbackBoundary::Returned(vm.active.frames.data[0].id()));
                }
                "inactive" | "result" => {
                    let Frame::Native(mut native) = vm.active.frames.data[2] else {
                        panic!("outer native frame")
                    };
                    assert_eq!(native.id, outer.frame);
                    if forged == "inactive" {
                        native.continuation_active = false;
                        vm.active.frames.data[2] = Frame::Native(native);
                    } else {
                        vm.clear(native.roots, 0).unwrap();
                    }
                }
                "running" => vm.active.cancelling = false,
                _ => unreachable!(),
            }
            let registry = SnapshotRegistry::default();
            let mut capture = vm.snapshot_begin(&registry).unwrap();
            while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
            let image = capture.finish().unwrap();
            let mut restore = Vm::restore_begin(
                &image,
                vm.program().clone(),
                support,
                VmLimits::default(),
                SessionEpoch(2),
                &registry,
            )
            .unwrap();
            let error = loop {
                match restore.step(SnapshotBudget { units: 1 }) {
                    Ok(progress) => {
                        assert!(!progress.complete, "forged {forged} boundary accepted")
                    }
                    Err(error) => break error,
                }
            };
            assert!(
                matches!(
                    error,
                    crate::snapshot::SnapshotError::Invalid(_)
                        | crate::snapshot::SnapshotError::Vm(VmError::Heap(
                            HeapError::UninitializedSlot
                        ))
                ),
                "{forged}: {error:?}"
            );
        }
    }
}
