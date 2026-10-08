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
                required_capabilities: n
                    .capabilities
                    .iter()
                    .map(|c| p.symbols[c.index()].clone())
                    .collect(),
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
        assert_eq!(v.stats().slots.active_ranges, 4);
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
    assert_eq!(v.stats().slots.active_ranges, 4);
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
                FaultKind::InvalidState,
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
                match v.run_slice(SliceBudget::default(), || 0).unwrap() {
                    RunOutcome::HostCall(t) => {
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
    assert_eq!(v.stats().slots.active_ranges, 4);
    assert_eq!(v.stats().frames, 0);
    assert_eq!(v.stats().native_entries, 1000);
}
#[test]
fn slice_deadline_allocation_and_work_limits_preserve_unexecuted_operation() {
    let mut v = vm(fixtures::cell_closures());
    v.start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let initial = v.stats();
    assert!(matches!(
        v.run_slice(
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
        assert_eq!(v.stats().slots.active_ranges, 4);
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
