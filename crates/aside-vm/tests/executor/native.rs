//! Native calls, callbacks and host contracts.
use super::*;

// Integration regressions: compiled only while independent VM execution is blocked.
#[test]
fn owner_cancellation_preserves_host_roots_and_isolates_other_tasks() {
    let mut v = vm(fixtures::helper_native());
    let object = v.heap_mut().alloc_array(&[Value::int(7)]).unwrap();
    let root = v
        .heap_mut()
        .add_root(OWNER, RootKind::Host, object)
        .unwrap();
    let other_owner = RootOwner(18);
    let other_root = v
        .heap_mut()
        .add_root(other_owner, RootKind::Host, Value::int(99))
        .unwrap();
    let hole = v
        .heap_mut()
        .add_root(OWNER, RootKind::Host, Value::NULL)
        .unwrap();
    v.heap_mut().release_root(hole).unwrap();
    let issued = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let waiting = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let other = v
        .start(fixtures::ENTRY, &[], other_owner, Context::Ordinary)
        .unwrap();
    let issued_ticket = task_ticket(&mut v, issued);
    let waiting_ticket = task_ticket(&mut v, waiting);
    v.respond(
        waiting_ticket,
        HostResponse::Pending(waiting_ticket.wait_id()),
    )
    .unwrap();
    let other_ticket = task_ticket(&mut v, other);

    // Repeating task cancellation through the owner path uses the same teardown.
    v.cancel_task(issued).unwrap();
    v.cancel(OWNER).unwrap();
    v.cancel(OWNER).unwrap();
    assert!(matches!(
        v.respond(issued_ticket, HostResponse::Ready(Value::NULL)),
        Err(VmError::InvalidTicket)
    ));
    assert!(matches!(
        v.complete(waiting_ticket.wait_id(), Completion::Ready(Value::NULL)),
        Err(VmError::InvalidWait)
    ));
    assert_eq!(
        v.host_arguments(other_ticket).unwrap()[0].as_int(),
        Some(fixtures::ECHO_ARGUMENT)
    );

    let RunOutcome::BudgetYield(usage) = v
        .run_slice(
            issued,
            SliceBudget {
                work: 1,
                ..SliceBudget::default()
            },
            || 0,
        )
        .unwrap()
    else {
        panic!("frame release must wait for sufficient work");
    };
    assert_eq!(usage.work, 0);
    assert!(usage.required_work > 1);
    for task in [issued, waiting] {
        let mut finished = false;
        for _ in 0..100 {
            match v.run_slice(task, SliceBudget::default(), || 0) {
                Ok(RunOutcome::BudgetYield(_)) => {}
                Err(VmError::Idle) => {
                    finished = true;
                    break;
                }
                other => panic!("{other:?}"),
            }
        }
        assert!(finished, "cancelled task must finish budgeted cleanup");
    }
    assert!(matches!(v.cancel(OWNER), Err(VmError::Idle)));
    assert!(!v.is_idle(), "another owner's task remains active");
    collect(&mut v);
    assert!(v.heap().root_value(root).unwrap().same_bits(object));
    assert_eq!(v.heap().get_field(object, 0).unwrap().as_int(), Some(7));

    // Host cleanup proceeds even though the cancelled owner has no TaskId.
    // Descriptor holes consume work too; the return value counts released roots.
    let end = v.heap().root_slots();
    let mut cursor = 0;
    let mut released = 0;
    while cursor < end {
        let before = cursor;
        released += v.heap_mut().release_owner_step(OWNER, &mut cursor, 1);
        assert_eq!(cursor - before, 1);
        assert_eq!(v.heap().root_value(other_root).unwrap().as_int(), Some(99));
    }
    assert_eq!(released, 1);
    assert!(matches!(
        v.heap().root_value(root),
        Err(HeapError::InvalidRoot)
    ));
    collect(&mut v);
    assert!(v.heap().info(object).is_err());
    v.respond(
        other_ticket,
        HostResponse::Ready(Value::int(fixtures::ECHO_RESULT)),
    )
    .unwrap();
    let delivery = task_finish(&mut v, other);
    assert_eq!(
        v.delivery_value(delivery).unwrap().as_int(),
        Some(fixtures::ECHO_RESULT)
    );
    v.discard_delivery(delivery).unwrap();
    v.heap_mut().release_root(other_root).unwrap();
    assert!(v.is_idle());
}
#[test]
fn host_ticket_and_wait_are_scoped_to_vm_epoch_and_current_issue() {
    let mut a = vm(fixtures::helper_native());
    let mut b = vm(fixtures::helper_native());
    a.start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    b.start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let next = |v: &mut Vm| loop {
        match slice(v) {
            RunOutcome::HostCall(t) => break t,
            RunOutcome::BudgetYield(_) => {}
            other => panic!("{other:?}"),
        }
    };
    let ta = next(&mut a);
    let tb = next(&mut b);
    assert!(matches!(
        b.respond(ta, HostResponse::Ready(Value::int(42))),
        Err(VmError::InvalidTicket)
    ));
    assert!(matches!(
        a.respond(ta, HostResponse::Pending(tb.wait_id())),
        Err(VmError::InvalidWait)
    ));
    a.respond(ta, HostResponse::Pending(ta.wait_id())).unwrap();
    assert!(matches!(
        a.complete(tb.wait_id(), Completion::Ready(Value::int(42))),
        Err(VmError::InvalidWait)
    ));
    a.complete(ta.wait_id(), Completion::Ready(Value::int(42)))
        .unwrap();
    finish(&mut a);
}
#[test]
fn repeated_native_calls_resume_without_frame_pool_allocation_or_range_leak() {
    let mut v = vm(fixtures::helper_native());
    warm_code(&v);
    let allocation = allocation_counter::measure(|| {
        for _ in 0..1000 {
            v.start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
                .unwrap();
            loop {
                match v
                    .run_slice(v.task_id().unwrap(), SliceBudget::default(), || 0)
                    .unwrap()
                {
                    RunOutcome::HostCall(t) => {
                        assert!(v.host_continuation(t).unwrap().is_empty());
                        v.respond(t, HostResponse::Ready(Value::int(42))).unwrap()
                    }
                    RunOutcome::BudgetYield(_) => {}
                    RunOutcome::Returned(id) => {
                        v.discard_delivery(id).unwrap();
                        break;
                    }
                    o => panic!("{o:?}"),
                }
            }
        }
    });
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
    assert_eq!(v.stats().slots.active_ranges, v.pooled_root_ranges());
    assert_eq!(v.stats().frames, 0);
    assert_eq!(v.stats().native_entries, 1000);
    assert_eq!(v.stats().native_null_publications, 0);
}
#[test]
fn callback_pending_suspends_same_stack_and_inherits_nowait_dynamically() {
    for restricted in [false, true] {
        let mut p = with_callback(fixtures::helper_native(), false);
        let callback = &mut p.functions[3];
        callback.register_count = 2;
        callback.descriptors = vec![Descriptor::Call(CallDesc {
            contract: None,
            target: CallTarget::Native(NativeId(0)),
            arguments: Arguments::Window(RegWindow {
                start: Reg(0),
                len: 1,
            }),
            names: vec![],
        })];
        callback.code = vec![
            Instruction::new(Opcode::LoadConst, 0, 1, 0),
            Instruction::new(Opcode::Call, 1, 0, 0),
            Instruction::new(Opcode::Return, 1, 0, 0),
        ];
        let mut v = vm(p);
        v.start(FunctionId(2), &[], OWNER, Context::Ordinary)
            .unwrap();
        let id = finish(&mut v);
        let closure = v.delivery_value(id).unwrap();
        let root = v
            .heap_mut()
            .add_root(OWNER, RootKind::Host, Value::NULL)
            .unwrap();
        v.take_delivery_root(id, root).unwrap();
        v.start(
            fixtures::ENTRY,
            &[],
            OWNER,
            if restricted {
                Context::NoWait
            } else {
                Context::Ordinary
            },
        )
        .unwrap();
        let mut outer = None;
        let mut callback_entries = 0;
        let mut waits = 0;
        loop {
            let out = slice(&mut v);
            gc(&mut v);
            match out {
                RunOutcome::BudgetYield(_) => {}
                RunOutcome::HostCall(t) => match t.input {
                    NativeInput::Entry if outer.is_none() => {
                        outer = Some(t.frame);
                        v.respond(
                            t,
                            HostResponse::CallScript {
                                closure,
                                arguments: &[],
                                continuation: Continuation {
                                    faults: FaultRetention::Preserve,
                                    version: 1,
                                    stage: 1,
                                    values: &[],
                                },
                            },
                        )
                        .unwrap();
                    }
                    NativeInput::Entry => {
                        assert_ne!(outer, Some(t.frame));
                        assert_eq!(
                            t.context,
                            ContextFlags::from(if restricted {
                                Context::NoWait
                            } else {
                                Context::Ordinary
                            })
                        );
                        callback_entries += 1;
                        v.respond(t, HostResponse::Pending(t.wait_id())).unwrap();
                    }
                    NativeInput::CallbackReturned | NativeInput::CallbackFault => {
                        assert_eq!(outer, Some(t.frame));
                        if restricted {
                            assert_eq!(t.input, NativeInput::CallbackFault);
                            assert_eq!(
                                v.host_callback_fault(t).unwrap().unwrap().kind,
                                FaultKind::Effect
                            );
                        } else {
                            assert_eq!(t.input, NativeInput::CallbackReturned);
                            assert_eq!(v.host_resume_value(t).unwrap().as_int(), Some(42));
                        }
                        v.respond(t, HostResponse::Ready(Value::int(42))).unwrap();
                    }
                    o => panic!("{o:?}"),
                },
                RunOutcome::Waiting(w) => {
                    assert!(!restricted);
                    waits += 1;
                    for _ in 0..3 {
                        collect(&mut v);
                    }
                    v.complete(w, Completion::Ready(Value::int(42))).unwrap();
                }
                RunOutcome::Returned(id) => {
                    assert_eq!(v.delivery_value(id).unwrap().as_int(), Some(42));
                    break;
                }
                o => panic!("{o:?}"),
            }
        }
        assert_eq!(callback_entries, 1);
        assert_eq!(waits, usize::from(!restricted));
        assert_eq!(v.stats().native_entries, 2);
        assert_eq!(v.stats().native_resumes, 1);
    }
}
#[test]
fn frame_continuation_work_pools_and_inline_control_share_one_budget() {
    let p = fixtures::helper_native();
    let registry = support(&p);
    let program = verify(p, Limits::default()).unwrap();
    let bytes = program.stats().owned_capacity_bytes as u64;
    let mut limits = VmLimits::default();
    limits.heap.max_bytes = bytes + 300_000;
    assert!(matches!(
        Vm::load(program, registry, limits, SessionEpoch(1)),
        Err(VmError::Heap(HeapError::ResourceLimit { .. }))
    ));
    let mut v = vm(fixtures::helper_native());
    let usage = v.heap().budget_usage();
    let before = usage.charged_bytes();
    assert!(v.stats().control_bytes > 0);
    assert!(v.stats().program_bytes > 0);
    v.start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let root = v
        .heap_mut()
        .add_root(OWNER, RootKind::Host, Value::NULL)
        .unwrap();
    let id = loop {
        match slice(&mut v) {
            RunOutcome::HostCall(t) => {
                let state = v.heap_mut().alloc_array(&[Value::int(4); 9]).unwrap();
                v.respond(
                    t,
                    HostResponse::Continue(Continuation {
                        faults: FaultRetention::Preserve,
                        version: 1,
                        stage: 1,
                        values: &[state],
                    }),
                )
                .unwrap();
                break t;
            }
            RunOutcome::BudgetYield(_) => {}
            o => panic!("{o:?}"),
        }
    };
    assert!(usage.charged_bytes() > before);
    v.cancel(OWNER).unwrap();
    loop {
        match v.run_slice(v.task_id().unwrap(), SliceBudget::default(), || 0) {
            Ok(RunOutcome::BudgetYield(_)) => {}
            Err(VmError::Idle) => break,
            o => panic!("{o:?}"),
        }
    }
    // Task teardown does not retire independently retained host roots.
    assert_eq!(v.heap().root_value(root).unwrap().tag(), ValueTag::Null);
    v.heap_mut().release_root(root).unwrap();
    assert!(v.heap().root_value(root).is_err());
    assert!(matches!(
        v.respond(id, HostResponse::Ready(Value::int(42))),
        Err(VmError::InvalidTicket)
    ));
    drop(v);
    assert!(usage.charged_bytes() < before);
}
#[test]
fn vm_load_does_not_trust_erased_native_capabilities() {
    let mut p = fixtures::helper_native();
    let registry = support(&p);
    p.capabilities.clear();
    p.natives[0].capabilities.clear();
    let verified = verify(p, Limits::default()).unwrap();
    assert!(matches!(
        Vm::load(verified, registry, VmLimits::default(), SessionEpoch(1)),
        Err(VmError::Format(Error {
            kind: ErrorKind::Capability,
            ..
        }))
    ));
}
#[test]
fn native_unknown_argument_kind_and_call_pc_are_preserved() {
    let mut p = fixtures::helper_native();
    let f = &mut p.functions[1];
    f.source_map.clear();
    f.register_count = 3;
    let Descriptor::Call(d) = &mut f.descriptors[0] else {
        panic!()
    };
    d.arguments = Arguments::Packed(Reg(2));
    let i = Instruction::new;
    f.code = vec![
        i(Opcode::ArgsNew, 2, 0, 0),
        i(Opcode::ArgsNamed, 2, 0, 9),
        i(Opcode::Call, 1, 0, 0),
        i(Opcode::Return, 1, 0, 0),
    ];
    let mut v = vm(p);
    let task = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let id = task_finish(&mut v, task);
    let fault = v.delivery_fault(id).unwrap().unwrap();
    assert_eq!(fault.kind, FaultKind::UnknownArgument);
    assert_eq!(fault.location.code.function, FunctionId(1));
    assert_instruction(&v, fault.location, 2);
    assert_eq!(v.stats().native_entries, 0);
}
#[test]
fn continuation_dispatch_uses_returned_stage_without_host_round_counter() {
    let mut v = vm(with_callback(fixtures::helper_native(), false));
    let a = v
        .start(FunctionId(2), &[], OWNER, Context::Ordinary)
        .unwrap();
    let d = task_finish(&mut v, a);
    let closure = v.delivery_value(d).unwrap();
    let root = v
        .heap_mut()
        .add_root(OWNER, RootKind::Host, closure)
        .unwrap();
    v.discard_delivery(d).unwrap();
    let task = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    loop {
        match task_slice(&mut v, task) {
            RunOutcome::BudgetYield(_) => {}
            RunOutcome::HostCall(t) => {
                let stage = t.continuation.map_or(0, |c| {
                    assert_eq!(c.version, 1);
                    c.stage
                });
                if stage == 7 {
                    v.respond(t, HostResponse::Ready(Value::int(42))).unwrap();
                } else if stage % 2 == 0 {
                    v.respond(
                        t,
                        HostResponse::Continue(Continuation {
                            faults: FaultRetention::Preserve,
                            version: 1,
                            stage: stage + 1,
                            values: &[],
                        }),
                    )
                    .unwrap();
                } else {
                    v.respond(
                        t,
                        HostResponse::CallScript {
                            closure,
                            arguments: &[],
                            continuation: Continuation {
                                faults: FaultRetention::Preserve,
                                version: 1,
                                stage: stage + 1,
                                values: &[],
                            },
                        },
                    )
                    .unwrap();
                }
            }
            RunOutcome::Returned(d) => {
                assert_eq!(v.delivery_value(d).unwrap().as_int(), Some(42));
                break;
            }
            o => panic!("{o:?}"),
        }
    }
    assert_eq!(v.stats().native_entries, 1);
    assert_eq!(v.stats().native_resumes, 7);
    v.heap_mut().release_root(root).unwrap();
}

