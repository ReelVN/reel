use aside_vm::{bytecode::*, executor::*, *};
const OWNER: RootOwner = RootOwner(17);
fn support(p: &BytecodePackage) -> RuntimeSupport {
    RuntimeSupport {
        opcodes: vec![],
        capabilities: p
            .capabilities
            .iter()
            .map(|s| p.symbols[s.index()].clone())
            .collect(),
        natives: p
            .natives
            .iter()
            .map(|n| NativeSupport {
                symbol: p.symbols[n.symbol.index()].clone(),
                abi_version: n.abi_version,
                signature: p.signatures[n.signature.index()].clone(),
            })
            .collect(),
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
        assert_eq!(v.stats().slots.active_ranges, 3);
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
        type_id: TypeId(7),
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
    let mut v = vm(p);
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
    let RunOutcome::BudgetYield(cost) = v.run_slice(tiny, || 0).unwrap() else {
        panic!()
    };
    assert!(cost.required_work > 1);
    assert_eq!(cost.work, 0);
    let mut steps = 0;
    loop {
        steps += 1;
        match v.run_slice(
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
    assert_eq!(v.stats().slots.active_ranges, 3);
}
#[test]
fn load_rejects_unconnected_handlers_even_if_unreachable() {
    let mut p = fixtures::exception_regions();
    p.functions[0].code[0] = Instruction::new(Opcode::MakeArray, 0, 0, 0);
    p.functions[0]
        .descriptors
        .push(Descriptor::Sequence(RegWindow {
            start: Reg(0),
            len: 0,
        }));
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
