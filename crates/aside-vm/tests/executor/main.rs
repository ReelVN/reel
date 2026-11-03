use aside_vm::{bytecode::*, executor::*, *};
const OWNER: RootOwner = RootOwner(17);
fn support(_p: &BytecodePackage) -> RuntimeSupport {
    // This fixture adapter is declared independently of package native metadata.
    // Its fixed fixture symbol/type table is Any=0, Int=1, x=4.
    RuntimeSupport {
        host_types: Vec::new(),
        opcodes: vec![],
        capabilities: vec!["host.echo".into()],
        natives: vec![NativeSupport {
            standard_derived_commit: false,
            read_only_leaf: None,
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
fn module_roots_and_lazy_literals_do_not_consume_the_execution_window_allowance() {
    let mut p = fixtures::helper_native();
    p.constants.extend((0..9_000).map(Const::Int));
    let last = p.constants.len() as u32 - 1;
    p.functions[0].code[0] = Instruction::new(Opcode::LoadConst, 0, last, 0);
    p.functions[0].code[1] = Instruction::new(Opcode::Move, 1, 0, 0);
    let static_roots = p.symbols.len();
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
fn cell_read_rejects_noncell_containers() {
    let mut h = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
    for kind in [ObjectKind::Closure, ObjectKind::Instance] {
        let v = h.alloc_record(kind, &[Value::int(1)]).unwrap();
        assert!(matches!(h.cell_read(v), Err(HeapError::WrongKind)));
    }
    let a = h.alloc_array(&[Value::int(1)]).unwrap();
    assert!(matches!(h.cell_read(a), Err(HeapError::WrongKind)));
}

fn inner_fault_in_finally(pending_throw: bool, fault: u8) -> BytecodePackage {
    let mut p = with_callback(fixtures::helper_native(), true);
    let i = Instruction::new;
    let f = &mut p.functions[0];
    f.source_map.clear();
    f.register_count = 3;
    f.descriptors = vec![Descriptor::Call(CallDesc {
        contract: None,
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
        version: 1,
        stage,
        values: &[],
        faults,
    }
}

fn native_default_registry(closures: bool) -> RuntimeSupport {
    // Independent fixture adapter declaration; never copied from the package.
    RuntimeSupport {
        host_types: Vec::new(),
        opcodes: vec![],
        capabilities: vec!["host.parameter_defaults".into()],
        natives: vec![NativeSupport {
            standard_derived_commit: false,
            read_only_leaf: None,
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
                        default: Some(ParameterDefault::Function(FunctionId(1))),
                    },
                    Parameter {
                        name: SymbolId(15),
                        ty: TypeId(if closures { 0 } else { 1 }),
                        mutable: true,
                        default: Some(ParameterDefault::Function(FunctionId(2))),
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

mod faults;
mod native;
mod scheduling;

fn assert_instruction(vm: &Vm, location: Location, ordinal: usize) {
    let code = &vm.program().package().functions[location.code.function.index()].code;
    let (pc, _) = code.indexed_iter().nth(ordinal).unwrap().unwrap();
    assert_eq!(location.pc, Pc(pc as u32));
}

fn warm_code(vm: &Vm) {
    for function in &vm.program().package().functions {
        for word in function.code.words() {
            word.unwrap();
        }
    }
}