#[test]
fn inherited_context_flags_accumulate_and_derived_write_fails_under_nowait_or_init() {
    for parent in [Context::NoWait, Context::Init] {
        let mut p = fixtures::helper_native();
        // A pure native may inspect inherited context; the subsequent write
        // must still fail under the helper's Derived context.
        p.signatures[0].effects = Effects(0);
        p.signatures.push(Signature {
            parameters: vec![Parameter {
                name: SymbolId(4),
                ty: TypeId(0),
                mutable: false,
                default: None,
            }],
            rest: None,
            return_type: TypeId(1),
            effects: Effects::ALL,
            property_bag_tail: None,
        });
        let i = Instruction::new;
        let main = &mut p.functions[0];
        main.signature = SignatureId(2);
        main.source_map.clear();
        main.code = vec![i(Opcode::Call, 1, 0, 0), i(Opcode::Return, 1, 0, 0)];
        let helper = &mut p.functions[1];
        helper.signature = SignatureId(2);
        helper.context = Context::Derived;
        helper.bindings.clear();
        helper.source_map.clear();
        helper.register_count = 3;
        let Descriptor::Call(d) = &mut helper.descriptors[0] else {
            panic!()
        };
        d.arguments = Arguments::Window(RegWindow {
            start: Reg(1),
            len: 1,
        });
        helper.code = vec![
            i(Opcode::LoadConst, 1, 2, 0),
            i(Opcode::Call, 2, 0, 0),
            i(Opcode::CellWrite, 0, 1, 0),
            i(Opcode::Return, 1, 0, 0),
        ];
        let mut registry = support(&p);
        registry.natives[0].signature.effects = Effects(0);
        let mut v = Vm::load(
            verify(p, Limits::default()).unwrap(),
            registry,
            VmLimits::default(),
            SessionEpoch(4),
        )
        .unwrap();
        let cell = v.heap_mut().alloc_cell(Value::int(41)).unwrap();
        let task = v.start(fixtures::ENTRY, &[cell], OWNER, parent).unwrap();
        let ticket = task_ticket(&mut v, task);
        assert!(ticket.context.derived);
        assert_eq!(ticket.context.no_wait, parent == Context::NoWait);
        assert_eq!(ticket.context.init, parent == Context::Init);
        v.respond(ticket, HostResponse::Ready(Value::int(42)))
            .unwrap();
        let d = task_finish(&mut v, task);
        assert_eq!(
            v.delivery_fault(d).unwrap().unwrap().kind,
            FaultKind::Effect
        );
        assert_eq!(v.heap().cell_read(cell).unwrap().as_int(), Some(41));
    }
}

