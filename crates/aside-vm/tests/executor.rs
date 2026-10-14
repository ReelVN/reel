use aside_vm::{bytecode::*, executor::*, *};
const OWNER: RootOwner = RootOwner(17);
fn support(_p: &BytecodePackage) -> RuntimeSupport {
    // This fixture adapter is declared independently of package native metadata.
    // Its fixed fixture symbol/type table is Any=0, Int=1, x=4.
    RuntimeSupport {
        opcodes: vec![],
        capabilities: vec!["host.echo".into()],
        natives: vec![NativeSupport {
            symbol: "host::echo".into(),
            abi_version: 1,
            signature: Signature {
                parameters: vec![Parameter {
                    name: SymbolId(4),
                    ty: TypeId(1),
                    mutable: false,
                    default: None,
                }],
                rest: None,
                return_type: TypeId(1),
                effects: Effects::ALL,
                property_bag_tail: None,
            },
            required_capabilities: vec!["host.echo".into()],
        }],
    }
}
fn vm(p: BytecodePackage) -> Vm {
    let registry = support(&p);
    Vm::load(
        verify(p, Limits::default()).unwrap(),
        registry,
        VmLimits::default(),
        SessionEpoch(4),
    )
    .unwrap()
}
fn gc(vm: &mut Vm) {
    if vm.heap().phase() == GcPhase::Idle {
        vm.begin_collection().unwrap();
    }
    vm.gc_step(GcBudget {
        units: 1,
        ..GcBudget::default()
    })
    .unwrap();
}
fn collect(vm: &mut Vm) {
    while vm.heap().phase() != GcPhase::Idle {
        gc(vm);
    }
    vm.begin_collection().unwrap();
    while vm.heap().phase() != GcPhase::Idle {
        gc(vm);
    }
}
fn slice(vm: &mut Vm) -> RunOutcome {
    vm.run_slice(
        vm.task_id().unwrap(),
        SliceBudget {
            instructions: 1,
            ..SliceBudget::default()
        },
        || 0,
    )
    .unwrap()
}
fn finish(vm: &mut Vm) -> DeliveryId {
    for _ in 0..10000 {
        let out = slice(vm);
        gc(vm);
        match out {
            RunOutcome::Returned(id) => return id,
            RunOutcome::BudgetYield(_) => {}
            other => panic!("{other:?}"),
        }
    }
    panic!("did not finish")
}
#[test]
fn unmatched_branch_reports_a_typed_fault() {
    let mut p = fixtures::helper_native();
    p.functions[0].code[0] = Instruction::new(Opcode::MatchFail, 0, 0, 0);
    let mut v = vm(p);
    v.start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    loop {
        match slice(&mut v) {
            RunOutcome::BudgetYield(_) => gc(&mut v),
            RunOutcome::Fault(id) => {
                let fault = v.delivery_fault(id).unwrap().unwrap();
                assert_eq!(fault.kind, FaultKind::MatchError);
                assert_eq!(fault.location.code.function, fixtures::ENTRY);
                assert_eq!(fault.location.pc, Pc(0));
                v.discard_delivery(id).unwrap();
                break;
            }
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn module_roots_do_not_consume_the_execution_window_allowance() {
    let mut p = fixtures::helper_native();
    p.constants.extend((0..9_000).map(Const::Int));
    let last = p.constants.len() as u32 - 1;
    p.functions[0].code[0] = Instruction::new(Opcode::LoadConst, 0, last, 0);
    p.functions[0].code[1] = Instruction::new(Opcode::Move, 1, 0, 0);
    let static_roots = p.constants.len() + p.symbols.len();
    let mut v = vm(p);
    assert_eq!(
        v.stats().slots.capacity as usize,
        VmLimits::default().slots.initial_capacity as usize + static_roots
    );
    v.start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let result = finish(&mut v);
    assert_eq!(v.delivery_value(result).unwrap().as_int(), Some(8_999));
    v.discard_delivery(result).unwrap();
    assert!(v.stats().heap.charged_bytes > 0);
}

#[test]
fn same_shared_helper_fixture_ready_pending_with_budget_one_and_gc() {
    for completion in fixtures::ECHO_COMPLETIONS {
        let mut v = vm(fixtures::helper_native());
        v.start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
            .unwrap();
        let mut entries = 0;
        let mut wait = None;
        let result = loop {
            let out = slice(&mut v);
            gc(&mut v);
            match out {
                RunOutcome::BudgetYield(_) => {}
                RunOutcome::HostCall(t) => {
                    entries += 1;
                    assert_eq!(
                        v.host_arguments(t).unwrap()[0].as_int(),
                        Some(fixtures::ECHO_ARGUMENT)
                    );
                    if completion == fixtures::EchoCompletion::Ready {
                        v.respond(t, HostResponse::Ready(Value::int(fixtures::ECHO_RESULT)))
                            .unwrap();
                    } else {
                        let w = t.wait_id();
                        v.respond(t, HostResponse::Pending(w)).unwrap();
                        wait = Some(w);
                    }
                    assert!(matches!(
                        v.respond(t, HostResponse::Ready(Value::NULL)),
                        Err(VmError::InvalidTicket)
                    ));
                }
                RunOutcome::Waiting(w) => {
                    assert_eq!(wait, Some(w));
                    for _ in 0..3 {
                        collect(&mut v);
                    }
                    v.complete(w, Completion::Ready(Value::int(fixtures::ECHO_RESULT)))
                        .unwrap();
                    assert!(matches!(
                        v.complete(w, Completion::Ready(Value::NULL)),
                        Err(VmError::InvalidWait)
                    ));
                }
                RunOutcome::Returned(id) => break id,
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(
            v.delivery_value(result).unwrap().as_int(),
            Some(fixtures::ECHO_RESULT)
        );
        assert_eq!(entries, 1);
        assert_eq!(v.stats().native_entries, 1);
        assert_eq!(v.stats().frames, 0);
        assert_eq!(v.stats().slots.active_ranges, v.pooled_root_ranges());
        v.discard_delivery(result).unwrap();
        assert!(matches!(
            v.delivery_value(result),
            Err(VmError::InvalidDelivery)
        ));
    }
}
#[test]
fn shared_cell_closures_and_definition_capture_defaults() {
    for package in [fixtures::cell_closures(), fixtures::captured_default()] {
        let mut v = vm(package);
        v.start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
            .unwrap();
        let id = finish(&mut v);
        let closure = v.delivery_value(id).unwrap();
        let root = v
            .heap_mut()
            .add_root(OWNER, RootKind::Host, Value::NULL)
            .unwrap();
        v.take_delivery_root(id, root).unwrap();
        for round in 0..2 {
            collect(&mut v);
            v.start_closure(closure, &[], OWNER, Context::Ordinary)
                .unwrap();
            let result = finish(&mut v);
            let expected = if v.program().package().functions.len() == 2 {
                fixtures::ECHO_ARGUMENT + round + 1
            } else {
                fixtures::ECHO_ARGUMENT
            };
            assert_eq!(v.delivery_value(result).unwrap().as_int(), Some(expected));
            v.discard_delivery(result).unwrap();
        }
        v.heap_mut().release_root(root).unwrap();
        collect(&mut v);
        assert!(v.heap().info(closure).is_err());
    }
}
#[test]
fn shared_duplicate_fixture_evaluates_native_before_final_binding_fault() {
    let mut v = vm(fixtures::duplicate_arguments());
    v.start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let mut entries = 0;
    loop {
        match slice(&mut v) {
            RunOutcome::BudgetYield(_) => {}
            RunOutcome::HostCall(t) => {
                entries += 1;
                v.respond(t, HostResponse::Ready(Value::int(42))).unwrap();
            }
            RunOutcome::Fault(id) => {
                assert_eq!(
                    v.delivery_fault(id).unwrap().unwrap().kind,
                    FaultKind::DuplicateArgument
                );
                break;
            }
            other => panic!("{other:?}"),
        };
        gc(&mut v);
    }
    assert_eq!(entries, 1);
}
#[test]
fn shared_exception_regions_really_execute_catch_and_finally() {
    let mut v = vm(fixtures::exception_regions());
    v.start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let result = finish(&mut v);
    assert_eq!(v.delivery_value(result).unwrap().as_int(), Some(41));
    assert_eq!(v.stats().unwinds, 0);
}
#[test]
fn language_cell_contract_cannot_be_bypassed_by_generic_fields() {
    let mut h = Heap::new(SessionEpoch(5), HeapLimits::default()).unwrap();
    let contract = CellContract {
        mutable: false,
        ty: BuiltinType::Int,
        type_id: Some(TypeId(7)),
        module: ModuleId(3),
        code_version: 19,
        module_version: 23,
    };
    let cell = h.alloc_language_cell(contract).unwrap();
    assert!(!h.cell_info(cell).unwrap().initialized);
    assert_eq!(h.cell_read(cell).unwrap_err(), HeapError::UninitializedCell);
    assert_eq!(
        h.set_field(cell, 0, Value::int(9)).unwrap_err(),
        HeapError::UninitializedCell
    );
    assert_eq!(
        h.cell_initialize(cell, Value::NULL).unwrap_err(),
        HeapError::TypeMismatch
    );
    h.cell_initialize(cell, Value::int(9)).unwrap();
    assert_eq!(
        h.cell_initialize(cell, Value::int(10)).unwrap_err(),
        HeapError::AlreadyInitialized
    );
    assert_eq!(
        h.set_field(cell, 0, Value::int(10)).unwrap_err(),
        HeapError::ReadOnly
    );
    assert_eq!(h.cell_info(cell).unwrap().contract, contract);
    let null = h
        .alloc_language_cell(CellContract {
            ty: BuiltinType::Any,
            ..contract
        })
        .unwrap();
    h.cell_initialize(null, Value::NULL).unwrap();
    assert!(h.cell_info(null).unwrap().initialized);
    assert_eq!(h.cell_read(null).unwrap().tag(), ValueTag::Null);
}
#[test]
fn dynamic_nowait_allows_ready_and_faults_actual_pending_at_original_call() {
    for pending in [false, true] {
        let mut v = vm(fixtures::helper_native());
        v.start(fixtures::ENTRY, &[], OWNER, Context::NoWait)
            .unwrap();
        loop {
            match slice(&mut v) {
                RunOutcome::BudgetYield(_) => {}
                RunOutcome::HostCall(t) => {
                    assert_eq!(t.location.code.function, FunctionId(1));
                    assert_eq!(t.location.pc, Pc(0));
                    if pending {
                        v.respond(t, HostResponse::Pending(t.wait_id())).unwrap();
                    } else {
                        v.respond(t, HostResponse::Ready(Value::int(42))).unwrap();
                    }
                }
                RunOutcome::Returned(id) => {
                    assert!(!pending);
                    assert_eq!(v.delivery_value(id).unwrap().as_int(), Some(42));
                    break;
                }
                RunOutcome::Fault(id) => {
                    assert!(pending);
                    let info = v.delivery_fault(id).unwrap().unwrap();
                    assert_eq!(info.kind, FaultKind::Effect);
                    assert_eq!(info.location.code.function, FunctionId(1));
                    assert_eq!(info.location.pc, Pc(0));
                    break;
                }
                other => panic!("{other:?}"),
            }
        }
    }
}
#[test]
fn bare_delivery_stays_rooted_and_wait_arguments_survive_collections() {
    let mut p = fixtures::helper_native();
    p.signatures[0].parameters[0].ty = TypeId(0);
    p.signatures[0].return_type = TypeId(0);
    p.signatures[1].return_type = TypeId(0);
    p.functions[1].bindings[0].ty = TypeId(0);
    p.constants[1] = Const::Utf8("rooted argument".into());
    let mut adapter = support(&p);
    // This separate host adapter explicitly accepts/returns Any for rooting tests.
    adapter.natives[0].signature.parameters[0].ty = TypeId(0);
    adapter.natives[0].signature.return_type = TypeId(0);
    let mut v = Vm::load(
        verify(p, Limits::default()).unwrap(),
        adapter,
        VmLimits::default(),
        SessionEpoch(4),
    )
    .unwrap();
    v.start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let (ticket, arg) = loop {
        match slice(&mut v) {
            RunOutcome::BudgetYield(_) => {}
            RunOutcome::HostCall(t) => break (t, v.host_arguments(t).unwrap()[0]),
            other => panic!("{other:?}"),
        }
    };
    let handle = v.heap().handle(arg).unwrap();
    v.respond(ticket, HostResponse::Pending(ticket.wait_id()))
        .unwrap();
    for _ in 0..3 {
        collect(&mut v);
    }
    assert!(v.heap().import_handle(handle).is_ok());
    v.complete(ticket.wait_id(), Completion::Ready(arg))
        .unwrap();
    let result = finish(&mut v);
    let raw = v.delivery_value(result).unwrap();
    for _ in 0..3 {
        collect(&mut v);
    }
    assert!(v.heap().info(raw).is_ok());
    v.discard_delivery(result).unwrap();
    // Constant storage remains rooted for the program lifetime. Check a fresh host result instead.
    v.start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let ticket = loop {
        match slice(&mut v) {
            RunOutcome::HostCall(t) => break t,
            RunOutcome::BudgetYield(_) => {}
            o => panic!("{o:?}"),
        }
    };
    let result = v.heap_mut().alloc_array(&[Value::int(7)]).unwrap();
    v.respond(ticket, HostResponse::Ready(result)).unwrap();
    let id = finish(&mut v);
    let raw = v.delivery_value(id).unwrap();
    collect(&mut v);
    assert_eq!(v.heap().get_field(raw, 0).unwrap().as_int(), Some(7));
    v.discard_delivery(id).unwrap();
    collect(&mut v);
    assert!(v.heap().info(raw).is_err());
}
#[test]
fn cancellation_is_bounded_and_invalidates_ticket_immediately() {
    let mut v = vm(fixtures::helper_native());
    v.start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let ticket = loop {
        match slice(&mut v) {
            RunOutcome::HostCall(t) => break t,
            RunOutcome::BudgetYield(_) => {}
            o => panic!("{o:?}"),
        }
    };
    v.cancel(OWNER).unwrap();
    assert!(matches!(
        v.respond(ticket, HostResponse::Ready(Value::int(42))),
        Err(VmError::InvalidTicket)
    ));
    let tiny = SliceBudget {
        instructions: 1,
        work: 1,
        ..SliceBudget::default()
    };
    let RunOutcome::BudgetYield(cost) = v.run_slice(v.task_id().unwrap(), tiny, || 0).unwrap()
    else {
        panic!()
    };
    assert!(cost.required_work > 1);
    assert_eq!(cost.work, 0);
    let mut steps = 0;
    loop {
        steps += 1;
        match v.run_slice(
            v.task_id().unwrap(),
            SliceBudget {
                instructions: 1,
                work: 4 * (4 * 1024 + 8),
                ..SliceBudget::default()
            },
            || 0,
        ) {
            Ok(RunOutcome::BudgetYield(u)) => assert!(u.work <= 4 * (4 * 1024 + 8)),
            Err(VmError::Idle) => break,
            other => panic!("{other:?}"),
        }
    }
    assert!(steps <= 4);
    assert_eq!(v.stats().frames, 0);
    assert_eq!(v.stats().slots.active_ranges, v.pooled_root_ranges());
}

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
fn owner_with_only_a_fault_root_requires_explicit_host_cleanup() {
    let mut v = vm(fixtures::uninitialized_cell_read());
    let task = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let delivery = task_finish(&mut v, task);
    let root = v.take_fault_delivery(delivery, OWNER).unwrap();
    let fault = root.id();
    assert!(v.is_idle());
    assert!(matches!(v.cancel(OWNER), Err(VmError::Idle)));
    collect(&mut v);
    assert_eq!(root.info(v.heap()).unwrap().id, fault);

    let end = v.heap().root_slots();
    let mut cursor = 0;
    let mut released = 0;
    while cursor < end {
        let before = cursor;
        released += v.heap_mut().release_owner_step(OWNER, &mut cursor, 1);
        assert_eq!(cursor - before, 1);
    }
    assert_eq!(released, 1);
    assert!(matches!(root.info(v.heap()), Err(HeapError::InvalidRoot)));
    collect(&mut v);
    assert!(v.heap().fault_info(fault).is_err());
    // The host retires this invalidated wrapper rather than releasing it twice.
    drop(root);
}

#[test]
fn native_default_type_failure_and_partial_preparation_resource_failure_never_issue_entry() {
    for limited_frames in [false, true] {
        let mut p = fixtures::native_parameter_defaults();
        if !limited_frames {
            p.functions[1].code[4] = Instruction::new(Opcode::LoadConst, 3, 0, 0);
            p.functions[1]
                .code
                .push(Instruction::new(Opcode::Return, 3, 0, 0));
        }
        let mut limits = VmLimits::default();
        if limited_frames {
            limits.max_frames = 2;
        }
        let mut v = Vm::load(
            verify(p, Limits::default()).unwrap(),
            native_default_registry(false),
            limits,
            SessionEpoch(4),
        )
        .unwrap();
        let task = v
            .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
            .unwrap();
        let d = task_finish(&mut v, task);
        let expected = if limited_frames {
            FaultKind::Resource
        } else {
            FaultKind::Type
        };
        let info = v.delivery_fault(d).unwrap().unwrap();
        assert_eq!(info.kind, expected);
        assert_eq!(v.stats().native_entries, 0);
        assert_eq!(
            v.heap()
                .core_error_info(v.delivery_value(d).unwrap())
                .unwrap()
                .code
                .kind,
            expected
        );
        if limited_frames {
            assert_eq!(info.location.pc, Pc(1));
            assert_eq!(info.location.code.function, FunctionId(0));
        }
        v.discard_delivery(d).unwrap();
        collect(&mut v);
        assert_eq!(v.heap().stats().live_objects, 0);
        assert_eq!(v.stats().slots.active_ranges, v.pooled_root_ranges());
    }
}

#[test]
fn native_rest_collects_only_tail_positional_arguments_with_defaults_skipped() {
    let mut p = fixtures::native_parameter_defaults();
    let f = &mut p.functions[0];
    f.register_count = 6;
    f.code = vec![
        Instruction::new(Opcode::LoadConst, 0, 1, 0),
        Instruction::new(Opcode::LoadConst, 1, 2, 0),
        Instruction::new(Opcode::LoadConst, 2, 1, 0),
        Instruction::new(Opcode::LoadConst, 3, 2, 0),
        Instruction::new(Opcode::LoadConst, 4, 1, 0),
        Instruction::new(Opcode::Call, 5, 0, 0),
        Instruction::new(Opcode::Return, 5, 0, 0),
    ];
    let Descriptor::Call(call) = &mut f.descriptors[0] else {
        panic!()
    };
    call.arguments = Arguments::Window(RegWindow {
        start: Reg(0),
        len: 5,
    });
    let mut v = native_default_vm(p, false);
    let task = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let mut work = 1;
    let mut allocation_bytes = 0;
    let t = loop {
        match v
            .run_slice(
                task,
                SliceBudget {
                    instructions: 1,
                    work,
                    allocation_bytes,
                    ..SliceBudget::default()
                },
                || 0,
            )
            .unwrap()
        {
            RunOutcome::BudgetYield(u) => {
                work = u.required_work.max(1);
                allocation_bytes = u.required_allocation;
                gc(&mut v);
            }
            RunOutcome::HostCall(t) => break t,
            other => panic!("{other:?}"),
        }
    };
    let args = v.host_arguments(t).unwrap();
    assert_eq!(args[0].as_int(), Some(41));
    assert_eq!(args[1].as_int(), Some(1));
    assert_eq!(args[2].as_int(), Some(41));
    let rest = args[3];
    assert_eq!(v.heap().info(rest).unwrap().len, 2);
    assert_eq!(v.heap().get_field(rest, 0).unwrap().as_int(), Some(1));
    assert_eq!(v.heap().get_field(rest, 1).unwrap().as_int(), Some(41));
    collect(&mut v);
    v.cancel_task(task).unwrap();
    loop {
        match v.run_slice(
            task,
            SliceBudget {
                instructions: 1,
                work: 16416,
                ..SliceBudget::default()
            },
            || 0,
        ) {
            Ok(RunOutcome::BudgetYield(u)) => assert!(u.work <= 16416),
            Err(VmError::Idle) => break,
            other => panic!("{other:?}"),
        }
    }
    collect(&mut v);
    assert!(v.heap().info(rest).is_err());
    assert_eq!(v.heap().stats().live_objects, 0);
    assert_eq!(v.stats().slots.active_ranges, v.pooled_root_ranges());
}
#[test]
fn load_rejects_unconnected_handlers_even_if_unreachable() {
    let mut p = fixtures::exception_regions();
    // MakeArray is now connected. Keep the unreachable-opcode rejection
    // regression against named spread, which remains outside this milestone.
    p.functions[0].code.extend([
        Instruction::new(Opcode::ArgsNamedSpread, 0, 0, 0),
        Instruction::new(Opcode::Return, 0, 0, 0),
    ]);
    let program = verify(p, Limits::default()).unwrap();
    let err = Vm::load(
        program,
        RuntimeSupport::default(),
        VmLimits::default(),
        SessionEpoch(1),
    )
    .err()
    .unwrap();
    assert!(matches!(
        err,
        VmError::Format(Error {
            kind: ErrorKind::Capability,
            ..
        })
    ));
}

#[test]
fn all_final_b01_cell_cases_execute_their_distinct_outcomes() {
    for case in fixtures::CELL_CASES {
        let mut v = vm((case.package)());
        v.start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
            .unwrap();
        let first = loop {
            let out = slice(&mut v);
            gc(&mut v);
            match out {
                RunOutcome::BudgetYield(_) => {}
                RunOutcome::Returned(id) | RunOutcome::Fault(id) => break id,
                other => panic!("{}: {other:?}", case.name),
            }
        };
        let id = if case.invoke_returned_closure {
            let closure = v.delivery_value(first).unwrap();
            let root = v
                .heap_mut()
                .add_root(OWNER, RootKind::Host, Value::NULL)
                .unwrap();
            v.take_delivery_root(first, root).unwrap();
            v.start_closure(closure, &[], OWNER, Context::Ordinary)
                .unwrap();
            loop {
                let out = slice(&mut v);
                gc(&mut v);
                match out {
                    RunOutcome::BudgetYield(_) => {}
                    RunOutcome::Returned(id) | RunOutcome::Fault(id) => break id,
                    other => panic!("{}: {other:?}", case.name),
                }
            }
        } else {
            first
        };
        match case.expected {
            fixtures::CellOutcome::IntOne => assert_eq!(
                v.delivery_value(id).unwrap().as_int(),
                Some(1),
                "{}",
                case.name
            ),
            fixtures::CellOutcome::Null => assert_eq!(
                v.delivery_value(id).unwrap().tag(),
                ValueTag::Null,
                "{}",
                case.name
            ),
            fixtures::CellOutcome::UninitializedReadFault => assert_eq!(
                v.delivery_fault(id).unwrap().unwrap().kind,
                FaultKind::Uninitialized,
                "{}",
                case.name
            ),
            fixtures::CellOutcome::SecondInitializationFault => assert_eq!(
                v.delivery_fault(id).unwrap().unwrap().kind,
                FaultKind::AlreadyInitialized,
                "{}",
                case.name
            ),
            fixtures::CellOutcome::ReadonlyWriteFault => assert_eq!(
                v.delivery_fault(id).unwrap().unwrap().kind,
                FaultKind::ReadOnly,
                "{}",
                case.name
            ),
        }
    }
}
fn with_callback(mut p: BytecodePackage, throw: bool) -> BytecodePackage {
    let mut callback = fixtures::exception_regions().functions.remove(0);
    callback.regions.clear();
    callback.source_map.clear();
    callback.code = vec![
        Instruction::new(Opcode::LoadConst, 0, 1, 0),
        Instruction::new(if throw { Opcode::Throw } else { Opcode::Return }, 0, 0, 0),
    ];
    callback.signature = SignatureId(1);
    let callback_id = FunctionId(p.functions.len() as u32 + 1);
    let mut factory = callback.clone();
    factory.code = vec![
        Instruction::new(Opcode::MakeClosure, 0, 0, 0),
        Instruction::new(Opcode::Return, 0, 0, 0),
    ];
    factory.descriptors = vec![Descriptor::Closure {
        function: callback_id,
        captures: vec![],
    }];
    factory.signature = SignatureId(p.signatures.len() as u32);
    p.signatures.push(Signature {
        parameters: vec![],
        rest: None,
        return_type: TypeId(0),
        effects: Effects::ALL,
        property_bag_tail: None,
    });
    p.functions.push(factory);
    p.functions.push(callback);
    p
}
#[test]
fn callscript_and_continue_multi_round_callback_fault_and_rooted_state() {
    for callback_fault in [false, true] {
        let mut v = vm(with_callback(fixtures::helper_native(), callback_fault));
        v.start(FunctionId(2), &[], OWNER, Context::Ordinary)
            .unwrap();
        let id = finish(&mut v);
        let closure = v.delivery_value(id).unwrap();
        let root = v
            .heap_mut()
            .add_root(OWNER, RootKind::Host, Value::NULL)
            .unwrap();
        v.take_delivery_root(id, root).unwrap();
        v.start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
            .unwrap();
        let state = v.heap_mut().alloc_array(&[Value::int(9)]).unwrap();
        let state_root = v.heap_mut().add_root(OWNER, RootKind::Host, state).unwrap();
        let mut rounds = 0;
        let mut old = None;
        loop {
            let out = slice(&mut v);
            gc(&mut v);
            match out {
                RunOutcome::BudgetYield(_) => {}
                RunOutcome::HostCall(t) => {
                    if let Some(previous) = old {
                        assert!(matches!(
                            v.respond(previous, HostResponse::Ready(Value::int(42))),
                            Err(VmError::InvalidTicket)
                        ));
                    }
                    old = Some(t);
                    match t.input {
                        NativeInput::Entry => {
                            rounds += 1;
                            v.respond(
                                t,
                                HostResponse::Continue(Continuation {
                                    faults: FaultRetention::Preserve,
                                    version: 7,
                                    stage: 1,
                                    values: &[state],
                                }),
                            )
                            .unwrap();
                            v.heap_mut().release_root(state_root).unwrap();
                        }
                        NativeInput::Continue {
                            version: 7,
                            stage: 1,
                        } => {
                            assert_eq!(
                                v.heap()
                                    .get_field(v.host_continuation(t).unwrap()[0], 0)
                                    .unwrap()
                                    .as_int(),
                                Some(9)
                            );
                            rounds += 1;
                            v.respond(
                                t,
                                HostResponse::CallScript {
                                    closure,
                                    arguments: &[],
                                    continuation: Continuation {
                                        faults: FaultRetention::Preserve,
                                        version: 7,
                                        stage: 2,
                                        values: &[state],
                                    },
                                },
                            )
                            .unwrap();
                        }
                        NativeInput::CallbackReturned | NativeInput::CallbackFault => {
                            assert_eq!(
                                t.input,
                                if callback_fault {
                                    NativeInput::CallbackFault
                                } else {
                                    NativeInput::CallbackReturned
                                }
                            );
                            assert_eq!(v.host_callback_value(t).unwrap().as_int(), Some(41));
                            rounds += 1;
                            if rounds < 5 {
                                v.respond(
                                    t,
                                    HostResponse::CallScript {
                                        closure,
                                        arguments: &[],
                                        continuation: Continuation {
                                            faults: FaultRetention::Preserve,
                                            version: 7,
                                            stage: rounds,
                                            values: &[state],
                                        },
                                    },
                                )
                                .unwrap();
                            } else {
                                v.respond(t, HostResponse::Ready(Value::int(42))).unwrap();
                            }
                        }
                        other => panic!("{other:?}"),
                    }
                    for _ in 0..2 {
                        collect(&mut v);
                    }
                }
                RunOutcome::Returned(id) => {
                    assert_eq!(v.delivery_value(id).unwrap().as_int(), Some(42));
                    break;
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(rounds, 5);
        assert_eq!(v.stats().native_entries, 1);
        assert_eq!(v.stats().native_resumes, 4);
        collect(&mut v);
        assert!(v.heap().info(state).is_err());
    }
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
fn loop_program(iterations: i64) -> BytecodePackage {
    let mut p = fixtures::exception_regions();
    let f = &mut p.functions[0];
    f.regions.clear();
    f.source_map.clear();
    f.register_count = 4;
    p.constants = vec![Const::Int(0), Const::Int(1), Const::Int(iterations)];
    f.code = vec![
        Instruction::new(Opcode::LoadConst, 0, 0, 0),
        Instruction::new(Opcode::LoadConst, 1, 1, 0),
        Instruction::new(Opcode::LoadConst, 2, 2, 0),
        Instruction::new(Opcode::Add, 0, 0, 1),
        Instruction::new(Opcode::Lt, 3, 0, 2),
        Instruction::new(Opcode::BranchBool, 3, 3, 6),
        Instruction::new(Opcode::Return, 0, 0, 0),
    ];
    p
}
#[test]
fn numeric_hot_loop_has_zero_allocator_calls_and_reports_real_work() {
    let mut v = vm(loop_program(100_000));
    v.start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let mut instructions = 0;
    let mut work = 0;
    let mut delivery = None;
    let allocation = allocation_counter::measure(|| {
        loop {
            let out = v
                .run_slice(
                    v.task_id().unwrap(),
                    SliceBudget {
                        instructions: 4096,
                        ..SliceBudget::default()
                    },
                    || 0,
                )
                .unwrap();
            let stats = v.stats();
            instructions += stats.usage.instructions;
            work += stats.usage.work;
            match out {
                RunOutcome::BudgetYield(_) => {}
                RunOutcome::Returned(id) => {
                    delivery = Some(id);
                    break;
                }
                o => panic!("{o:?}"),
            }
        }
    });
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
    assert_eq!(instructions, 300004);
    assert!(work >= instructions as u64);
    assert_eq!(
        v.delivery_value(delivery.unwrap()).unwrap().as_int(),
        Some(100_000)
    );
}
#[test]
fn repeated_native_calls_resume_without_frame_pool_allocation_or_range_leak() {
    let mut v = vm(fixtures::helper_native());
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
fn slice_deadline_allocation_and_work_limits_preserve_unexecuted_operation() {
    let mut v = vm(fixtures::cell_closures());
    v.start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let initial = v.stats();
    assert!(matches!(
        v.run_slice(
            v.task_id().unwrap(),
            SliceBudget {
                deadline_ns: Some(10),
                ..SliceBudget::default()
            },
            || 10
        )
        .unwrap(),
        RunOutcome::BudgetYield(_)
    ));
    assert_eq!(initial.frames, v.stats().frames);
    loop {
        let out = v
            .run_slice(
                v.task_id().unwrap(),
                SliceBudget {
                    instructions: 1,
                    allocation_bytes: 0,
                    ..SliceBudget::default()
                },
                || 0,
            )
            .unwrap();
        match out {
            RunOutcome::BudgetYield(u) if u.required_allocation > 0 => {
                assert_eq!(u.allocation_bytes, 0);
                break;
            }
            RunOutcome::BudgetYield(_) => {}
            o => panic!("{o:?}"),
        }
    }
    let id = finish(&mut v);
    assert_eq!(
        v.heap().info(v.delivery_value(id).unwrap()).unwrap().kind,
        ObjectKind::Closure
    );
}
fn finally_program(nested_in_cleanup: bool) -> BytecodePackage {
    let mut p = fixtures::exception_regions();
    p.constants.push(Const::Int(0));
    p.modules[0].globals.push(GlobalDef {
        name: SymbolId(9),
        ty: TypeId(1),
        mutable: true,
        linked_function: None,
    });
    let f = &mut p.functions[0];
    f.register_count = 3;
    f.source_map.clear();
    f.descriptors = vec![Descriptor::Global {
        module: ModuleId(0),
        global: GlobalId(0),
    }];
    let i = Instruction::new;
    f.code = vec![
        i(Opcode::LoadConst, 0, 3, 0),
        i(Opcode::GlobalInit, 0, 0, 0),
        i(Opcode::LoadConst, 1, 2, 0),
        i(Opcode::LoadConst, 2, 1, 0),
        i(Opcode::Throw, 2, 0, 0),
        i(Opcode::GlobalRead, 0, 0, 0),
        i(Opcode::Add, 0, 0, 1),
        i(Opcode::GlobalWrite, 0, 0, 0),
        i(Opcode::EndFinally, 0, 0, 0),
        i(Opcode::GlobalRead, 0, 0, 0),
        i(Opcode::Add, 0, 0, 1),
        i(Opcode::GlobalWrite, 0, 0, 0),
        i(Opcode::EndFinally, 1, 0, 0),
        i(Opcode::Return, 0, 0, 0),
    ];
    f.regions = vec![
        ExceptionRegion {
            protected: PcRange {
                start: Pc(3),
                end: Pc(5),
            },
            catch: None,
            finally: Some(PcRange {
                start: Pc(5),
                end: Pc(9),
            }),
        },
        ExceptionRegion {
            protected: PcRange {
                start: Pc(0),
                end: Pc(9),
            },
            catch: None,
            finally: Some(PcRange {
                start: Pc(9),
                end: Pc(13),
            }),
        },
    ];
    if nested_in_cleanup {
        // Leave from a try nested INSIDE outer finally must preserve the pending throw.
        f.code = vec![
            i(Opcode::LoadConst, 0, 3, 0),
            i(Opcode::GlobalInit, 0, 0, 0),
            i(Opcode::LoadConst, 1, 2, 0),
            i(Opcode::LoadConst, 2, 1, 0),
            i(Opcode::Throw, 2, 0, 0),
            i(Opcode::Leave, 10, 0, 0),
            i(Opcode::GlobalRead, 0, 0, 0),
            i(Opcode::Add, 0, 0, 1),
            i(Opcode::GlobalWrite, 0, 0, 0),
            i(Opcode::EndFinally, 0, 0, 0),
            i(Opcode::GlobalRead, 0, 0, 0),
            i(Opcode::Add, 0, 0, 1),
            i(Opcode::GlobalWrite, 0, 0, 0),
            i(Opcode::EndFinally, 1, 0, 0),
            i(Opcode::Return, 0, 0, 0),
        ];
        f.regions = vec![
            ExceptionRegion {
                protected: PcRange {
                    start: Pc(5),
                    end: Pc(6),
                },
                catch: None,
                finally: Some(PcRange {
                    start: Pc(6),
                    end: Pc(10),
                }),
            },
            ExceptionRegion {
                protected: PcRange {
                    start: Pc(0),
                    end: Pc(5),
                },
                catch: None,
                finally: Some(PcRange {
                    start: Pc(5),
                    end: Pc(14),
                }),
            },
        ];
    }
    let mut observer = f.clone();
    observer.regions.clear();
    observer.code = vec![i(Opcode::GlobalRead, 0, 0, 0), i(Opcode::Return, 0, 0, 0)];
    p.functions.push(observer);
    p
}
#[test]
fn two_finally_layers_once_and_nested_normal_try_inside_finally() {
    for nested in [false, true] {
        let mut v = vm(finally_program(nested));
        v.start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
            .unwrap();
        let id = loop {
            let out = slice(&mut v);
            gc(&mut v);
            match out {
                RunOutcome::BudgetYield(_) => {}
                RunOutcome::Fault(id) => break id,
                o => panic!("{o:?}"),
            }
        };
        assert_eq!(v.delivery_value(id).unwrap().as_int(), Some(41));
        assert_eq!(v.stats().unwinds, 0);
        v.discard_delivery(id).unwrap();
        v.start(FunctionId(1), &[], OWNER, Context::Ordinary)
            .unwrap();
        let id = finish(&mut v);
        assert_eq!(v.delivery_value(id).unwrap().as_int(), Some(2));
    }
}
fn binary_program(op: Opcode) -> BytecodePackage {
    let mut p = fixtures::exception_regions();
    p.signatures[0].parameters = vec![
        Parameter {
            name: SymbolId(4),
            ty: TypeId(0),
            mutable: false,
            default: None,
        },
        Parameter {
            name: SymbolId(9),
            ty: TypeId(0),
            mutable: false,
            default: None,
        },
    ];
    let f = &mut p.functions[0];
    f.regions.clear();
    f.source_map.clear();
    f.register_count = 3;
    f.code = vec![
        Instruction::new(op, 2, 0, 1),
        Instruction::new(Opcode::Return, 2, 0, 0),
    ];
    p
}
#[test]
fn exact_mixed_numbers_nan_strict_bool_and_checked_overflow() {
    for (op, a, b, expected) in [
        (
            Opcode::Eq,
            Value::int(9007199254740993),
            Value::float(9007199254740992.0),
            Some(Value::bool(false)),
        ),
        (
            Opcode::Gt,
            Value::int(i64::MAX),
            Value::float(9223372036854775808.0),
            Some(Value::bool(false)),
        ),
        (
            Opcode::Eq,
            Value::int(0),
            Value::float(-0.0),
            Some(Value::bool(true)),
        ),
        (
            Opcode::Eq,
            Value::float(f64::NAN),
            Value::float(f64::NAN),
            Some(Value::bool(false)),
        ),
        (
            Opcode::Eq,
            Value::bool(true),
            Value::int(1),
            Some(Value::bool(false)),
        ),
        (Opcode::Add, Value::int(i64::MAX), Value::int(1), None),
        (Opcode::Div, Value::int(i64::MIN), Value::int(-1), None),
    ] {
        let mut v = vm(binary_program(op));
        v.start(fixtures::ENTRY, &[a, b], OWNER, Context::Ordinary)
            .unwrap();
        loop {
            match slice(&mut v) {
                RunOutcome::BudgetYield(_) => {}
                RunOutcome::Returned(id) => {
                    assert!(v.delivery_value(id).unwrap().same_bits(expected.unwrap()));
                    break;
                }
                RunOutcome::Fault(id) => {
                    assert!(expected.is_none());
                    assert_eq!(
                        v.delivery_fault(id).unwrap().unwrap().kind,
                        FaultKind::Overflow
                    );
                    break;
                }
                o => panic!("{o:?}"),
            }
        }
    }
    let mut p = binary_program(Opcode::Eq);
    p.functions[0].code = vec![
        Instruction::new(Opcode::BranchBool, 0, 1, 1),
        Instruction::new(Opcode::Return, 0, 0, 0),
    ];
    let mut v = vm(p);
    v.start(
        fixtures::ENTRY,
        &[Value::int(1), Value::NULL],
        OWNER,
        Context::Ordinary,
    )
    .unwrap();
    loop {
        match slice(&mut v) {
            RunOutcome::BudgetYield(_) => {}
            RunOutcome::Fault(id) => {
                assert_eq!(v.delivery_fault(id).unwrap().unwrap().kind, FaultKind::Type);
                break;
            }
            o => panic!("{o:?}"),
        }
    }
}
#[test]
fn text_bytes_and_nested_tuple_equality_use_rooted_resumable_stages() {
    for op in [Opcode::Eq, Opcode::Ne] {
        let mut v = vm(binary_program(op));
        let left = v.heap_mut().alloc_string("中文🙂").unwrap();
        let right = v.heap_mut().alloc_string("中文🙂").unwrap();
        let bytes_left = v.heap_mut().alloc_bytes(&[1, 2, 3]).unwrap();
        let bytes_right = v.heap_mut().alloc_bytes(&[1, 2, 3]).unwrap();
        let a = v
            .heap_mut()
            .alloc_tuple(&[left, bytes_left, Value::int(9007199254740992)])
            .unwrap();
        let b = v
            .heap_mut()
            .alloc_tuple(&[right, bytes_right, Value::float(9007199254740992.0)])
            .unwrap();
        let a = v.heap_mut().alloc_tuple(&[a]).unwrap();
        let b = v.heap_mut().alloc_tuple(&[b]).unwrap();
        v.start(fixtures::ENTRY, &[a, b], OWNER, Context::Ordinary)
            .unwrap();
        let mut work_limit = 1;
        let mut small_steps = 0;
        loop {
            let out = v
                .run_slice(
                    v.task_id().unwrap(),
                    SliceBudget {
                        instructions: 1,
                        work: work_limit,
                        ..SliceBudget::default()
                    },
                    || 0,
                )
                .unwrap();
            gc(&mut v);
            work_limit = 1;
            match out {
                RunOutcome::BudgetYield(u) => {
                    if u.work == 1 {
                        small_steps += 1;
                    }
                    if u.work == 0 {
                        work_limit = u.required_work.max(1);
                    }
                }
                RunOutcome::Returned(id) => {
                    assert_eq!(
                        v.delivery_value(id).unwrap().as_bool(),
                        Some(op == Opcode::Eq)
                    );
                    break;
                }
                o => panic!("{o:?}"),
            }
        }
        assert!(small_steps > 10);
        assert_eq!(v.stats().slots.active_ranges, v.pooled_root_ranges());
    }
    let mut v = vm(binary_program(Opcode::Add));
    let a = v.heap_mut().alloc_string("中文").unwrap();
    let b = v.heap_mut().alloc_string("🙂").unwrap();
    v.start(fixtures::ENTRY, &[a, b], OWNER, Context::Ordinary)
        .unwrap();
    let id = finish(&mut v);
    let result = v.delivery_value(id).unwrap();
    let mut output = [0; 10];
    assert_eq!(v.heap().read_bytes(result, 0, &mut output).unwrap(), 10);
    assert_eq!(std::str::from_utf8(&output).unwrap(), "中文🙂");
}
#[test]
fn callback_pending_suspends_same_stack_and_inherits_nowait_dynamically() {
    for restricted in [false, true] {
        let mut p = with_callback(fixtures::helper_native(), false);
        let callback = &mut p.functions[3];
        callback.register_count = 2;
        callback.descriptors = vec![Descriptor::Call(CallDesc {
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
                            assert_eq!(v.host_callback_value(t).unwrap().as_int(), Some(42));
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
fn defaults_read_definition_cell_after_defining_frame_is_gone() {
    let mut v = vm(fixtures::captured_default());
    v.start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let id = finish(&mut v);
    let closure = v.delivery_value(id).unwrap();
    let root = v
        .heap_mut()
        .add_root(OWNER, RootKind::Host, Value::NULL)
        .unwrap();
    v.take_delivery_root(id, root).unwrap();
    let cell = v.heap().get_field(closure, 3).unwrap();
    v.heap_mut().cell_write(cell, Value::int(99)).unwrap();
    collect(&mut v);
    v.start_closure(closure, &[], OWNER, Context::Ordinary)
        .unwrap();
    let id = finish(&mut v);
    assert_eq!(v.delivery_value(id).unwrap().as_int(), Some(99));
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
                let state = v.heap_mut().alloc_array(&[Value::int(4)]).unwrap();
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

fn task_slice(v: &mut Vm, task: TaskId) -> RunOutcome {
    let out = v
        .run_slice(
            task,
            SliceBudget {
                instructions: 1,
                ..SliceBudget::default()
            },
            || 0,
        )
        .unwrap();
    gc(v);
    out
}
fn task_finish(v: &mut Vm, task: TaskId) -> DeliveryId {
    for _ in 0..10000 {
        match task_slice(v, task) {
            RunOutcome::BudgetYield(_) => {}
            RunOutcome::Returned(id) | RunOutcome::Fault(id) => return id,
            other => panic!("{other:?}"),
        }
    }
    panic!("task did not finish")
}
fn task_ticket(v: &mut Vm, task: TaskId) -> HostTicket {
    for _ in 0..10000 {
        match task_slice(v, task) {
            RunOutcome::BudgetYield(_) => {}
            RunOutcome::HostCall(t) => return t,
            other => panic!("{other:?}"),
        }
    }
    panic!("no ticket")
}
#[test]
fn tasks_share_cells_globals_gc_and_have_independent_tickets_deliveries_and_cancel() {
    let mut p = fixtures::helper_native();
    let i = Instruction::new;
    p.modules[0].globals.push(GlobalDef {
        name: SymbolId(9),
        ty: TypeId(0),
        mutable: true,
        linked_function: None,
    });
    p.signatures.push(Signature {
        parameters: vec![Parameter {
            name: SymbolId(4),
            ty: TypeId(0),
            mutable: false,
            default: None,
        }],
        rest: None,
        return_type: TypeId(0),
        effects: Effects::ALL,
        property_bag_tail: None,
    });
    let mut a = p.functions[0].clone();
    a.signature = SignatureId(2);
    a.source_map.clear();
    a.register_count = 4;
    a.descriptors = vec![
        Descriptor::Call(CallDesc {
            target: CallTarget::Native(NativeId(0)),
            arguments: Arguments::Window(RegWindow {
                start: Reg(1),
                len: 1,
            }),
            names: vec![],
        }),
        Descriptor::Global {
            module: ModuleId(0),
            global: GlobalId(0),
        },
    ];
    a.code = vec![
        i(Opcode::GlobalInit, 0, 1, 0),
        i(Opcode::LoadConst, 1, 1, 0),
        i(Opcode::Call, 2, 0, 0),
        i(Opcode::GlobalRead, 3, 1, 0),
        i(Opcode::CellRead, 2, 3, 0),
        i(Opcode::Return, 2, 0, 0),
    ];
    let mut b = a.clone();
    b.code = vec![
        i(Opcode::LoadConst, 1, 2, 0),
        i(Opcode::CellWrite, 0, 1, 0),
        i(Opcode::GlobalRead, 3, 1, 0),
        i(Opcode::CellRead, 2, 3, 0),
        i(Opcode::Return, 2, 0, 0),
    ];
    p.functions[0] = a;
    p.functions.push(b);
    let mut v = vm(p);
    let cell = v.heap_mut().alloc_cell(Value::int(41)).unwrap();
    let a = v
        .start(FunctionId(0), &[cell], OWNER, Context::Ordinary)
        .unwrap();
    let ta = task_ticket(&mut v, a);
    assert_eq!(ta.task, a);
    v.respond(ta, HostResponse::Pending(ta.wait_id())).unwrap();
    let b = v
        .start(FunctionId(2), &[cell], RootOwner(18), Context::Ordinary)
        .unwrap();
    let db = task_finish(&mut v, b);
    assert_eq!(v.delivery_value(db).unwrap().as_int(), Some(1));
    assert_eq!(v.heap().cell_read(cell).unwrap().as_int(), Some(1));
    collect(&mut v);
    assert_eq!(task_slice(&mut v, a), RunOutcome::Waiting(ta.wait_id()));
    let c = v
        .start(
            FunctionId(1),
            &[Value::int(41)],
            RootOwner(18),
            Context::Ordinary,
        )
        .unwrap();
    let tc = task_ticket(&mut v, c);
    v.cancel_task(c).unwrap();
    assert!(matches!(
        v.respond(tc, HostResponse::Ready(Value::int(42))),
        Err(VmError::InvalidTicket)
    ));
    while !matches!(
        v.run_slice(c, SliceBudget::default(), || 0),
        Err(VmError::Idle)
    ) {}
    assert!(matches!(
        v.run_slice(c, SliceBudget::default(), || 0),
        Err(VmError::UnknownTask)
    ));
    v.complete(ta.wait_id(), Completion::Ready(Value::int(42)))
        .unwrap();
    let da = task_finish(&mut v, a);
    collect(&mut v);
    assert_eq!(v.delivery_value(da).unwrap().as_int(), Some(1));
    assert_eq!(v.delivery_value(db).unwrap().as_int(), Some(1));
    v.discard_delivery(db).unwrap();
    assert!(matches!(
        v.delivery_value(db),
        Err(VmError::InvalidDelivery)
    ));
    v.discard_delivery(da).unwrap();
    assert!(v.is_idle());
    assert_eq!(v.stats().slots.active_ranges, v.pooled_root_ranges());
    let d = v
        .start(FunctionId(1), &[Value::int(41)], OWNER, Context::Ordinary)
        .unwrap();
    assert_ne!(d, c);
    assert!(matches!(
        v.complete(tc.wait_id(), Completion::Ready(Value::int(42))),
        Err(VmError::InvalidWait)
    ));
    let td = task_ticket(&mut v, d);
    v.respond(td, HostResponse::Ready(Value::int(42))).unwrap();
    task_finish(&mut v, d);
}
#[test]
fn cell_read_rejects_noncell_containers() {
    let mut h = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
    for kind in [ObjectKind::Closure, ObjectKind::Instance] {
        let v = h.alloc_record(kind, &[Value::int(1)]).unwrap();
        assert!(matches!(h.cell_read(v), Err(HeapError::WrongKind)));
    }
    let a = h.alloc_array(&[Value::int(1)]).unwrap();
    assert!(matches!(h.cell_read(a), Err(HeapError::WrongKind)));
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
    assert_eq!(fault.location.pc, Pc(2));
    assert_eq!(v.stats().native_entries, 0);
}
#[test]
fn dynamic_call_type_fault_is_at_call_and_caught_there() {
    let mut p = fixtures::exception_regions();
    let f = &mut p.functions[0];
    f.source_map.clear();
    f.regions.clear();
    f.descriptors = vec![Descriptor::Call(CallDesc {
        target: CallTarget::Register(Reg(0)),
        arguments: Arguments::Window(RegWindow {
            start: Reg(0),
            len: 0,
        }),
        names: vec![],
    })];
    let i = Instruction::new;
    f.code = vec![
        i(Opcode::LoadConst, 0, 1, 0),
        i(Opcode::Call, 1, 0, 0),
        i(Opcode::Leave, 5, 0, 0),
        i(Opcode::LoadConst, 0, 2, 0),
        i(Opcode::Return, 0, 0, 0),
        i(Opcode::Return, 0, 0, 0),
    ];
    f.regions = vec![ExceptionRegion {
        protected: PcRange {
            start: Pc(1),
            end: Pc(3),
        },
        catch: Some(CatchRegion {
            range: PcRange {
                start: Pc(3),
                end: Pc(5),
            },
            exception: Reg(1),
        }),
        finally: None,
    }];
    let mut v = vm(p);
    let t = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let d = task_finish(&mut v, t);
    assert_eq!(v.delivery_value(d).unwrap().as_int(), Some(1));
    assert_eq!(v.delivery_fault(d).unwrap(), None);
}

fn inner_fault_in_finally(pending_throw: bool, fault: u8) -> BytecodePackage {
    let mut p = with_callback(fixtures::helper_native(), true);
    let i = Instruction::new;
    let f = &mut p.functions[0];
    f.source_map.clear();
    f.register_count = 3;
    f.descriptors = vec![Descriptor::Call(CallDesc {
        target: CallTarget::Native(NativeId(0)),
        arguments: Arguments::Window(RegWindow {
            start: Reg(1),
            len: 1,
        }),
        names: vec![],
    })];
    f.code = vec![
        i(Opcode::LoadConst, 0, 1, 0),
        i(
            if pending_throw {
                Opcode::Throw
            } else {
                Opcode::Return
            },
            0,
            0,
            0,
        ),
        i(Opcode::LoadConst, 1, 2, 0),
        i(Opcode::LoadConst, 2, 0, 0),
        match fault {
            0 => i(Opcode::Throw, 1, 0, 0),
            1 => i(Opcode::Add, 2, 1, 2),
            _ => i(Opcode::Call, 2, 0, 0),
        },
        i(Opcode::Leave, 8, 0, 0),
        i(Opcode::LoadConst, 1, 2, 0),
        i(Opcode::Leave, 8, 0, 0),
        i(Opcode::EndFinally, 0, 0, 0),
        i(Opcode::Return, 0, 0, 0),
    ];
    f.regions = vec![
        ExceptionRegion {
            protected: PcRange {
                start: Pc(0),
                end: Pc(2),
            },
            catch: None,
            finally: Some(PcRange {
                start: Pc(2),
                end: Pc(9),
            }),
        },
        ExceptionRegion {
            protected: PcRange {
                start: Pc(2),
                end: Pc(6),
            },
            catch: Some(CatchRegion {
                range: PcRange {
                    start: Pc(6),
                    end: Pc(8),
                },
                exception: Reg(2),
            }),
            finally: None,
        },
    ];
    p
}
#[test]
fn caught_errors_inside_finally_preserve_pending_return_and_throw() {
    for pending_throw in [false, true] {
        for fault in 0..4 {
            let mut v = vm(inner_fault_in_finally(pending_throw, fault));
            let factory = v
                .start(FunctionId(2), &[], OWNER, Context::Ordinary)
                .unwrap();
            let d = task_finish(&mut v, factory);
            let closure = v.delivery_value(d).unwrap();
            let root = v
                .heap_mut()
                .add_root(OWNER, RootKind::Host, closure)
                .unwrap();
            v.discard_delivery(d).unwrap();
            let task = v
                .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
                .unwrap();
            let mut entries = 0;
            let mut callbacks = 0;
            let delivery = loop {
                match task_slice(&mut v, task) {
                    RunOutcome::BudgetYield(_) => {}
                    RunOutcome::HostCall(t) => match t.input {
                        NativeInput::Entry => {
                            entries += 1;
                            if fault == 3 {
                                v.respond(
                                    t,
                                    HostResponse::CallScript {
                                        closure,
                                        arguments: &[],
                                        continuation: Continuation {
                                            faults: FaultRetention::Preserve,
                                            version: 17,
                                            stage: 73,
                                            values: &[],
                                        },
                                    },
                                )
                                .unwrap();
                            } else {
                                v.respond(t, HostResponse::Error(NewFault::thrown(Value::int(1))))
                                    .unwrap();
                            }
                        }
                        NativeInput::CallbackFault => {
                            callbacks += 1;
                            assert_eq!(
                                t.continuation,
                                Some(ContinuationState {
                                    version: 17,
                                    stage: 73
                                })
                            );
                            let info = v.host_callback_fault(t).unwrap().unwrap();
                            assert_eq!(info.location.code.function, FunctionId(3));
                            let _value = v.host_callback_value(t).unwrap();
                            v.respond(
                                t,
                                HostResponse::ForwardFault(
                                    v.host_callback_fault(t).unwrap().unwrap().id,
                                ),
                            )
                            .unwrap();
                        }
                        o => panic!("{o:?}"),
                    },
                    RunOutcome::Returned(d) if !pending_throw => break d,
                    RunOutcome::Fault(d) if pending_throw => break d,
                    o => panic!("pending={pending_throw}, fault={fault}: {o:?}"),
                }
            };
            assert_eq!(v.delivery_value(delivery).unwrap().as_int(), Some(41));
            assert_eq!(v.stats().unwinds, 0);
            if pending_throw {
                let info = v.delivery_fault(delivery).unwrap().unwrap();
                assert_eq!(info.kind, FaultKind::Thrown);
                assert_eq!(info.location.pc, Pc(1));
                assert!(
                    v.heap()
                        .fault_causes(info.id)
                        .unwrap()
                        .next(v.heap())
                        .unwrap()
                        .is_none()
                );
            } else {
                assert_eq!(v.delivery_fault(delivery).unwrap(), None);
            }
            assert_eq!(entries, if fault >= 2 { 1 } else { 0 });
            assert_eq!(callbacks, if fault == 3 { 1 } else { 0 });
            v.heap_mut().release_root(root).unwrap();
        }
    }
}
#[test]
fn escaping_finally_error_preserves_pending_value_as_rooted_cause() {
    let mut p = inner_fault_in_finally(true, 2);
    p.functions[0].regions.truncate(1);
    for pc in [5, 7] {
        p.functions[0].code[pc] = Instruction::new(Opcode::Jump, 8, 0, 0);
    }
    let mut v = vm(p);
    let task = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let t = task_ticket(&mut v, task);
    let exception = v.heap_mut().alloc_string("cleanup failed").unwrap();
    v.respond(t, HostResponse::Error(NewFault::thrown(exception)))
        .unwrap();
    let d = task_finish(&mut v, task);
    collect(&mut v);
    let f = v.delivery_fault(d).unwrap().unwrap();
    assert_eq!(f.location.pc, Pc(4));
    assert!(
        v.heap()
            .fault_causes(f.id)
            .unwrap()
            .next(v.heap())
            .unwrap()
            .is_some()
    );
    assert_eq!(v.delivery_cause(d).unwrap().unwrap().as_int(), Some(41));
    assert!(v.delivery_value(d).unwrap().same_bits(exception));
}
fn preparation_package(missing: bool, caller_catch: bool) -> BytecodePackage {
    let mut p = fixtures::helper_native();
    let i = Instruction::new;
    let f = &mut p.functions[1];
    f.source_map.clear();
    f.register_count = 2;
    f.code = vec![
        i(Opcode::LoadConst, 1, 1, 0),
        i(Opcode::Return, 1, 0, 0),
        i(Opcode::LoadConst, 1, 2, 0),
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
            exception: Reg(1),
        }),
        finally: None,
    }];
    let main = &mut p.functions[0];
    main.source_map.clear();
    main.code[0] = i(
        if missing {
            Opcode::ArgsNew
        } else {
            Opcode::Clear
        },
        0,
        0,
        0,
    );
    if missing {
        let Descriptor::Call(d) = &mut main.descriptors[0] else {
            panic!()
        };
        d.arguments = Arguments::Packed(Reg(0));
    }
    if caller_catch {
        main.code = vec![
            main.code[0],
            main.code[1],
            i(Opcode::Leave, 5, 0, 0),
            i(Opcode::LoadConst, 0, 2, 0),
            i(Opcode::Return, 0, 0, 0),
            i(Opcode::Return, 1, 0, 0),
        ];
        main.regions = vec![ExceptionRegion {
            protected: PcRange {
                start: Pc(1),
                end: Pc(3),
            },
            catch: Some(CatchRegion {
                range: PcRange {
                    start: Pc(3),
                    end: Pc(5),
                },
                exception: Reg(1),
            }),
            finally: None,
        }];
    }
    p
}
#[test]
fn preparation_faults_route_to_caller_without_entering_callee_handlers_or_replaying() {
    for missing in [false, true] {
        for caller_catch in [false, true] {
            let mut v = vm(preparation_package(missing, caller_catch));
            let t = v
                .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
                .unwrap();
            let d = task_finish(&mut v, t);
            if caller_catch {
                assert_eq!(v.delivery_fault(d).unwrap(), None);
                assert_eq!(v.delivery_value(d).unwrap().as_int(), Some(1));
            } else {
                let f = v.delivery_fault(d).unwrap().unwrap();
                assert_eq!(
                    f.kind,
                    if missing {
                        FaultKind::MissingArgument
                    } else {
                        FaultKind::Uninitialized
                    }
                );
                assert_eq!(f.location.code.function, FunctionId(0));
                assert_eq!(f.location.pc, Pc(1));
            }
            assert_eq!(v.stats().native_entries, 0);
            assert_eq!(v.stats().frames, 0);
        }
    }
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
                    assert_eq!(c.version, 19);
                    c.stage
                });
                if stage == 7 {
                    v.respond(t, HostResponse::Ready(Value::int(42))).unwrap();
                } else if stage % 2 == 0 {
                    v.respond(
                        t,
                        HostResponse::Continue(Continuation {
                            faults: FaultRetention::Preserve,
                            version: 19,
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
                                version: 19,
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
fn resource_failures_during_default_and_callback_preparation_are_consumed_faults() {
    let p = fixtures::captured_default();
    let registry = support(&p);
    let mut v = Vm::load(
        verify(p, Limits::default()).unwrap(),
        registry,
        VmLimits {
            max_frames: 1,
            ..VmLimits::default()
        },
        SessionEpoch(4),
    )
    .unwrap();
    let factory = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let d = task_finish(&mut v, factory);
    let closure = v.delivery_value(d).unwrap();
    let root = v
        .heap_mut()
        .add_root(OWNER, RootKind::Host, closure)
        .unwrap();
    v.discard_delivery(d).unwrap();
    let t = v
        .start_closure(closure, &[], OWNER, Context::Ordinary)
        .unwrap();
    let d = task_finish(&mut v, t);
    assert_eq!(
        v.delivery_fault(d).unwrap().unwrap().kind,
        FaultKind::Resource
    );
    assert_eq!(v.stats().frames, 0);
    v.heap_mut().release_root(root).unwrap();
    let p = with_callback(fixtures::helper_native(), false);
    let registry = support(&p);
    let mut v = Vm::load(
        verify(p, Limits::default()).unwrap(),
        registry,
        VmLimits {
            max_frames: 3,
            ..VmLimits::default()
        },
        SessionEpoch(4),
    )
    .unwrap();
    let f = v
        .start(FunctionId(2), &[], OWNER, Context::Ordinary)
        .unwrap();
    let d = task_finish(&mut v, f);
    let closure = v.delivery_value(d).unwrap();
    let root = v
        .heap_mut()
        .add_root(OWNER, RootKind::Host, closure)
        .unwrap();
    v.discard_delivery(d).unwrap();
    let task = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let ticket = task_ticket(&mut v, task);
    v.respond(
        ticket,
        HostResponse::CallScript {
            closure,
            arguments: &[],
            continuation: Continuation {
                faults: FaultRetention::Preserve,
                version: 1,
                stage: 99,
                values: &[],
            },
        },
    )
    .unwrap();
    let resume = task_ticket(&mut v, task);
    assert_eq!(resume.input, NativeInput::CallbackFault);
    assert_eq!(resume.continuation.unwrap().stage, 99);
    let fault = v.host_callback_fault(resume).unwrap().unwrap();
    assert_eq!(fault.kind, FaultKind::Resource);
    assert_eq!(fault.location, ticket.location);
    let _value = v.host_callback_value(resume).unwrap();
    v.respond(
        resume,
        HostResponse::ForwardFault(v.host_callback_fault(resume).unwrap().unwrap().id),
    )
    .unwrap();
    let d = task_finish(&mut v, task);
    assert_eq!(
        v.delivery_fault(d).unwrap().unwrap().kind,
        FaultKind::Resource
    );
    assert_eq!(v.stats().native_entries, 1);
    v.heap_mut().release_root(root).unwrap();
}
#[test]
fn rethrow_keeps_original_fault_source_through_nested_catches_and_gc() {
    let mut p = fixtures::exception_regions();
    let f = &mut p.functions[0];
    let i = Instruction::new;
    f.source_map.clear();
    f.register_count = 2;
    f.code = vec![
        i(Opcode::LoadConst, 0, 1, 0),
        i(Opcode::Throw, 0, 0, 0),
        i(Opcode::Rethrow, 0, 0, 0),
        i(Opcode::Rethrow, 0, 0, 0),
    ];
    f.regions = vec![
        ExceptionRegion {
            protected: PcRange {
                start: Pc(0),
                end: Pc(2),
            },
            catch: Some(CatchRegion {
                range: PcRange {
                    start: Pc(2),
                    end: Pc(3),
                },
                exception: Reg(1),
            }),
            finally: None,
        },
        ExceptionRegion {
            protected: PcRange {
                start: Pc(0),
                end: Pc(3),
            },
            catch: Some(CatchRegion {
                range: PcRange {
                    start: Pc(3),
                    end: Pc(4),
                },
                exception: Reg(1),
            }),
            finally: None,
        },
    ];
    let mut v = vm(p);
    let t = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let d = task_finish(&mut v, t);
    assert_eq!(v.delivery_value(d).unwrap().as_int(), Some(41));
    assert_eq!(v.delivery_fault(d).unwrap().unwrap().location.pc, Pc(1));
}

fn normal_cleanup_in_handler(in_finally: bool, leave: bool) -> BytecodePackage {
    let mut p = fixtures::exception_regions();
    p.constants.push(Const::Int(0));
    p.modules[0].globals.push(GlobalDef {
        name: SymbolId(9),
        ty: TypeId(1),
        mutable: true,
        linked_function: None,
    });
    let i = Instruction::new;
    let f = &mut p.functions[0];
    f.source_map.clear();
    f.register_count = 3;
    f.descriptors = vec![Descriptor::Global {
        module: ModuleId(0),
        global: GlobalId(0),
    }];
    f.code = vec![
        i(Opcode::LoadConst, 0, 3, 0),
        i(Opcode::GlobalInit, 0, 0, 0),
        i(Opcode::LoadConst, 1, 2, 0),
        i(
            if in_finally {
                Opcode::Return
            } else {
                Opcode::Throw
            },
            1,
            0,
            0,
        ),
        i(Opcode::LoadConst, 0, 1, 0),
        i(
            if leave { Opcode::Leave } else { Opcode::Return },
            if leave { 10 } else { 0 },
            0,
            0,
        ),
        i(Opcode::GlobalRead, 2, 0, 0),
        i(Opcode::Add, 2, 2, 1),
        i(Opcode::GlobalWrite, 2, 0, 0),
        i(Opcode::EndFinally, 0, 0, 0),
        i(
            if in_finally {
                Opcode::Jump
            } else {
                Opcode::Return
            },
            if in_finally { 11 } else { 0 },
            0,
            0,
        ),
        i(Opcode::GlobalRead, 2, 0, 0),
        i(Opcode::Add, 2, 2, 1),
        i(Opcode::GlobalWrite, 2, 0, 0),
        i(Opcode::EndFinally, 1, 0, 0),
        i(Opcode::GlobalRead, 2, 0, 0),
        i(Opcode::Return, 2, 0, 0),
    ];
    f.regions = vec![
        ExceptionRegion {
            protected: PcRange {
                start: Pc(4),
                end: Pc(6),
            },
            catch: None,
            finally: Some(PcRange {
                start: Pc(6),
                end: Pc(10),
            }),
        },
        ExceptionRegion {
            protected: PcRange {
                start: Pc(0),
                end: Pc(4),
            },
            catch: if in_finally {
                None
            } else {
                Some(CatchRegion {
                    range: PcRange {
                        start: Pc(4),
                        end: Pc(11),
                    },
                    exception: Reg(2),
                })
            },
            finally: Some(PcRange {
                start: Pc(if in_finally { 4 } else { 11 }),
                end: Pc(15),
            }),
        },
    ];
    let mut observer = f.clone();
    observer.regions.clear();
    observer.code = vec![i(Opcode::GlobalRead, 0, 0, 0), i(Opcode::Return, 0, 0, 0)];
    p.functions.push(observer);
    p
}
#[test]
fn shared_cleanup_selects_full_lexical_parents_for_return_and_leave_inside_catch_and_finally() {
    for in_finally in [false, true] {
        for leave in [false, true] {
            // The language/verifier forbids Return from finally; Leave still
            // exercises a cleanup nested in that lexical block.
            if in_finally && !leave {
                continue;
            }
            let mut v = vm(normal_cleanup_in_handler(in_finally, leave));
            let task = v
                .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
                .unwrap();
            let d = task_finish(&mut v, task);
            assert_eq!(v.delivery_fault(d).unwrap(), None);
            assert_eq!(
                v.delivery_value(d).unwrap().as_int(),
                Some(if in_finally && leave { 1 } else { 41 })
            );
            assert_eq!(v.stats().unwinds, 0);
            v.discard_delivery(d).unwrap();
            let task = v
                .start(FunctionId(1), &[], OWNER, Context::Ordinary)
                .unwrap();
            let d = task_finish(&mut v, task);
            assert_eq!(
                v.delivery_value(d).unwrap().as_int(),
                Some(if in_finally && !leave { 1 } else { 2 })
            );
        }
    }
}

#[test]
fn exhausted_unwind_roots_preserve_the_complete_fault_until_hard_teardown() {
    let p = finally_program(true);
    let registry = support(&p);
    let mut limits = VmLimits::default();
    limits.heap.max_root_ranges = 39;
    let mut v = Vm::load(
        verify(p, Limits::default()).unwrap(),
        registry,
        limits,
        SessionEpoch(4),
    )
    .unwrap();
    let t = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let blocked = loop {
        match task_slice(&mut v, t) {
            RunOutcome::BudgetYield(_) => {}
            RunOutcome::ResourceBlocked(b) => break b,
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(blocked.resource, BlockedResource::RootRanges);
    assert_eq!(v.stats().unwinds, 1);
    v.cancel_task(t).unwrap();
    while !matches!(
        v.run_slice(t, SliceBudget::default(), || 0),
        Err(VmError::Idle)
    ) {}
    assert_eq!(v.stats().slots.active_ranges, v.pooled_root_ranges());
}
#[test]
fn explicit_throw_in_catch_sets_new_source_and_retains_original_value_as_cause() {
    let mut p = fixtures::exception_regions();
    let i = Instruction::new;
    let f = &mut p.functions[0];
    f.source_map.clear();
    f.register_count = 2;
    f.code = vec![
        i(Opcode::LoadConst, 0, 1, 0),
        i(Opcode::Throw, 0, 0, 0),
        i(Opcode::LoadConst, 0, 2, 0),
        i(Opcode::Throw, 0, 0, 0),
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
            exception: Reg(1),
        }),
        finally: None,
    }];
    let mut v = vm(p);
    let t = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let d = task_finish(&mut v, t);
    collect(&mut v);
    let f = v.delivery_fault(d).unwrap().unwrap();
    assert_eq!(f.location.pc, Pc(3));
    assert!(
        v.heap()
            .fault_causes(f.id)
            .unwrap()
            .next(v.heap())
            .unwrap()
            .is_some()
    );
    assert_eq!(v.delivery_value(d).unwrap().as_int(), Some(1));
    assert_eq!(v.delivery_cause(d).unwrap().unwrap().as_int(), Some(41));
}

#[test]
fn dynamic_text_allocation_obeys_the_inherited_effect_mask() {
    let mut p = binary_program(Opcode::Add);
    p.signatures[0].effects = Effects(Effects::WRITE);
    let mut v = vm(p);
    let a = v.heap_mut().alloc_string("a").unwrap();
    let b = v.heap_mut().alloc_string("b").unwrap();
    let task = v
        .start(fixtures::ENTRY, &[a, b], OWNER, Context::Ordinary)
        .unwrap();
    let d = task_finish(&mut v, task);
    assert_eq!(
        v.delivery_fault(d).unwrap().unwrap().kind,
        FaultKind::Effect
    );
}

fn continue_at(stage: u64, faults: FaultRetention<'_>) -> Continuation<'_> {
    Continuation {
        version: 77,
        stage,
        values: &[],
        faults,
    }
}
#[test]
fn full_native_fault_identity_survives_continue_wait_and_repeated_callbacks_then_host_transfer() {
    for mode in 0..3 {
        let mut v = vm(with_callback(fixtures::helper_native(), true));
        let f = v
            .start(FunctionId(2), &[], OWNER, Context::Ordinary)
            .unwrap();
        let d = task_finish(&mut v, f);
        let closure = v.delivery_value(d).unwrap();
        let cr = v
            .heap_mut()
            .add_root(OWNER, RootKind::Host, closure)
            .unwrap();
        v.discard_delivery(d).unwrap();
        let task = v
            .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
            .unwrap();
        let mut original = None;
        let mut pending_ticket = None;
        let mut callback_count = 0;
        let delivery = loop {
            match task_slice(&mut v, task) {
                RunOutcome::BudgetYield(_) => {}
                RunOutcome::HostCall(t) => {
                    let stage = t.continuation.map_or(0, |c| {
                        assert_eq!(c.version, 77);
                        c.stage
                    });
                    match (t.input, stage) {
                        (NativeInput::Entry, 0) => v
                            .respond(
                                t,
                                HostResponse::CallScript {
                                    closure,
                                    arguments: &[],
                                    continuation: continue_at(1, FaultRetention::Preserve),
                                },
                            )
                            .unwrap(),
                        (NativeInput::CallbackFault, 1) => {
                            let info = v.host_callback_fault(t).unwrap().unwrap();
                            original = Some(info);
                            callback_count += 1;
                            assert_eq!(v.heap().fault_value(info.id).unwrap().as_int(), Some(41));
                            v.respond(
                                t,
                                HostResponse::Continue(continue_at(2, FaultRetention::Preserve)),
                            )
                            .unwrap();
                        }
                        (NativeInput::Continue { .. }, 2) => {
                            assert_eq!(
                                v.host_callback_fault(t).unwrap().unwrap().id,
                                original.unwrap().id
                            );
                            pending_ticket = Some(t);
                            v.respond(t, HostResponse::Pending(t.wait_id())).unwrap();
                            assert!(matches!(
                                v.host_resume_value(t),
                                Err(VmError::InvalidTicket)
                            ));
                        }
                        (NativeInput::WaitingReturned, 2) => {
                            assert_eq!(v.host_resume_value(t).unwrap().as_int(), Some(42));
                            assert_eq!(v.host_callback_value(t).unwrap().as_int(), Some(42));
                            assert!(matches!(
                                v.host_resume_value(pending_ticket.unwrap()),
                                Err(VmError::InvalidTicket)
                            ));
                            v.respond(
                                t,
                                HostResponse::CallScript {
                                    closure,
                                    arguments: &[],
                                    continuation: continue_at(3, FaultRetention::Preserve),
                                },
                            )
                            .unwrap();
                        }
                        (NativeInput::CallbackFault, 3) => {
                            callback_count += 1;
                            assert_ne!(
                                v.host_callback_fault(t).unwrap().unwrap().id,
                                original.unwrap().id
                            );
                            let old = original.unwrap().id;
                            v.respond(
                                t,
                                HostResponse::Continue(continue_at(
                                    4,
                                    FaultRetention::Replace(&[old]),
                                )),
                            )
                            .unwrap();
                        }
                        (NativeInput::Continue { .. }, 4) => {
                            let old = original.unwrap().id;
                            assert_eq!(
                                v.heap().fault_id(v.host_faults(t).unwrap()[0]).unwrap(),
                                old
                            );
                            let value = v.heap().fault_value(old).unwrap();
                            v.respond(
                                t,
                                match mode {
                                    0 => HostResponse::ForwardFault(old),
                                    1 => HostResponse::Error(NewFault::thrown(value)),
                                    _ => HostResponse::Error(NewFault {
                                        value,
                                        code: FaultCode::core(FaultKind::Thrown),
                                        cause: Some(old),
                                    }),
                                },
                            )
                            .unwrap();
                        }
                        other => panic!("{other:?}"),
                    }
                }
                RunOutcome::Waiting(wait) => {
                    collect(&mut v);
                    v.complete(wait, Completion::Ready(Value::int(42))).unwrap();
                }
                RunOutcome::Fault(d) => break d,
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(callback_count, 2);
        let info = v.delivery_fault(delivery).unwrap().unwrap();
        let old = original.unwrap();
        assert_eq!(info.id == old.id, mode == 0);
        assert_eq!(
            info.location.code.function,
            if mode == 0 {
                FunctionId(3)
            } else {
                FunctionId(1)
            }
        );
        let value_root = v
            .heap_mut()
            .add_root(RootOwner(44), RootKind::Host, Value::NULL)
            .unwrap();
        v.copy_delivery_value_to_root(delivery, value_root).unwrap();
        assert_eq!(v.delivery_fault(delivery).unwrap().unwrap().id, info.id);
        assert!(matches!(
            v.take_delivery_root(delivery, value_root),
            Err(VmError::Contract(_))
        ));
        let root = v.take_fault_delivery(delivery, RootOwner(45)).unwrap();
        assert!(v.is_idle());
        collect(&mut v);
        assert_eq!(root.info(v.heap()).unwrap().id, info.id);
        assert_eq!(v.heap().fault_value(root.id()).unwrap().as_int(), Some(41));
        assert_eq!(
            v.heap()
                .fault_program(root.id())
                .unwrap()
                .package()
                .content_version,
            info.location.code.version
        );
        let mut stack = v.heap().fault_stack(root.id()).unwrap();
        let mut entries = Vec::new();
        while let Some(e) = stack.next(v.heap()).unwrap() {
            entries.push(e);
        }
        assert_eq!(entries.len(), if mode == 0 { 4 } else { 3 });
        assert!(
            entries
                .iter()
                .any(|e| matches!(e.kind, StackFrameKind::Native { abi_version: 1, .. }))
        );
        let first = v
            .heap()
            .fault_causes(root.id())
            .unwrap()
            .next(v.heap())
            .unwrap();
        assert_eq!(first.is_some(), mode == 2);
        if let Some(cause) = first {
            assert_eq!(cause.kind, CauseKind::ExplicitCause);
            assert_eq!(cause.fault, Some(old.id));
        }
        assert!(matches!(
            v.heap_mut().alloc_record(ObjectKind::FaultEnvelope, &[]),
            Err(HeapError::WrongKind)
        ));
        assert!(matches!(
            v.heap_mut().set_field(root.id().as_value(), 0, Value::NULL),
            Err(HeapError::WrongKind)
        ));
        let id = root.id();
        v.heap_mut().release_fault_root(root).unwrap();
        v.heap_mut().release_root(value_root).unwrap();
        v.heap_mut().release_root(cr).unwrap();
        collect(&mut v);
        assert!(v.heap().fault_info(id).is_err());
        assert!(v.heap().fault_info(old.id).is_err());
    }
}
#[test]
fn sequential_catches_reuse_one_activation_and_helpers_observe_only_the_active_chain() {
    let mut p = fixtures::helper_native();
    p.constants.extend([Const::Int(0), Const::Int(1000)]);
    let i = Instruction::new;
    let f = &mut p.functions[0];
    f.source_map.clear();
    f.register_count = 5;
    f.code = vec![
        i(Opcode::LoadConst, 2, 3, 0),
        i(Opcode::LoadConst, 3, 4, 0),
        i(Opcode::LoadConst, 4, 2, 0),
        i(Opcode::LoadConst, 0, 1, 0),
        i(Opcode::Throw, 0, 0, 0),
        i(Opcode::Call, 1, 0, 0),
        i(Opcode::Leave, 7, 0, 0),
        i(Opcode::Add, 2, 2, 4),
        i(Opcode::Lt, 0, 2, 3),
        i(Opcode::BranchBool, 0, 3, 10),
        i(Opcode::Return, 2, 0, 0),
    ];
    let Descriptor::Call(call) = &mut f.descriptors[0] else {
        panic!()
    };
    call.arguments = Arguments::Window(RegWindow {
        start: Reg(1),
        len: 1,
    });
    f.regions = vec![ExceptionRegion {
        protected: PcRange {
            start: Pc(3),
            end: Pc(5),
        },
        catch: Some(CatchRegion {
            range: PcRange {
                start: Pc(5),
                end: Pc(7),
            },
            exception: Reg(1),
        }),
        finally: None,
    }];
    let registry = support(&p);
    let mut v = Vm::load(
        verify(p, Limits::default()).unwrap(),
        registry,
        VmLimits {
            max_unwinds: 1,
            ..VmLimits::default()
        },
        SessionEpoch(4),
    )
    .unwrap();
    let task = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let mut calls = 0;
    let mut last = None;
    let d = loop {
        match task_slice(&mut v, task) {
            RunOutcome::BudgetYield(_) => {}
            RunOutcome::HostCall(t) => {
                calls += 1;
                let info = v.error_info(task).unwrap().unwrap();
                assert_eq!(info.location.pc, Pc(4));
                assert_eq!(info.location.code.function, FunctionId(0));
                assert_ne!(last, Some(info.id));
                last = Some(info.id);
                v.respond(t, HostResponse::Ready(Value::int(42))).unwrap();
                if calls % 50 == 0 {
                    collect(&mut v);
                }
            }
            RunOutcome::Returned(d) => break d,
            RunOutcome::ResourceBlocked(_) => collect(&mut v),
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(calls, 1000);
    assert_eq!(v.delivery_value(d).unwrap().as_int(), Some(1000));
    assert_eq!(v.error_info(task).unwrap(), None);
    collect(&mut v);
    assert!(v.heap().fault_info(last.unwrap()).is_err());
}
#[test]
fn fault_capture_is_sliced_and_hard_capacity_resumes_without_reexecuting_throw() {
    let mut p = fixtures::exception_regions();
    let f = &mut p.functions[0];
    f.regions.clear();
    f.source_map.clear();
    f.code = vec![
        Instruction::new(Opcode::LoadConst, 0, 0, 0),
        Instruction::new(Opcode::Throw, 0, 0, 0),
    ];
    let registry = support(&p);
    let mut limits = VmLimits::default();
    limits.heap.max_objects = 2;
    let mut v = Vm::load(
        verify(p, Limits::default()).unwrap(),
        registry,
        limits,
        SessionEpoch(4),
    )
    .unwrap();
    let a = v.heap_mut().alloc_array(&[]).unwrap();
    let b = v.heap_mut().alloc_array(&[]).unwrap();
    let ar = v
        .heap_mut()
        .add_root(RootOwner(33), RootKind::Host, a)
        .unwrap();
    let br = v
        .heap_mut()
        .add_root(RootOwner(33), RootKind::Host, b)
        .unwrap();
    let task = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let mut quota = SliceBudget {
        instructions: 1,
        work: 1,
        allocation_bytes: 0,
        deadline_ns: None,
    };
    let mut throws = 0;
    let mut blocks = 0;
    let mut yields = 0;
    let d = loop {
        let out = v.run_slice(task, quota, || 0).unwrap();
        throws += v.stats().usage.instructions;
        gc(&mut v);
        match out {
            RunOutcome::BudgetYield(u) => {
                yields += 1;
                quota.work = (u.work + u.required_work).max(1);
                quota.allocation_bytes = (u.allocation_bytes + u.required_allocation).max(1);
            }
            RunOutcome::ResourceBlocked(blocked) => {
                blocks += 1;
                assert_eq!(blocked.resource, BlockedResource::HeapObjects);
                assert_eq!(blocked.current, 2);
                assert_eq!(blocked.required, 3);
                assert_eq!(v.stats().frames, 1);
                if blocks == 1 {
                    v.heap_mut().release_root(ar).unwrap();
                } else {
                    assert_eq!(blocks, 2);
                    v.heap_mut().release_root(br).unwrap();
                }
                collect(&mut v);
            }
            RunOutcome::Fault(d) => break d,
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(throws, 2);
    assert_eq!(blocks, 2);
    assert!(yields >= 5);
    let info = v.delivery_fault(d).unwrap().unwrap();
    assert_eq!(v.delivery_value(d).unwrap().tag(), ValueTag::Null);
    assert_eq!(
        v.heap()
            .fault_stack(info.id)
            .unwrap()
            .next(v.heap())
            .unwrap()
            .unwrap()
            .location
            .pc,
        Pc(1)
    );
}
#[test]
fn every_finally_replacement_keeps_the_complete_prior_fault_and_distinct_event() {
    let mut p = fixtures::exception_regions();
    let f = &mut p.functions[0];
    f.source_map.clear();
    let i = Instruction::new;
    f.code = vec![
        i(Opcode::LoadConst, 0, 1, 0),
        i(Opcode::Throw, 0, 0, 0),
        i(Opcode::LoadConst, 0, 2, 0),
        i(Opcode::Throw, 0, 0, 0),
        i(Opcode::EndFinally, 0, 0, 0),
        i(Opcode::LoadConst, 0, 0, 0),
        i(Opcode::Throw, 0, 0, 0),
        i(Opcode::EndFinally, 1, 0, 0),
        i(Opcode::Return, 0, 0, 0),
    ];
    f.regions = vec![
        ExceptionRegion {
            protected: PcRange {
                start: Pc(0),
                end: Pc(2),
            },
            catch: None,
            finally: Some(PcRange {
                start: Pc(2),
                end: Pc(5),
            }),
        },
        ExceptionRegion {
            protected: PcRange {
                start: Pc(0),
                end: Pc(5),
            },
            catch: None,
            finally: Some(PcRange {
                start: Pc(5),
                end: Pc(8),
            }),
        },
    ];
    let mut v = vm(p);
    let t = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let d = task_finish(&mut v, t);
    collect(&mut v);
    let third = v.delivery_fault(d).unwrap().unwrap();
    let c3 = v
        .heap()
        .fault_causes(third.id)
        .unwrap()
        .next(v.heap())
        .unwrap()
        .unwrap();
    let second = c3.fault.unwrap();
    let c2 = v
        .heap()
        .fault_causes(second)
        .unwrap()
        .next(v.heap())
        .unwrap()
        .unwrap();
    let first = c2.fault.unwrap();
    assert_eq!(v.heap().fault_value(first).unwrap().as_int(), Some(41));
    assert_eq!(v.heap().fault_value(second).unwrap().as_int(), Some(1));
    assert_eq!(
        v.heap().fault_value(third.id).unwrap().tag(),
        ValueTag::Null
    );
    assert_eq!(
        c3.kind,
        CauseKind::ReplacedExit(ReplacedExit::Throw(second))
    );
    assert_eq!(c2.kind, CauseKind::ReplacedExit(ReplacedExit::Throw(first)));
    assert_ne!(c2.event, c3.event);
    assert!(
        v.heap()
            .fault_causes(first)
            .unwrap()
            .next(v.heap())
            .unwrap()
            .is_none()
    );
}

#[test]
fn one_fault_preserves_both_its_caught_fault_and_the_return_it_replaces() {
    let mut p = fixtures::exception_regions();
    let i = Instruction::new;
    let f = &mut p.functions[0];
    f.source_map.clear();
    f.register_count = 2;
    f.code = vec![
        i(Opcode::LoadConst, 0, 1, 0),
        i(Opcode::Throw, 0, 0, 0),
        i(Opcode::LoadConst, 1, 2, 0),
        i(Opcode::Jump, 4, 0, 0),
        i(Opcode::LoadConst, 1, 2, 0),
        i(Opcode::Return, 1, 0, 0),
        i(Opcode::LoadConst, 0, 0, 0),
        i(Opcode::Throw, 0, 0, 0),
        i(Opcode::EndFinally, 0, 0, 0),
        i(Opcode::Return, 1, 0, 0),
    ];
    f.regions = vec![
        ExceptionRegion {
            protected: PcRange {
                start: Pc(4),
                end: Pc(6),
            },
            catch: None,
            finally: Some(PcRange {
                start: Pc(6),
                end: Pc(9),
            }),
        },
        ExceptionRegion {
            protected: PcRange {
                start: Pc(0),
                end: Pc(2),
            },
            catch: Some(CatchRegion {
                range: PcRange {
                    start: Pc(2),
                    end: Pc(9),
                },
                exception: Reg(1),
            }),
            finally: None,
        },
    ];
    let mut v = vm(p);
    let t = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let d = task_finish(&mut v, t);
    collect(&mut v);
    let info = v.delivery_fault(d).unwrap().unwrap();
    let mut causes = v.heap().fault_causes(info.id).unwrap();
    let replaced = causes.next(v.heap()).unwrap().unwrap();
    let caught = causes.next(v.heap()).unwrap().unwrap();
    assert_eq!(replaced.kind, CauseKind::ReplacedExit(ReplacedExit::Return));
    assert_eq!(replaced.value.as_int(), Some(1));
    assert_eq!(caught.kind, CauseKind::CaughtFault);
    assert_eq!(caught.value.as_int(), Some(41));
    assert_eq!(
        v.heap()
            .fault_info(caught.fault.unwrap())
            .unwrap()
            .location
            .pc,
        Pc(1)
    );
    assert!(causes.next(v.heap()).unwrap().is_none());
}
#[test]
fn cooperative_cancel_is_a_complete_catchable_core_error_and_wait_ticket_is_revoked() {
    let mut p = fixtures::helper_native();
    let f = &mut p.functions[1];
    let i = Instruction::new;
    f.source_map.clear();
    f.code = vec![
        i(Opcode::Call, 1, 0, 0),
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
            exception: Reg(1),
        }),
        finally: None,
    }];
    let mut v = vm(p);
    let task = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let t = task_ticket(&mut v, task);
    v.respond(t, HostResponse::Pending(t.wait_id())).unwrap();
    v.cancel_cooperatively(task).unwrap();
    let d = task_finish(&mut v, task);
    assert_eq!(v.delivery_fault(d).unwrap(), None);
    assert_eq!(v.delivery_value(d).unwrap().as_int(), Some(1));
    assert!(matches!(
        v.complete(t.wait_id(), Completion::Ready(Value::int(42))),
        Err(VmError::InvalidWait)
    ));
    let mut v = vm(fixtures::helper_native());
    let task = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let t = task_ticket(&mut v, task);
    v.respond(t, HostResponse::Pending(t.wait_id())).unwrap();
    v.cancel_cooperatively(task).unwrap();
    let d = task_finish(&mut v, task);
    let info = v.delivery_fault(d).unwrap().unwrap();
    let value = v.delivery_value(d).unwrap();
    assert_eq!(info.kind, FaultKind::Cancelled);
    let error = v.heap().core_error_info(value).unwrap();
    assert_eq!(error.code, info.code);
    assert_eq!(error.message, "cooperative cancellation");
    assert_eq!(error.details.tag(), ValueTag::Null);
}

#[test]
fn native_fault_capacity_retains_pending_source_and_resumes_without_repeating_host_actions() {
    let p = with_callback(fixtures::helper_native(), true);
    let registry = support(&p);
    let mut v = Vm::load(
        verify(p, Limits::default()).unwrap(),
        registry,
        VmLimits {
            max_native_faults: 1,
            ..VmLimits::default()
        },
        SessionEpoch(4),
    )
    .unwrap();
    let factory = v
        .start(FunctionId(2), &[], OWNER, Context::Ordinary)
        .unwrap();
    let d = task_finish(&mut v, factory);
    let closure = v.delivery_value(d).unwrap();
    let root = v
        .heap_mut()
        .add_root(OWNER, RootKind::Host, closure)
        .unwrap();
    v.discard_delivery(d).unwrap();
    let task = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let mut first = None;
    let mut second = None;
    let mut final_fault = None;
    let mut entries = 0;
    let mut waits = 0;
    let mut callbacks = 0;
    let mut blocks = 0;
    let d = loop {
        match task_slice(&mut v, task) {
            RunOutcome::BudgetYield(_) => {}
            RunOutcome::HostCall(t) => {
                let stage = t.continuation.map_or(0, |c| c.stage);
                match (t.input, stage) {
                    (NativeInput::Entry, 0) => {
                        entries += 1;
                        v.respond(
                            t,
                            HostResponse::CallScript {
                                closure,
                                arguments: &[],
                                continuation: continue_at(1, FaultRetention::Preserve),
                            },
                        )
                        .unwrap();
                    }
                    (NativeInput::CallbackFault, 1) => {
                        callbacks += 1;
                        first = Some(v.host_callback_fault(t).unwrap().unwrap().id);
                        v.respond(
                            t,
                            HostResponse::Continue(continue_at(2, FaultRetention::Preserve)),
                        )
                        .unwrap();
                    }
                    (NativeInput::WaitingFault, 2) => {
                        assert_eq!(v.host_resume_value(t).unwrap().as_int(), Some(41));
                        // Forwarding an already retained fault needs no extra fault slot.
                        assert_eq!(
                            v.host_callback_fault(t).unwrap().unwrap().id,
                            first.unwrap()
                        );
                        assert_eq!(v.host_faults(t).unwrap().len(), 1);
                        v.respond(
                            t,
                            HostResponse::Continue(continue_at(3, FaultRetention::Preserve)),
                        )
                        .unwrap();
                    }
                    (NativeInput::Continue { .. }, 3) => v
                        .respond(
                            t,
                            HostResponse::CallScript {
                                closure,
                                arguments: &[],
                                continuation: continue_at(4, FaultRetention::Preserve),
                            },
                        )
                        .unwrap(),
                    (NativeInput::CallbackFault, 4) => {
                        callbacks += 1;
                        second = Some(v.host_callback_fault(t).unwrap().unwrap().id);
                        assert_ne!(second, first);
                        v.respond(
                            t,
                            HostResponse::Continue(continue_at(5, FaultRetention::Preserve)),
                        )
                        .unwrap();
                    }
                    (NativeInput::Continue { .. }, 2 | 5) => {
                        v.respond(t, HostResponse::Pending(t.wait_id())).unwrap()
                    }
                    (NativeInput::WaitingFault, 5) => {
                        let id = v.host_callback_fault(t).unwrap().unwrap().id;
                        final_fault = Some(id);
                        assert_ne!(Some(id), second);
                        let cause = v
                            .heap()
                            .fault_causes(id)
                            .unwrap()
                            .next(v.heap())
                            .unwrap()
                            .unwrap();
                        assert_eq!(cause.kind, CauseKind::ExplicitCause);
                        assert_eq!(cause.fault, second);
                        v.respond(
                            t,
                            HostResponse::Continue(continue_at(6, FaultRetention::Preserve)),
                        )
                        .unwrap();
                    }
                    (NativeInput::Continue { .. }, 6) => v
                        .respond(t, HostResponse::ForwardFault(final_fault.unwrap()))
                        .unwrap(),
                    other => panic!("{other:?}"),
                }
            }
            RunOutcome::Waiting(w) => {
                waits += 1;
                let result = if waits == 1 {
                    Completion::ForwardFault(first.unwrap())
                } else {
                    Completion::Error(NewFault {
                        value: Value::int(41),
                        code: FaultCode::core(FaultKind::Thrown),
                        cause: second,
                    })
                };
                v.complete(w, result).unwrap();
                assert!(matches!(
                    v.complete(w, Completion::Ready(Value::int(42))),
                    Err(VmError::InvalidWait)
                ));
            }
            RunOutcome::ResourceBlocked(b) => {
                blocks += 1;
                assert_eq!((b.current, b.required), (1, 2));
                let BlockedResource::NativeFaultSlots { frame } = b.resource else {
                    panic!("{b:?}")
                };
                assert_eq!(b.task, task);
                collect(&mut v);
                v.replace_native_faults(task, frame, &[]).unwrap();
                collect(&mut v);
                if blocks == 1 {
                    assert!(v.heap().fault_info(first.unwrap()).is_err());
                } else {
                    // The partially delivered new fault still roots its explicit cause.
                    assert!(v.heap().fault_info(second.unwrap()).is_ok());
                }
            }
            RunOutcome::Fault(d) => break d,
            other => panic!("{other:?}"),
        }
    };
    assert_eq!((entries, callbacks, waits, blocks), (1, 2, 2, 2));
    assert_eq!(
        v.delivery_fault(d).unwrap().unwrap().id,
        final_fault.unwrap()
    );
    assert_eq!(v.delivery_value(d).unwrap().as_int(), Some(41));
    v.discard_delivery(d).unwrap();
    v.heap_mut().release_root(root).unwrap();
    collect(&mut v);
    assert!(v.heap().fault_info(final_fault.unwrap()).is_err());
    assert!(v.heap().fault_info(second.unwrap()).is_err());
}

#[test]
fn continued_wait_delivery_gets_its_own_fault_slot_before_continuation_values_are_replaced() {
    let mut v = vm(with_callback(fixtures::helper_native(), true));
    let factory = v
        .start(FunctionId(2), &[], OWNER, Context::Ordinary)
        .unwrap();
    let d = task_finish(&mut v, factory);
    let closure = v.delivery_value(d).unwrap();
    let root = v
        .heap_mut()
        .add_root(OWNER, RootKind::Host, closure)
        .unwrap();
    v.discard_delivery(d).unwrap();
    let task = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let entry = task_ticket(&mut v, task);
    v.respond(
        entry,
        HostResponse::CallScript {
            closure,
            arguments: &[],
            continuation: continue_at(1, FaultRetention::Preserve),
        },
    )
    .unwrap();
    let callback = task_ticket(&mut v, task);
    let id = v.host_callback_fault(callback).unwrap().unwrap().id;
    v.respond(
        callback,
        HostResponse::Continue(Continuation {
            values: &[id.as_value()],
            faults: FaultRetention::Replace(&[]),
            ..continue_at(2, FaultRetention::Preserve)
        }),
    )
    .unwrap();
    let cont = task_ticket(&mut v, task);
    assert!(v.host_faults(cont).unwrap().is_empty());
    assert_eq!(v.host_continuation(cont).unwrap().len(), 1);
    assert_eq!(
        v.heap()
            .fault_id(v.host_continuation(cont).unwrap()[0])
            .unwrap(),
        id
    );
    v.respond(cont, HostResponse::Pending(cont.wait_id()))
        .unwrap();
    v.complete(cont.wait_id(), Completion::ForwardFault(id))
        .unwrap();
    let wait = task_ticket(&mut v, task);
    assert_eq!(wait.input, NativeInput::WaitingFault);
    assert_eq!(v.host_resume_value(wait).unwrap().as_int(), Some(41));
    assert_eq!(v.host_faults(wait).unwrap().len(), 1);
    assert_eq!(
        v.heap().fault_id(v.host_faults(wait).unwrap()[0]).unwrap(),
        id
    );
    v.respond(
        wait,
        HostResponse::Continue(continue_at(3, FaultRetention::Preserve)),
    )
    .unwrap();
    let cont = task_ticket(&mut v, task);
    assert!(v.host_continuation(cont).unwrap().is_empty());
    collect(&mut v);
    assert_eq!(v.host_callback_fault(cont).unwrap().unwrap().id, id);
    assert_eq!(v.heap().fault_value(id).unwrap().as_int(), Some(41));
    v.respond(cont, HostResponse::ForwardFault(id)).unwrap();
    let d = task_finish(&mut v, task);
    assert_eq!(v.delivery_fault(d).unwrap().unwrap().id, id);
    assert_eq!(v.stats().native_entries, 1);
    v.discard_delivery(d).unwrap();
    v.heap_mut().release_root(root).unwrap();
    collect(&mut v);
    assert!(v.heap().fault_info(id).is_err());
}

#[test]
fn cooperative_cancel_aborts_deep_equality_and_cleanup_without_retaining_work_roots() {
    for caught in [false, true] {
        for cleanup in [false, true] {
            let mut p = binary_program(Opcode::Eq);
            if caught {
                let f = &mut p.functions[0];
                f.register_count = 4;
                f.code[1] = Instruction::new(Opcode::Leave, 3, 0, 0);
                f.code.push(Instruction::new(Opcode::Return, 3, 0, 0));
                f.code.push(Instruction::new(Opcode::Return, 2, 0, 0));
                f.regions = vec![ExceptionRegion {
                    protected: PcRange {
                        start: Pc(0),
                        end: Pc(2),
                    },
                    catch: Some(CatchRegion {
                        range: PcRange {
                            start: Pc(2),
                            end: Pc(3),
                        },
                        exception: Reg(3),
                    }),
                    finally: None,
                }];
            }
            let mut v = vm(p);
            let mut a = v.heap_mut().alloc_string("a").unwrap();
            let mut b = v.heap_mut().alloc_string("b").unwrap();
            let leaf = a;
            for _ in 0..12 {
                a = v.heap_mut().alloc_tuple(&[a]).unwrap();
                b = v.heap_mut().alloc_tuple(&[b]).unwrap();
            }
            let task = v
                .start(fixtures::ENTRY, &[a, b], OWNER, Context::Ordinary)
                .unwrap();
            let mut work = 1;
            loop {
                let out = v
                    .run_slice(
                        task,
                        SliceBudget {
                            instructions: 1,
                            work,
                            ..SliceBudget::default()
                        },
                        || 0,
                    )
                    .unwrap();
                gc(&mut v);
                assert!(matches!(out, RunOutcome::BudgetYield(_)));
                if v.stats().comparison_depth == 13 {
                    break;
                }
                let RunOutcome::BudgetYield(u) = out else {
                    unreachable!()
                };
                work = u.required_work.max(1);
            }
            if cleanup {
                let out = v
                    .run_slice(
                        task,
                        SliceBudget {
                            instructions: 1,
                            work: 1,
                            ..SliceBudget::default()
                        },
                        || 0,
                    )
                    .unwrap();
                assert!(matches!(out, RunOutcome::BudgetYield(_)));
                assert_eq!(v.stats().comparison_depth, 13);
            }
            v.cancel_cooperatively(task).unwrap();
            let mut work = 1;
            let mut allocation_bytes = 0;
            let d = loop {
                let out = v
                    .run_slice(
                        task,
                        SliceBudget {
                            instructions: 1,
                            work,
                            allocation_bytes,
                            ..SliceBudget::default()
                        },
                        || 0,
                    )
                    .unwrap();
                gc(&mut v);
                match out {
                    RunOutcome::BudgetYield(u) => {
                        work = u.required_work.max(1);
                        allocation_bytes = u.required_allocation;
                    }
                    RunOutcome::Returned(d) if caught => break d,
                    RunOutcome::Fault(d) if !caught => break d,
                    other => panic!("{other:?}"),
                }
            };
            assert_eq!(
                v.heap()
                    .core_error_info(v.delivery_value(d).unwrap())
                    .unwrap()
                    .code
                    .kind,
                FaultKind::Cancelled
            );
            assert_eq!(v.stats().comparison_depth, 0);
            v.discard_delivery(d).unwrap();
            collect(&mut v);
            assert!(v.heap().info(a).is_err());
            assert!(v.heap().info(b).is_err());
            assert!(v.heap().info(leaf).is_err());
            assert!(v.is_idle());
        }
    }
}

#[test]
fn cooperative_cancel_during_entry_preparation_never_enters_its_body_handler() {
    let mut v = vm(fixtures::exception_regions());
    let task = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    v.cancel_cooperatively(task).unwrap();
    let d = task_finish(&mut v, task);
    assert_eq!(
        v.delivery_fault(d).unwrap().unwrap().kind,
        FaultKind::Cancelled
    );
    assert_eq!(v.stats().usage.instructions, 0);
}

#[test]
fn exact_b01_parameter_cell_fixture_shares_default_closure_and_body_but_ordinary_calls_are_independent()
 {
    let mut v = vm(fixtures::parameter_cell_default());
    let t = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let mut work = 1;
    let mut allocation_bytes = 0;
    let d = loop {
        match v
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
            .unwrap()
        {
            RunOutcome::BudgetYield(u) => {
                work = u.required_work.max(1);
                allocation_bytes = u.required_allocation;
                gc(&mut v);
            }
            RunOutcome::Returned(d) => break d,
            other => panic!("{other:?}"),
        }
    };
    collect(&mut v);
    let result = v.delivery_value(d).unwrap();
    let owner_result = v.heap().get_field(result, 0).unwrap();
    let owner_closure = v.heap().get_field(owner_result, 1).unwrap();
    let ordinary_closure = v.heap().get_field(result, 1).unwrap();
    assert_eq!(
        v.heap().get_field(owner_result, 0).unwrap().as_int(),
        Some(43)
    );
    let owner_cell = v.heap().get_field(owner_closure, 3).unwrap();
    let ordinary_cell = v.heap().get_field(ordinary_closure, 3).unwrap();
    assert!(!owner_cell.same_bits(ordinary_cell));
    assert_eq!(v.heap().cell_read(owner_cell).unwrap().as_int(), Some(43));
    assert_eq!(
        v.heap().cell_read(ordinary_cell).unwrap().as_int(),
        Some(42)
    );
    // Two parameter Cells, two closures and two tuples; the prefix call adds no Cell.
    assert_eq!(v.heap().stats().live_objects, 6);
    for (closure, expected) in [(owner_closure, 43), (ordinary_closure, 42)] {
        let t = v
            .start_closure(closure, &[], OWNER, Context::Ordinary)
            .unwrap();
        let r = task_finish(&mut v, t);
        assert_eq!(v.delivery_value(r).unwrap().as_int(), Some(expected));
        v.discard_delivery(r).unwrap();
    }
    v.heap_mut().cell_write(owner_cell, Value::int(99)).unwrap();
    let t = v
        .start_closure(owner_closure, &[], OWNER, Context::Ordinary)
        .unwrap();
    let r = task_finish(&mut v, t);
    assert_eq!(v.delivery_value(r).unwrap().as_int(), Some(99));
    assert_eq!(
        v.heap().cell_read(ordinary_cell).unwrap().as_int(),
        Some(42)
    );
    v.discard_delivery(r).unwrap();
    v.discard_delivery(d).unwrap();
    collect(&mut v);
    assert!(v.heap().cell_info(owner_cell).is_err());
    assert!(v.heap().cell_info(ordinary_cell).is_err());
}

fn native_default_registry(closures: bool) -> RuntimeSupport {
    // Independent fixture adapter declaration; never copied from the package.
    RuntimeSupport {
        opcodes: vec![],
        capabilities: vec!["host.parameter_defaults".into()],
        natives: vec![NativeSupport {
            symbol: "host::parameter_defaults".into(),
            abi_version: 1,
            signature: Signature {
                parameters: vec![
                    Parameter {
                        name: SymbolId(4),
                        ty: TypeId(1),
                        mutable: true,
                        default: None,
                    },
                    Parameter {
                        name: SymbolId(14),
                        ty: TypeId(if closures { 0 } else { 1 }),
                        mutable: false,
                        default: Some(FunctionId(1)),
                    },
                    Parameter {
                        name: SymbolId(15),
                        ty: TypeId(if closures { 0 } else { 1 }),
                        mutable: true,
                        default: Some(FunctionId(2)),
                    },
                ],
                rest: Some(Parameter {
                    name: SymbolId(18),
                    ty: TypeId(0),
                    mutable: true,
                    default: None,
                }),
                return_type: TypeId(0),
                effects: Effects::ALL,
                property_bag_tail: None,
            },
            required_capabilities: vec!["host.parameter_defaults".into()],
        }],
    }
}

#[test]
fn readonly_value_native_defaults_use_no_cell_environment_and_allow_ready_in_nowait() {
    let mut p = fixtures::native_parameter_defaults();
    p.signatures[1].parameters[0].mutable = false;
    p.signatures[2].parameters[0].mutable = false;
    p.signatures[3].parameters[0].mutable = false;
    p.signatures[2].effects = Effects(0);
    p.signatures[3].effects = Effects(Effects::ALLOCATE);
    p.functions[1].bindings[0].mutable = false;
    p.functions[1].bindings[0].storage = BindingStorage::Value(Reg(0));
    p.functions[1].code = vec![
        Instruction::new(Opcode::LoadConst, 1, 1, 0),
        Instruction::new(Opcode::Return, 1, 0, 0),
    ];
    p.functions[2].bindings[0].mutable = false;
    p.functions[2].bindings[0].storage = BindingStorage::Value(Reg(0));
    p.functions[2].bindings[1].storage = BindingStorage::Value(Reg(1));
    p.functions[2].code = vec![
        Instruction::new(Opcode::MakeClosure, 2, 0, 0),
        Instruction::new(Opcode::Call, 3, 1, 0),
        Instruction::new(Opcode::Return, 3, 0, 0),
    ];
    p.functions[3].captures = vec![CaptureKind::Value];
    p.functions[3].code = vec![
        Instruction::new(Opcode::CaptureRead, 0, 0, 0),
        Instruction::new(Opcode::Return, 0, 0, 0),
    ];
    let mut registry = native_default_registry(false);
    registry.natives[0].signature.parameters[0].mutable = false;
    let program = verify(p, Limits::default()).unwrap();
    assert!(!program.native_parameters_need_cells(NativeId(0)).unwrap());
    let mut v = Vm::load(program, registry, VmLimits::default(), SessionEpoch(4)).unwrap();
    let task = v
        .start(fixtures::ENTRY, &[], OWNER, Context::NoWait)
        .unwrap();
    let t = task_ticket(&mut v, task);
    collect(&mut v);
    assert_eq!(
        v.stats().slots.active_ranges,
        v.pooled_root_ranges() + 4 + 3
    );
    assert_eq!(v.heap().stats().live_objects, 1); // Only the native's empty rest array.
    for value in &v.host_arguments(t).unwrap()[..3] {
        assert_eq!(value.as_int(), Some(41));
    }
    v.respond(t, HostResponse::Ready(Value::NULL)).unwrap();
    let d = task_finish(&mut v, task);
    assert_eq!(v.delivery_value(d).unwrap().tag(), ValueTag::Null);
    assert_eq!(v.stats().native_entries, 1);
    v.discard_delivery(d).unwrap();
    collect(&mut v);
    assert_eq!(v.heap().stats().live_objects, 0);
}
fn native_default_vm(p: BytecodePackage, closures: bool) -> Vm {
    Vm::load(
        verify(p, Limits::default()).unwrap(),
        native_default_registry(closures),
        VmLimits::default(),
        SessionEpoch(4),
    )
    .unwrap()
}
fn add_native_callable(p: &mut BytecodePackage) -> FunctionId {
    let id = FunctionId(p.functions.len() as u32);
    p.functions.push(FunctionDef {
        module: ModuleId(0),
        name: p.natives[0].symbol,
        kind: FunctionKind::Native(NativeId(0)),
        signature: p.natives[0].signature,
        context: Context::Ordinary,
        register_count: 0,
        captures: vec![],
        bindings: vec![],
        descriptors: vec![],
        code: vec![],
        regions: vec![],
        source_map: vec![],
    });
    id
}
fn through_native_value(mut p: BytecodePackage) -> BytecodePackage {
    let id = add_native_callable(&mut p);
    for f in &mut p.functions {
        for d in &mut f.descriptors {
            if let Descriptor::Call(c) = d
                && c.target == CallTarget::Native(NativeId(0))
            {
                c.target = CallTarget::Function(id);
            }
        }
    }
    p
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
#[test]
fn exact_b01_native_default_fixture_prepares_cells_defaults_and_author_values_before_one_entry() {
    for via_value in [false, true] {
        for pending in [false, true] {
            let mut v = native_default_vm(
                if via_value {
                    through_native_value(fixtures::native_parameter_defaults())
                } else {
                    fixtures::native_parameter_defaults()
                },
                false,
            );
            let t = v
                .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
                .unwrap();
            let mut work = 1;
            let mut allocation_bytes = 0;
            let mut entries = 0;
            let d = loop {
                let out = v
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
                gc(&mut v);
                match out {
                    RunOutcome::BudgetYield(u) => {
                        work = u.required_work.max(1);
                        allocation_bytes = u.required_allocation;
                    }
                    RunOutcome::HostCall(ticket) => {
                        entries += 1;
                        assert_eq!(ticket.input, NativeInput::Entry);
                        assert_eq!(v.host_arguments(ticket).unwrap().len(), 4);
                        assert_eq!(v.host_arguments(ticket).unwrap()[0].as_int(), Some(43));
                        assert_eq!(v.host_arguments(ticket).unwrap()[1].as_int(), Some(42));
                        assert_eq!(v.host_arguments(ticket).unwrap()[2].as_int(), Some(42));
                        let rest = v.host_arguments(ticket).unwrap()[3];
                        assert_eq!(v.heap().info(rest).unwrap().kind, ObjectKind::Array);
                        assert_eq!(v.heap().info(rest).unwrap().len, 0);
                        v.respond(
                            ticket,
                            if pending {
                                HostResponse::Pending(ticket.wait_id())
                            } else {
                                HostResponse::Ready(Value::NULL)
                            },
                        )
                        .unwrap();
                    }
                    RunOutcome::Waiting(w) => {
                        collect(&mut v);
                        v.complete(w, Completion::Ready(Value::NULL)).unwrap();
                    }
                    RunOutcome::Returned(d) => break d,
                    other => panic!("{other:?}"),
                }
            };
            assert_eq!(entries, 1);
            assert_eq!(v.stats().native_entries, 1);
            assert_eq!(v.delivery_value(d).unwrap().tag(), ValueTag::Null);
            v.discard_delivery(d).unwrap();
            collect(&mut v);
            assert_eq!(v.stats().slots.active_ranges, v.pooled_root_ranges());
            assert_eq!(v.heap().stats().live_objects, u32::from(via_value));
        }
    }
}

fn native_closure_defaults() -> BytecodePackage {
    let mut p = fixtures::native_parameter_defaults();
    p.signatures[1].parameters[1].ty = TypeId(0);
    p.signatures[1].parameters[2].ty = TypeId(0);
    p.signatures[2].return_type = TypeId(0);
    p.signatures[2].effects = Effects(Effects::WRITE | Effects::ALLOCATE);
    p.signatures[3].parameters[1].ty = TypeId(0);
    p.signatures[3].return_type = TypeId(0);
    p.signatures[4].return_type = TypeId(0);
    p.functions[2].bindings[1].ty = TypeId(0);
    let i = Instruction::new;
    p.functions[1].code[4] = i(Opcode::MakeClosure, 3, 0, 0);
    p.functions[1].code.push(i(Opcode::Return, 3, 0, 0));
    p.functions[1].descriptors = vec![Descriptor::Closure {
        function: FunctionId(4),
        captures: vec![CaptureSource::Binding(BindingId(0))],
    }];
    let mut writer = p.functions[3].clone();
    writer.register_count = 3;
    writer.signature = SignatureId(5);
    writer.code = vec![
        i(Opcode::CaptureRead, 0, 0, 0),
        i(Opcode::CellRead, 1, 0, 0),
        i(Opcode::LoadConst, 2, 2, 0),
        i(Opcode::Add, 1, 1, 2),
        i(Opcode::CellWrite, 0, 1, 0),
        i(Opcode::Return, 1, 0, 0),
    ];
    p.signatures.push(Signature {
        parameters: vec![],
        rest: None,
        return_type: TypeId(1),
        effects: Effects(Effects::WRITE),
        property_bag_tail: None,
    });
    p.functions.push(writer);
    p
}
#[test]
fn native_arguments_are_reprojected_after_default_closure_callbacks_and_continued_wait() {
    for via_value in [false, true] {
        let mut v = native_default_vm(
            if via_value {
                through_native_value(native_closure_defaults())
            } else {
                native_closure_defaults()
            },
            true,
        );
        let task = v
            .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
            .unwrap();
        let mut closure = None;
        let mut cell = None;
        let mut callbacks = 0;
        let d = loop {
            match task_slice(&mut v, task) {
                RunOutcome::BudgetYield(_) => {}
                RunOutcome::HostCall(t) => {
                    let stage = t.continuation.map_or(0, |c| c.stage);
                    match (t.input, stage) {
                        (NativeInput::Entry, 0) => {
                            let args = v.host_arguments(t).unwrap();
                            assert_eq!(args[0].as_int(), Some(43));
                            assert!(args[1].same_bits(args[2]));
                            closure = Some(args[1]);
                            cell = Some(v.heap().get_field(args[1], 3).unwrap());
                            v.respond(
                                t,
                                HostResponse::CallScript {
                                    closure: closure.unwrap(),
                                    arguments: &[],
                                    continuation: continue_at(1, FaultRetention::Preserve),
                                },
                            )
                            .unwrap();
                        }
                        (NativeInput::CallbackReturned, 1 | 3) => {
                            callbacks += 1;
                            let expected = if stage == 1 { 44 } else { 45 };
                            assert_eq!(v.host_resume_value(t).unwrap().as_int(), Some(expected));
                            assert_eq!(v.host_arguments(t).unwrap()[0].as_int(), Some(expected));
                            assert_eq!(
                                v.heap().cell_read(cell.unwrap()).unwrap().as_int(),
                                Some(expected)
                            );
                            if stage == 1 {
                                v.respond(
                                    t,
                                    HostResponse::Continue(continue_at(
                                        2,
                                        FaultRetention::Preserve,
                                    )),
                                )
                                .unwrap();
                            } else {
                                v.respond(t, HostResponse::Pending(t.wait_id())).unwrap();
                            }
                        }
                        (NativeInput::Continue { .. }, 2) => {
                            assert_eq!(v.host_arguments(t).unwrap()[0].as_int(), Some(44));
                            v.respond(
                                t,
                                HostResponse::CallScript {
                                    closure: closure.unwrap(),
                                    arguments: &[],
                                    continuation: continue_at(3, FaultRetention::Preserve),
                                },
                            )
                            .unwrap();
                        }
                        (NativeInput::WaitingReturned, 3) => {
                            assert_eq!(v.host_resume_value(t).unwrap().as_int(), Some(99));
                            assert_eq!(v.host_arguments(t).unwrap()[0].as_int(), Some(45));
                            v.respond(t, HostResponse::Ready(Value::NULL)).unwrap();
                        }
                        other => panic!("{other:?}"),
                    }
                    assert!(matches!(v.host_arguments(t), Err(VmError::InvalidTicket)));
                }
                RunOutcome::Waiting(w) => {
                    collect(&mut v);
                    v.complete(w, Completion::Ready(Value::int(99))).unwrap();
                }
                RunOutcome::Returned(d) => break d,
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(callbacks, 2);
        assert_eq!(v.stats().native_entries, 1);
        v.discard_delivery(d).unwrap();
        collect(&mut v);
        assert!(v.heap().cell_info(cell.unwrap()).is_err());
        assert_eq!(v.stats().slots.active_ranges, v.pooled_root_ranges());
    }
}

#[test]
fn reported_retry_grants_finish_helper_returns_under_a_live_catch_independent_of_unused_capacity() {
    let mut measured = Vec::new();
    for (roots, ranges) in [(16, 64), (4096, 4096)] {
        let mut p = fixtures::helper_native();
        let f = &mut p.functions[0];
        f.source_map.clear();
        let i = Instruction::new;
        f.code = vec![
            i(Opcode::LoadConst, 0, 1, 0),
            i(Opcode::Throw, 0, 0, 0),
            i(Opcode::Call, 1, 0, 0),
            i(Opcode::Leave, 4, 0, 0),
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
                exception: Reg(0),
            }),
            finally: None,
        }];
        let mut limits = VmLimits::default();
        limits.heap.max_roots = roots;
        limits.heap.max_root_ranges = ranges;
        let mut v = Vm::load(
            verify(p, Limits::default()).unwrap(),
            support(&fixtures::helper_native()),
            limits,
            SessionEpoch(4),
        )
        .unwrap();
        let t = v
            .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
            .unwrap();
        let mut quota = SliceBudget {
            instructions: 1,
            work: 1,
            allocation_bytes: 0,
            deadline_ns: None,
        };
        let mut work = 0;
        let mut delivery = None;
        for _ in 0..1000 {
            let out = v.run_slice(t, quota, || 0).unwrap();
            work += v.stats().usage.work;
            gc(&mut v);
            match out {
                RunOutcome::BudgetYield(u) => {
                    // Grant exactly the reported action, without paying earlier
                    // unsaved work again via u.work + u.required_work.
                    quota.work = u.required_work.max(1);
                    quota.allocation_bytes = u.required_allocation;
                }
                RunOutcome::HostCall(ticket) => {
                    assert!(v.error_info(t).unwrap().is_some());
                    v.respond(ticket, HostResponse::Pending(ticket.wait_id()))
                        .unwrap();
                }
                RunOutcome::Waiting(wait) => {
                    v.complete(wait, Completion::Ready(Value::int(42))).unwrap()
                }
                RunOutcome::Returned(d) => {
                    delivery = Some(d);
                    break;
                }
                other => panic!("{other:?}"),
            }
        }
        let d = delivery.expect("reported grants must finish the live-catch helper return");
        assert_eq!(v.delivery_value(d).unwrap().as_int(), Some(42));
        assert_eq!(v.stats().native_entries, 1);
        assert_eq!(v.stats().slots.active_ranges, v.pooled_root_ranges());
        v.discard_delivery(d).unwrap();
        measured.push(work);
    }
    assert_eq!(
        measured[0], measured[1],
        "unused descriptor capacity is not call work"
    );
}

#[test]
fn adaptive_call_precharge_covers_partial_window_rollback() {
    let p = fixtures::helper_native();
    let mut limits = VmLimits {
        max_tasks: 1,
        ..VmLimits::default()
    };
    // Four pooled roots + four caller roots + one successful child open;
    // the second child open fails and must release the first before faulting.
    limits.heap.max_root_ranges = 9;
    let mut v = Vm::load(
        verify(p.clone(), Limits::default()).unwrap(),
        support(&p),
        limits,
        SessionEpoch(4),
    )
    .unwrap();
    let t = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let mut quota = SliceBudget {
        instructions: 1,
        work: 1,
        allocation_bytes: 0,
        deadline_ns: None,
    };
    let mut delivery = None;
    for _ in 0..1000 {
        let out = v.run_slice(t, quota, || 0).unwrap();
        gc(&mut v);
        match out {
            RunOutcome::BudgetYield(u) => {
                quota.work = u.required_work.max(1);
                quota.allocation_bytes = u.required_allocation;
            }
            RunOutcome::Fault(d) => {
                delivery = Some(d);
                break;
            }
            other => panic!("{other:?}"),
        }
    }
    let d = delivery.expect("partial construction rollback must finish under reported grants");
    let info = v.delivery_fault(d).unwrap().unwrap();
    assert_eq!(info.kind, FaultKind::Resource);
    assert_eq!(info.location.pc, Pc(1));
    assert_eq!(v.stats().native_entries, 0);
    assert_eq!(v.stats().slots.active_ranges, v.pooled_root_ranges());
    v.discard_delivery(d).unwrap();
    collect(&mut v);
    assert_eq!(v.heap().stats().live_objects, 0);
}