#[test]
fn reused_task_admission_reserves_execution_windows_and_respects_the_host_ceiling() {
    for maximum in [1024, u32::MAX] {
        let mut package = fixtures::helper_native();
        // Keep the live caller close to the ceiling after static native calls
        // remove their duplicated name/value argument window.
        package.functions[0].register_count = 494;
        let mut limits = VmLimits::default();
        limits.slots.initial_capacity = 1024;
        limits.slots.max_capacity = maximum;
        limits.slots.max_window_len = 1024;
        let mut vm = Vm::load(
            verify(package.clone(), Limits::default()).unwrap(),
            support(&package),
            limits,
            SessionEpoch(4),
        )
        .unwrap();
        // The initial pooled task and every subsequent activation reuse the
        // same slot. Caller registers and permanent roots exercise the ceiling.
        let capacity = vm.stats().slots.capacity;
        let pooled = vm.pooled_root_ranges();
        for _ in 0..3 {
            vm.start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
                .unwrap();
            let result = loop {
                let outcome = slice(&mut vm);
                gc(&mut vm);
                match outcome {
                    RunOutcome::BudgetYield(_) => {}
                    RunOutcome::HostCall(ticket) => {
                        assert_eq!(
                            vm.host_arguments(ticket).unwrap()[0].as_int(),
                            Some(fixtures::ECHO_ARGUMENT)
                        );
                        let wait = ticket.wait_id();
                        vm.respond(ticket, HostResponse::Pending(wait)).unwrap();
                        collect(&mut vm);
                        vm.complete(wait, Completion::Ready(Value::int(fixtures::ECHO_RESULT)))
                            .unwrap();
                    }
                    RunOutcome::Returned(result) | RunOutcome::Fault(result) => break result,
                    other => panic!("{other:?}"),
                }
            };
            if maximum == 1024 {
                assert_eq!(
                    vm.delivery_fault(result).unwrap().unwrap().kind,
                    FaultKind::Resource
                );
                assert_eq!(vm.stats().slots.capacity, capacity);
            } else {
                assert_eq!(vm.delivery_fault(result).unwrap(), None);
                assert_eq!(
                    vm.delivery_value(result).unwrap().as_int(),
                    Some(fixtures::ECHO_RESULT)
                );
                assert!(vm.stats().slots.capacity > capacity);
            }
            vm.discard_delivery(result).unwrap();
            assert_eq!(vm.stats().slots.active_ranges, pooled);
        }
    }
}

#[test]
fn admitted_tasks_can_enter_native_calls_after_other_tasks_consume_the_pool() {
    use aside_vm::snapshot::{SnapshotBudget, SnapshotRegistry};
    let mut package = fixtures::helper_native();
    // Exercise actual callee storage instead of the retired 523-slot leaf cost.
    package.functions[1].register_count = 256;
    let mut limits = VmLimits::default();
    limits.slots.initial_capacity = 1024;
    limits.slots.max_window_len = 1024;
    let mut vm = Vm::load(
        verify(package.clone(), Limits::default()).unwrap(),
        support(&package),
        limits,
        SessionEpoch(4),
    )
    .unwrap();
    warm_code(&vm);
    let tasks: Vec<_> = (0..13)
        .map(|_| {
            vm.start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
                .unwrap()
        })
        .collect();
    let mut waits = Vec::new();
    let mut growths = 0;
    for &task in &tasks {
        let ticket = loop {
            let outcome = vm
                .run_slice(
                    task,
                    SliceBudget {
                        instructions: 1,
                        allocation_bytes: 0,
                        ..SliceBudget::default()
                    },
                    || 0,
                )
                .unwrap();
            gc(&mut vm);
            match outcome {
                RunOutcome::HostCall(ticket) => break ticket,
                RunOutcome::BudgetYield(usage) if usage.required_allocation != 0 => {
                    let before = vm.stats();
                    let denied = vm
                        .run_slice(
                            task,
                            SliceBudget {
                                work: usage.required_work - 1,
                                allocation_bytes: usage.required_allocation,
                                ..SliceBudget::default()
                            },
                            || 0,
                        )
                        .unwrap();
                    assert!(matches!(denied, RunOutcome::BudgetYield(_)));
                    assert_eq!(vm.stats().slots.capacity, before.slots.capacity);
                    assert_eq!(vm.stats().slots.active_ranges, before.slots.active_ranges);
                    assert_eq!(vm.stats().native_entries, before.native_entries);
                    let granted = vm
                        .run_slice(
                            task,
                            SliceBudget {
                                instructions: 1,
                                work: usage.required_work,
                                allocation_bytes: usage.required_allocation,
                                ..SliceBudget::default()
                            },
                            || 0,
                        )
                        .unwrap();
                    assert!(matches!(granted, RunOutcome::BudgetYield(_)));
                    assert!(
                        vm.stats().slots.capacity > before.slots.capacity
                            || vm.stats().slots.metadata_bytes > before.slots.metadata_bytes
                    );
                    assert_eq!(vm.stats().usage.allocation_bytes, usage.required_allocation);
                    growths += usize::from(vm.stats().slots.capacity > before.slots.capacity);
                }
                RunOutcome::BudgetYield(_) => {}
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(
            vm.host_arguments(ticket).unwrap()[0].as_int(),
            Some(fixtures::ECHO_ARGUMENT)
        );
        let wait = ticket.wait_id();
        vm.respond(ticket, HostResponse::Pending(wait)).unwrap();
        waits.push(wait);
    }
    assert!(growths > 0);
    let registry = SnapshotRegistry::default();
    let mut capture = vm.snapshot_begin(&registry).unwrap();
    while !capture.step(SnapshotBudget::default()).unwrap().complete {}
    let image = capture.finish().unwrap();
    let mut restore = Vm::restore_begin(
        &image,
        vm.program().clone(),
        support(&package),
        limits,
        SessionEpoch(5),
        &registry,
    )
    .unwrap();
    while !restore.step(SnapshotBudget::default()).unwrap().complete {}
    let (restored, map) = restore.finish().unwrap();
    let restored_tasks = tasks
        .iter()
        .map(|&task| map.task(task).unwrap())
        .collect::<Vec<_>>();
    let restored_waits = waits
        .iter()
        .map(|&wait| map.wait(wait).unwrap())
        .collect::<Vec<_>>();
    for (mut vm, mut tasks, mut waits) in [
        (vm, tasks, waits),
        (restored, restored_tasks, restored_waits),
    ] {
        let cancelled = tasks.pop().unwrap();
        let old_wait = waits.pop().unwrap();
        vm.cancel_task(cancelled).unwrap();
        while !matches!(
            vm.run_slice(cancelled, SliceBudget::default(), || 0),
            Err(VmError::Idle)
        ) {}
        assert!(
            vm.complete(old_wait, Completion::Ready(Value::int(0)))
                .is_err()
        );
        collect(&mut vm);
        for (task, wait) in tasks.into_iter().zip(waits) {
            vm.complete(wait, Completion::Ready(Value::int(fixtures::ECHO_RESULT)))
                .unwrap();
            let result = task_finish(&mut vm, task);
            assert_eq!(vm.delivery_fault(result).unwrap(), None);
            assert_eq!(
                vm.delivery_value(result).unwrap().as_int(),
                Some(fixtures::ECHO_RESULT)
            );
            vm.discard_delivery(result).unwrap();
        }
        assert_eq!(vm.stats().slots.active_ranges, vm.pooled_root_ranges());
    }
}

#[test]
fn one_slice_can_admit_a_deep_script_call_chain_and_its_native_continuation() {
    let mut package = fixtures::helper_native();
    let template = package.functions[1].clone();
    for index in 1..=20 {
        let mut helper = template.clone();
        helper.register_count = 1024;
        if index < 20 {
            let Descriptor::Call(call) = &mut helper.descriptors[0] else {
                unreachable!()
            };
            call.target = CallTarget::Function(FunctionId(index + 1));
        }
        if index == 1 {
            package.functions[1] = helper;
        } else {
            package.functions.push(helper);
        }
    }
    let mut vm = vm(package);
    let task = vm
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let before = vm.stats().slots.capacity;
    let budget = SliceBudget {
        instructions: 100_000,
        work: 10_000_000,
        allocation_bytes: 16 * 1024 * 1024,
        deadline_ns: None,
    };
    let RunOutcome::HostCall(ticket) = vm.run_slice(task, budget, || 0).unwrap() else {
        panic!("deep call did not reach the native");
    };
    assert!(vm.stats().slots.capacity > before);
    assert_eq!(
        vm.host_arguments(ticket).unwrap()[0].as_int(),
        Some(fixtures::ECHO_ARGUMENT)
    );
    vm.respond(
        ticket,
        HostResponse::Ready(Value::int(fixtures::ECHO_RESULT)),
    )
    .unwrap();
    collect(&mut vm);
    let result = task_finish(&mut vm, task);
    assert_eq!(vm.delivery_fault(result).unwrap(), None);
    assert_eq!(
        vm.delivery_value(result).unwrap().as_int(),
        Some(fixtures::ECHO_RESULT)
    );
    vm.discard_delivery(result).unwrap();
    assert_eq!(vm.stats().slots.active_ranges, vm.pooled_root_ranges());
}

#[test]
fn native_value_hot_loop_reuses_identity_without_allocation_or_allocate_effect() {
    let mut p = fixtures::helper_native();
    let callable = add_native_callable(&mut p);
    p.signatures[p.functions[0].signature.index()].effects = Effects(0);
    p.constants = vec![Const::Int(0), Const::Int(1), Const::Int(100_000)];
    let f = &mut p.functions[0];
    f.register_count = 6;
    f.source_map.clear();
    f.descriptors = vec![Descriptor::Closure {
        function: callable,
        captures: vec![],
    }];
    let i = Instruction::new;
    f.code = vec![
        i(Opcode::LoadConst, 0, 0, 0),
        i(Opcode::LoadConst, 1, 1, 0),
        i(Opcode::LoadConst, 2, 2, 0),
        i(Opcode::MakeClosure, 4, 0, 0),
        i(Opcode::MakeClosure, 5, 0, 0),
        i(Opcode::Eq, 3, 4, 5),
        i(Opcode::BranchBool, 3, 7, 11),
        i(Opcode::Add, 0, 0, 1),
        i(Opcode::Lt, 3, 0, 2),
        i(Opcode::BranchBool, 3, 4, 10),
        i(Opcode::Return, 0, 0, 0),
        i(Opcode::Return, 1, 0, 0),
    ];
    let mut v = vm(p);
    warm_code(&v);
    let task = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let mut delivery = None;
    let started = std::time::Instant::now();
    let allocation = allocation_counter::measure(|| {
        loop {
            match v
                .run_slice(
                    task,
                    SliceBudget {
                        allocation_bytes: 0,
                        ..Default::default()
                    },
                    || 0,
                )
                .unwrap()
            {
                RunOutcome::BudgetYield(usage) => assert_eq!(usage.required_allocation, 0),
                RunOutcome::Returned(id) => {
                    delivery = Some(id);
                    break;
                }
                other => panic!("{other:?}"),
            }
        }
    });
    let elapsed = started.elapsed();
    assert_eq!(allocation.count_total, 0);
    assert_eq!(
        v.delivery_value(delivery.unwrap()).unwrap().as_int(),
        Some(100_000)
    );
    v.discard_delivery(delivery.unwrap()).unwrap();
    collect(&mut v);
    assert_eq!(v.heap().stats().live_objects, 1);
    println!(
        "100000 native value reads and identity checks: allocations={}, elapsed={elapsed:?}",
        allocation.count_total
    );
}

#[test]
fn native_callable_host_entry_and_callback_use_exactly_one_frame_per_call() {
    let mut p = fixtures::helper_native();
    let callable = add_native_callable(&mut p);
    let registry = support(&p);
    let version = p.content_version;
    let encoded = encode(&p, Limits::default()).unwrap();
    let mut v = Vm::load(
        VerifiedProgram::decode(&encoded, Limits::default()).unwrap(),
        registry,
        VmLimits {
            max_frames: 2,
            ..VmLimits::default()
        },
        SessionEpoch(4),
    )
    .unwrap();
    let closure = v
        .heap_mut()
        .alloc_record(
            ObjectKind::Closure,
            &[
                Value::int(i64::from(callable.0)),
                Value::int(version as i64),
                Value::int(0),
            ],
        )
        .unwrap();
    let task = v
        .start_closure(closure, &[Value::int(41)], OWNER, Context::Ordinary)
        .unwrap();
    let ticket = task_ticket(&mut v, task);
    assert_eq!(v.stats().frames, 1);
    v.respond(
        ticket,
        HostResponse::CallScript {
            closure,
            arguments: &[Value::int(42)],
            continuation: continue_at(1, FaultRetention::Preserve),
        },
    )
    .unwrap();
    collect(&mut v);
    let nested = task_ticket(&mut v, task);
    assert_eq!(v.stats().frames, 2);
    assert_eq!(v.host_arguments(nested).unwrap()[0].as_int(), Some(42));
    v.respond(nested, HostResponse::Ready(Value::int(42)))
        .unwrap();
    let resumed = task_ticket(&mut v, task);
    assert_eq!(v.stats().frames, 1);
    assert_eq!(resumed.input, NativeInput::CallbackReturned);
    let value = v.host_resume_value(resumed).unwrap();
    v.respond(resumed, HostResponse::Ready(value)).unwrap();
    let delivery = task_finish(&mut v, task);
    assert_eq!(v.delivery_value(delivery).unwrap().as_int(), Some(42));
    v.discard_delivery(delivery).unwrap();
    collect(&mut v);
    assert_eq!(v.stats().slots.active_ranges, v.pooled_root_ranges());
    assert!(v.heap().info(closure).is_err());
    assert_eq!(v.heap().stats().live_objects, 1); // Loaded native function constant.
}
