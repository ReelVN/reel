//! Execution regressions for bounded collections, spread/rest and their GC roots.
use super::*;
mod clears;
mod equality;
mod operand_cost;
mod search_tests;

fn op(opcode: Opcode, a: u32, b: u32, c: u32) -> Instruction {
    Instruction::new(opcode, a, b, c)
}
fn package(
    constants: Vec<Const>,
    code: Vec<Instruction>,
    descriptors: Vec<Descriptor>,
    registers: u32,
) -> BytecodePackage {
    let mut p = fixtures::helper_native();
    p.natives.clear();
    p.capabilities.clear();
    p.signatures.truncate(1);
    p.signatures[0].parameters.clear();
    p.signatures[0].return_type = TypeId(0);
    p.functions.truncate(1);
    p.constants = constants;
    let f = &mut p.functions[0];
    f.signature = SignatureId(0);
    f.register_count = registers;
    f.descriptors = descriptors;
    f.source_map = (0..code.len())
        .map(|pc| SourceMapEntry {
            pc: Pc(pc as u32),
            span: SourceSpan {
                file: SourceId(0),
                start: 0,
                end: 1,
            },
        })
        .collect();
    f.code = code;
    p
}
fn load(p: BytecodePackage) -> Vm {
    load_with_limits(p, VmLimits::default())
}
fn load_with_limits(p: BytecodePackage, limits: VmLimits) -> Vm {
    Vm::load(
        verify(p, Limits::default()).unwrap(),
        RuntimeSupport {
            host_types: Vec::new(),
            opcodes: vec![],
            capabilities: vec![],
            natives: vec![],
        },
        limits,
        SessionEpoch(81),
    )
    .unwrap()
}
fn gc(v: &mut Vm) {
    if v.heap().phase() == GcPhase::Idle {
        v.begin_collection().unwrap();
    }
    v.gc_step(GcBudget {
        units: 1,
        ..GcBudget::default()
    })
    .unwrap();
}
fn tick(v: &mut Vm, task: TaskId) -> RunOutcome {
    let outcome = v
        .run_slice(
            task,
            SliceBudget {
                instructions: 1,
                work: 128.max(v.last_usage.required_work),
                allocation_bytes: VALUE_CHUNK_BYTES.max(v.last_usage.required_allocation),
                ..SliceBudget::default()
            },
            || 0,
        )
        .unwrap();
    gc(v);
    outcome
}
fn terminal(v: &mut Vm, task: TaskId) -> RunOutcome {
    for _ in 0..20000 {
        let out = tick(v, task);
        if !matches!(out, RunOutcome::BudgetYield(_)) {
            return out;
        }
    }
    panic!("bounded test driver exhausted");
}
fn returned(v: &mut Vm, task: TaskId) -> Value {
    let RunOutcome::Returned(delivery) = terminal(v, task) else {
        panic!("expected return")
    };
    v.delivery_value(delivery).unwrap()
}
fn fault(v: &mut Vm, task: TaskId, expected: FaultKind) {
    let RunOutcome::Fault(delivery) = terminal(v, task) else {
        panic!("expected fault")
    };
    assert_eq!(v.delivery_fault(delivery).unwrap().unwrap().kind, expected);
}
fn sequence(opcode: Opcode, len: u32) -> BytecodePackage {
    let mut code = (0..len)
        .map(|r| op(Opcode::LoadConst, r, 0, 0))
        .collect::<Vec<_>>();
    code.push(op(opcode, len, 0, 0));
    code.push(op(Opcode::Return, len, 0, 0));
    package(
        vec![Const::Int(17)],
        code,
        vec![Descriptor::Sequence(RegWindow { start: Reg(0), len })],
        len + 1,
    )
}

#[test]
fn packed_clears_charge_each_slot_and_release_only_the_named_roots() {
    for (opcode, count) in [(Opcode::Clear2, 2), (Opcode::Clear3, 3)] {
        let mut vm = load(package(
            vec![Const::Int(41)],
            vec![
                op(Opcode::LoadConst, 1, 0, 0),
                op(Opcode::MakeArray, 0, 0, 0),
                op(Opcode::MakeArray, 2, 0, 0),
                op(Opcode::MakeArray, 3, 0, 0),
                op(opcode, 0, 2, if count == 3 { 3 } else { 0 }),
                op(Opcode::Return, 1, 0, 0),
            ],
            vec![Descriptor::Sequence(RegWindow {
                start: Reg(0),
                len: 0,
            })],
            4,
        ));
        let task = vm
            .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
            .unwrap();
        let frame = loop {
            if let Some(Frame::Script(frame)) = vm.active.frames.data.last().copied()
                && frame.pc == test_pc(&vm, frame.code.function.0, 4)
            {
                break frame;
            }
            assert!(matches!(tick(&mut vm, task), RunOutcome::BudgetYield(_)));
        };
        let objects = [0, 2, 3].map(|register| vm.slots.read(at(frame.regs, register)).unwrap());
        for work in [count - 1, count] {
            let outcome = vm
                .run_slice(
                    task,
                    SliceBudget {
                        instructions: 1,
                        work,
                        allocation_bytes: 0,
                        deadline_ns: None,
                    },
                    || 0,
                )
                .unwrap();
            assert!(matches!(outcome, RunOutcome::BudgetYield(_)));
            for (index, register) in [0, 2, 3].into_iter().enumerate() {
                if work == count && index < count as usize {
                    assert!(matches!(
                        vm.slots.read(at(frame.regs, register)),
                        Err(HeapError::UninitializedSlot)
                    ));
                } else {
                    assert!(
                        vm.slots
                            .read(at(frame.regs, register))
                            .unwrap()
                            .same_bits(objects[index])
                    );
                }
            }
        }
        super::review_tests::collect(&mut vm);
        for (index, object) in objects.into_iter().enumerate() {
            assert_eq!(vm.heap().info(object).is_err(), index < count as usize);
        }
        assert_eq!(returned(&mut vm, task).as_int(), Some(41));
    }
}
#[test]
fn long_sequences_publish_only_after_budgeted_copy_and_survive_gc() {
    for opcode in [Opcode::MakeTuple, Opcode::MakeArray] {
        let mut vm = load(sequence(opcode, 513));
        let task = vm
            .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
            .unwrap();
        for _ in 0..2000 {
            if matches!(vm.active.frames.data.last(), Some(Frame::Script(s)) if matches!(s.phase, ScriptPhase::Sequence { .. }))
            {
                break;
            }
            assert!(matches!(tick(&mut vm, task), RunOutcome::BudgetYield(_)));
        }
        let Frame::Script(s) = *vm.active.frames.data.last().unwrap() else {
            panic!()
        };
        assert!(matches!(s.phase, ScriptPhase::Sequence { .. }));
        assert!(vm.slots.read(at(s.regs, 513)).is_err());
        let before = vm.heap().info(vm.value(s.env, 1).unwrap()).unwrap().len;
        let RunOutcome::BudgetYield(usage) = vm
            .run_slice(
                task,
                SliceBudget {
                    work: 0,
                    ..SliceBudget::default()
                },
                || 0,
            )
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(usage.work, 0);
        assert_eq!(
            vm.heap().info(vm.value(s.env, 1).unwrap()).unwrap().len,
            before
        );
        let value = returned(&mut vm, task);
        assert_eq!(vm.heap().sequence_len(value).unwrap(), 513);
        assert_eq!(
            vm.heap().sequence_get(value, 512).unwrap().as_int(),
            Some(17)
        );
        assert_eq!(
            vm.heap().info(value).unwrap().kind,
            if opcode == Opcode::MakeTuple {
                ObjectKind::Tuple
            } else {
                ObjectKind::Array
            }
        );
    }
}
#[test]
fn tuple_constants_are_shallow_dag_values_and_not_null() {
    let mut vm = load(package(
        vec![
            Const::Int(4),
            Const::Tuple(vec![]),
            Const::Tuple(vec![ConstId(0), ConstId(1)]),
        ],
        vec![op(Opcode::LoadConst, 0, 2, 0), op(Opcode::Return, 0, 0, 0)],
        vec![],
        1,
    ));
    let task = vm
        .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
        .unwrap();
    let value = returned(&mut vm, task);
    assert_eq!(vm.heap().sequence_len(value).unwrap(), 2);
    let empty = vm.heap().sequence_get(value, 1).unwrap();
    assert_eq!(vm.heap().sequence_len(empty).unwrap(), 0);
    assert_ne!(empty.tag(), ValueTag::Null);
}
#[test]
fn sequence_index_rejects_float_negative_and_bounds_without_coercion() {
    for (index, kind) in [
        (Const::FloatBits(0f64.to_bits()), FaultKind::Type),
        (Const::Int(-1), FaultKind::OutOfBounds),
        (Const::Int(1), FaultKind::OutOfBounds),
    ] {
        let mut vm = load(package(
            vec![Const::Int(8), index],
            vec![
                op(Opcode::LoadConst, 0, 0, 0),
                op(Opcode::MakeArray, 1, 0, 0),
                op(Opcode::LoadConst, 2, 1, 0),
                op(Opcode::IndexGet, 3, 1, 2),
                op(Opcode::Return, 3, 0, 0),
            ],
            vec![Descriptor::Sequence(RegWindow {
                start: Reg(0),
                len: 1,
            })],
            4,
        ));
        let task = vm
            .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
            .unwrap();
        fault(&mut vm, task, kind);
    }
}
#[test]
fn tuple_index_write_is_read_only_but_array_aliases_share_mutation() {
    for opcode in [Opcode::MakeTuple, Opcode::MakeArray] {
        let mut vm = load(package(
            vec![Const::Int(0), Const::Int(99)],
            vec![
                op(Opcode::LoadConst, 0, 0, 0),
                op(opcode, 1, 0, 0),
                op(Opcode::Move, 2, 1, 0),
                op(Opcode::LoadConst, 3, 1, 0),
                op(Opcode::IndexSet, 2, 0, 3),
                op(Opcode::IndexGet, 4, 1, 0),
                op(Opcode::Return, 4, 0, 0),
            ],
            vec![Descriptor::Sequence(RegWindow {
                start: Reg(0),
                len: 1,
            })],
            5,
        ));
        let task = vm
            .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
            .unwrap();
        if opcode == Opcode::MakeTuple {
            fault(&mut vm, task, FaultKind::ReadOnly);
        } else {
            assert_eq!(returned(&mut vm, task).as_int(), Some(99));
        }
    }
}
fn table_package(keys: Vec<Const>, values: Vec<Const>) -> BytecodePackage {
    let len = keys.len();
    let mut constants = keys;
    constants.extend(values);
    let mut code = (0..len * 2)
        .map(|r| op(Opcode::LoadConst, r as u32, r as u32, 0))
        .collect::<Vec<_>>();
    code.push(op(Opcode::MakeTable, (len * 2) as u32, 0, 0));
    code.push(op(Opcode::Return, (len * 2) as u32, 0, 0));
    package(
        constants,
        code,
        vec![Descriptor::Table(
            (0..len)
                .map(|i| [Reg(i as u32), Reg((len + i) as u32)])
                .collect(),
        )],
        (len * 2 + 1) as u32,
    )
}
#[test]
fn table_numeric_keys_cover_duplicates_zero_bool_and_large_int_precision() {
    let mut vm = load(table_package(
        vec![
            Const::Int(0),
            Const::FloatBits((-0f64).to_bits()),
            Const::Bool(false),
            Const::Int(9007199254740993),
            Const::FloatBits(9007199254740992f64.to_bits()),
        ],
        vec![
            Const::Int(1),
            Const::Int(2),
            Const::Int(3),
            Const::Int(4),
            Const::Int(5),
        ],
    ));
    let task = vm
        .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
        .unwrap();
    let table = returned(&mut vm, task);
    assert_eq!(vm.heap().info(table).unwrap().len, 4);
    assert_eq!(vm.heap().table_entry(table, 0).unwrap().1.as_int(), Some(2));
    assert_eq!(vm.heap().table_entry(table, 1).unwrap().1.as_int(), Some(3));
    assert_eq!(vm.heap().table_entry(table, 2).unwrap().1.as_int(), Some(4));
    assert_eq!(vm.heap().table_entry(table, 3).unwrap().1.as_int(), Some(5));
}
#[test]
fn table_tuple_keys_compare_by_value_and_reject_nested_nan() {
    let mut p = table_package(
        vec![Const::Tuple(vec![]), Const::Tuple(vec![])],
        vec![Const::Int(1), Const::Int(2)],
    );
    let mut vm = load(p.clone());
    let task = vm
        .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
        .unwrap();
    let table = returned(&mut vm, task);
    assert_eq!(vm.heap().info(table).unwrap().len, 1);
    assert_eq!(vm.heap().table_entry(table, 0).unwrap().1.as_int(), Some(2));
    p.constants[0] = Const::FloatBits(f64::NAN.to_bits());
    p.constants[1] = Const::Tuple(vec![ConstId(0)]);
    let mut vm = load(p);
    let task = vm
        .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
        .unwrap();
    fault(&mut vm, task, FaultKind::UnhashableKey);
}
#[test]
fn member_set_operand_order_and_missing_null_are_distinct() {
    let mut p = table_package(vec![Const::Utf8("x".into())], vec![Const::Null]);
    p.functions[0].code.pop();
    p.functions[0].code.extend([
        op(Opcode::MemberGet, 0, 2, 4),
        op(Opcode::LoadConst, 1, 0, 0),
        op(Opcode::MemberSet, 2, 1, 4),
        op(Opcode::MemberGet, 0, 2, 4),
        op(Opcode::Return, 0, 0, 0),
    ]);
    let f = &mut p.functions[0];
    f.source_map.clear();
    let mut vm = load(p);
    let task = vm
        .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
        .unwrap();
    let text = returned(&mut vm, task);
    let mut byte = [0];
    vm.heap().read_bytes(text, 0, &mut byte).unwrap();
    assert_eq!(byte, [b'x']);
}

#[test]
fn cancellation_during_sequence_build_never_publishes_partial_result() {
    let mut vm = load(sequence(Opcode::MakeArray, 513));
    let task = vm
        .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
        .unwrap();
    for _ in 0..2000 {
        if matches!(vm.active.frames.data.last(), Some(Frame::Script(s)) if matches!(s.phase, ScriptPhase::Sequence { .. }))
        {
            break;
        }
        tick(&mut vm, task);
    }
    vm.cancel_task(task).unwrap();
    let mut idle = false;
    for _ in 0..1000 {
        match vm.run_slice(task, SliceBudget::default(), || 0) {
            Err(VmError::Idle) => {
                idle = true;
                break;
            }
            Ok(RunOutcome::BudgetYield(_)) => gc(&mut vm),
            other => panic!("{other:?}"),
        }
    }
    assert!(idle);
    assert_eq!(vm.stats().frames, 0);
}

fn spread_rest_package() -> BytecodePackage {
    let mut p = package(
        vec![Const::Int(10), Const::Int(20)],
        vec![
            op(Opcode::LoadConst, 0, 0, 0),
            op(Opcode::LoadConst, 1, 1, 0),
            op(Opcode::MakeArray, 2, 0, 0),
            op(Opcode::ArgsNew, 3, 0, 0),
            op(Opcode::ArgsSpread, 3, 2, 0),
            op(Opcode::Call, 4, 1, 0),
            op(Opcode::Return, 4, 0, 0),
        ],
        vec![
            Descriptor::Sequence(RegWindow {
                start: Reg(0),
                len: 2,
            }),
            Descriptor::Call(CallDesc {
                contract: None,
                target: CallTarget::Function(FunctionId(1)),
                arguments: Arguments::Packed(Reg(3)),
                names: vec![],
            }),
        ],
        5,
    );
    let mut sig = p.signatures[0].clone();
    sig.rest = Some(Parameter {
        name: SymbolId(4),
        ty: TypeId(0),
        mutable: false,
        default: None,
    });
    p.signatures.push(sig);
    let mut callee = p.functions[0].clone();
    callee.name = SymbolId(2);
    callee.signature = SignatureId(1);
    callee.register_count = 1;
    callee.descriptors.clear();
    callee.source_map.clear();
    callee.code = vec![op(Opcode::Return, 0, 0, 0)];
    callee.bindings = vec![BindingDef {
        name: SymbolId(4),
        ty: TypeId(0),
        mutable: false,
        storage: BindingStorage::Value(Reg(0)),
        initialization: BindingInitialization::Parameter(0),
    }];
    p.functions.push(callee);
    p
}

#[test]
fn named_capture_and_long_name_binding_survive_gc_mutation_and_cancellation() {
    let name = "参数".repeat(150);
    for action in 0..4 {
        let mut p = spread_rest_package();
        p.symbols[4] = name.clone();
        p.constants = vec![Const::Utf8(name.clone()), Const::Int(42)];
        p.functions[0].code[2] = op(Opcode::MakeTable, 2, 0, 0);
        p.functions[0].descriptors[0] = Descriptor::Table(vec![[Reg(0), Reg(1)]]);
        let parameter = p.signatures[1].rest.take().unwrap();
        p.signatures[1].parameters.push(parameter);
        let mut vm = load(p);
        let task = vm
            .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
            .unwrap();
        loop {
            if matches!(vm.active.frames.data.last(), Some(Frame::Script(s))
                if s.pc == test_pc(&vm, s.code.function.0, 4) && matches!(s.phase, ScriptPhase::Body))
            {
                break;
            }
            assert!(matches!(tick(&mut vm, task), RunOutcome::BudgetYield(_)));
        }
        vm.run_slice(
            task,
            SliceBudget {
                instructions: 1,
                work: 1,
                ..SliceBudget::default()
            },
            || 0,
        )
        .unwrap();
        let Frame::Script(s) = *vm.active.frames.data.last().unwrap() else {
            panic!()
        };
        assert!(matches!(s.phase, ScriptPhase::Spread(_)));
        let source = vm.value(s.env, 2).unwrap();
        gc(&mut vm);
        match action {
            1 => {
                let (key, _) = vm.heap().table_entry(source, 0).unwrap();
                vm.heap_mut()
                    .table_store(source, 0, key, Value::int(99))
                    .unwrap();
            }
            2 => vm.cancel_cooperatively(task).unwrap(),
            _ => {
                // A small work grant exposes the binder while a multibyte name
                // is incomplete. Matching does not allocate a copied name.
                let mut budget = SliceBudget {
                    instructions: 1,
                    work: 1,
                    allocation_bytes: 0,
                    deadline_ns: None,
                };
                loop {
                    let out = vm.run_slice(task, budget, || 0).unwrap();
                    gc(&mut vm);
                    let RunOutcome::BudgetYield(usage) = out else {
                        panic!("{out:?}")
                    };
                    budget.work = usage.required_work.max(1);
                    budget.allocation_bytes = usage.required_allocation;
                    if matches!(vm.active.frames.data.last(), Some(Frame::Script(s))
                        if s.code.function == FunctionId(1) && matches!(s.phase, ScriptPhase::Bind(_)))
                    {
                        break;
                    }
                }
                let before = vm.heap().stats().chunk_allocations;
                let out = vm
                    .run_slice(
                        task,
                        SliceBudget {
                            instructions: 1,
                            work: 65,
                            allocation_bytes: 0,
                            deadline_ns: None,
                        },
                        || 0,
                    )
                    .unwrap();
                assert!(matches!(out, RunOutcome::BudgetYield(_)));
                assert_eq!(vm.heap().stats().chunk_allocations, before);
                gc(&mut vm);
                if action == 3 {
                    vm.cancel_cooperatively(task).unwrap();
                }
            }
        }
        match terminal(&mut vm, task) {
            RunOutcome::Returned(id) if action == 0 => {
                assert_eq!(vm.delivery_value(id).unwrap().as_int(), Some(42));
                vm.discard_delivery(id).unwrap();
            }
            RunOutcome::Fault(id) if action != 0 => {
                let info = vm.delivery_fault(id).unwrap().unwrap();
                assert_eq!(
                    info.kind,
                    if action == 1 {
                        FaultKind::ConcurrentModification
                    } else {
                        FaultKind::Cancelled
                    }
                );
                assert_eq!(info.location.code.function, FunctionId(0));
                assert_eq!(
                    info.location.pc,
                    test_pc(
                        &vm,
                        info.location.code.function.0,
                        if action == 3 { 5 } else { 4 }
                    )
                );
                vm.discard_delivery(id).unwrap();
            }
            other => panic!("action {action}: {other:?}"),
        }
        assert_eq!(vm.stats().frames, 0);
        assert!(vm.is_idle());
    }
}

#[test]
fn unhashable_key_reports_a_rooted_tuple_element_path() {
    let mut p = table_package(
        vec![
            Const::FloatBits(f64::NAN.to_bits()),
            Const::Tuple(vec![ConstId(0)]),
        ],
        vec![Const::Int(1), Const::Int(2)],
    );
    p.functions[0].descriptors[0] = Descriptor::Table(vec![[Reg(1), Reg(3)]]);
    let mut vm = load(p);
    let task = vm
        .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
        .unwrap();
    let RunOutcome::Fault(delivery) = terminal(&mut vm, task) else {
        panic!()
    };
    assert_eq!(
        vm.delivery_fault(delivery).unwrap().unwrap().kind,
        FaultKind::UnhashableKey
    );
    let error = vm.delivery_value(delivery).unwrap();
    let details = vm.heap().core_error_info(error).unwrap().details;
    assert_eq!(vm.heap().info(details).unwrap().kind, ObjectKind::Tuple);
    assert_eq!(vm.heap().sequence_len(details).unwrap(), 1);
    assert_eq!(
        vm.heap().sequence_get(details, 0).unwrap().as_int(),
        Some(0)
    );
}

#[test]
fn host_start_and_empty_script_rest_share_the_binder_contract() {
    let mut vm = load(spread_rest_package());
    let task = vm
        .start(
            FunctionId(1),
            &[Value::int(42)],
            RootOwner(9),
            Context::Ordinary,
        )
        .unwrap();
    let array = returned(&mut vm, task);
    assert_eq!(vm.heap().sequence_len(array).unwrap(), 1);
    assert_eq!(vm.heap().sequence_get(array, 0).unwrap().as_int(), Some(42));
    let task = vm
        .start(FunctionId(1), &[], RootOwner(10), Context::Ordinary)
        .unwrap();
    let empty = returned(&mut vm, task);
    assert_eq!(vm.heap().sequence_len(empty).unwrap(), 0);
    assert!(!array.same_bits(empty));
}

#[test]
fn optional_null_branch_skips_missing_member_but_nonnull_missing_faults() {
    let mut p = package(
        vec![Const::Null],
        vec![
            op(Opcode::LoadConst, 0, 0, 0),
            op(Opcode::BranchNull, 0, 3, 2),
            op(Opcode::MemberGet, 0, 0, 4),
            op(Opcode::Return, 0, 0, 0),
        ],
        vec![],
        1,
    );
    let mut vm = load(p.clone());
    let task = vm
        .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
        .unwrap();
    assert_eq!(returned(&mut vm, task).tag(), ValueTag::Null);
    p.functions[0].code[0] = op(Opcode::MakeTable, 0, 0, 0);
    p.functions[0].descriptors = vec![Descriptor::Table(vec![])];
    let mut vm = load(p);
    let task = vm
        .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
        .unwrap();
    fault(&mut vm, task, FaultKind::MissingKey);
}

#[test]
fn table_overwrite_keeps_structure_version_and_traces_new_value_during_mark() {
    let mut vm = load(table_package(
        vec![Const::Utf8("x".into())],
        vec![Const::Null],
    ));
    let task = vm
        .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
        .unwrap();
    let table = returned(&mut vm, task);
    let before = vm.heap().info(table).unwrap();
    let child = vm.heap_mut().alloc_array(&[Value::int(73)]).unwrap();
    vm.heap_mut()
        .table_store(table, 0, Value::NULL, child)
        .unwrap();
    let after = vm.heap().info(table).unwrap();
    assert_eq!(before.structure_version, after.structure_version);
    assert_ne!(before.content_version, after.content_version);
    // Exact GC entry is provider-aware; public generic writes cannot bypass the
    // language-table key schema or publish a half key/value pair.
    assert_eq!(
        vm.heap_mut().set_field(table, 0, Value::NULL),
        Err(HeapError::WrongKind)
    );
    for _ in 0..10000 {
        gc(&mut vm);
        if vm.heap().phase() == GcPhase::Idle {
            break;
        }
    }
    let (_, retained) = vm.heap().table_entry(table, 0).unwrap();
    assert!(retained.same_bits(child));
    assert_eq!(
        vm.heap().sequence_get(retained, 0).unwrap().as_int(),
        Some(73)
    );
}

#[test]
fn tuple_constants_larger_than_a_mutation_are_constructed_in_budgeted_steps() {
    let p = package(
        vec![Const::Int(1), Const::Tuple(vec![ConstId(0); 257])],
        vec![op(Opcode::LoadConst, 0, 1, 0), op(Opcode::Return, 0, 0, 0)],
        vec![],
        1,
    );
    let mut vm = load(p);
    assert_eq!(vm.heap().stats().live_objects, 0);
    let task = vm
        .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
        .unwrap();
    let value = returned(&mut vm, task);
    assert_eq!(vm.heap().info(value).unwrap().kind, ObjectKind::Tuple);
    assert_eq!(vm.heap().info(value).unwrap().len, 257);
    assert_eq!(
        vm.heap().sequence_get(value, 256).unwrap().as_int(),
        Some(1)
    );
    assert_eq!(vm.limits.heap.max_mutation_values, 256);
}

#[test]
fn uninitialized_table_operands_enter_catch_and_finally_instead_of_escaping_slice() {
    // The verifier bounds register indices but intentionally does not prove
    // definite assignment. Exercise the deferred reads after a successful
    // first field and after key validation/lookup, including both setter forms.
    for case in 0..4 {
        let mut code = vec![
            op(Opcode::LoadConst, 1, 0, 0),
            op(Opcode::LoadConst, 2, 1, 0),
            op(Opcode::LoadConst, 3, 2, 0),
            op(Opcode::MakeTable, 6, 0, 0),
        ];
        let fields = match case {
            0 => vec![[Reg(3), Reg(2)], [Reg(4), Reg(2)]],
            1 => vec![[Reg(3), Reg(4)]],
            _ => vec![],
        };
        match case {
            2 => code.push(op(Opcode::IndexSet, 6, 3, 4)),
            3 => code.push(op(Opcode::MemberSet, 6, 4, 4)),
            _ => {}
        }
        code.push(op(Opcode::Return, 6, 0, 0));
        let catch_start = code.len() as u32;
        code.push(op(Opcode::Return, 5, 0, 0));
        let finally_start = code.len() as u32;
        code.push(op(Opcode::IndexSet, 0, 1, 2));
        code.push(op(Opcode::EndFinally, 0, 0, 0));
        let finally_end = code.len() as u32;
        code.push(op(Opcode::Return, 5, 0, 0));
        let mut p = package(
            vec![Const::Int(0), Const::Int(1), Const::Utf8("x".into())],
            code,
            vec![Descriptor::Table(fields)],
            7,
        );
        p.signatures[0].parameters.push(Parameter {
            name: SymbolId(4),
            ty: TypeId(0),
            mutable: false,
            default: None,
        });
        p.functions[0].regions = vec![ExceptionRegion {
            protected: PcRange {
                start: Pc(0),
                end: Pc(catch_start),
            },
            catch: Some(CatchRegion {
                range: PcRange {
                    start: Pc(catch_start),
                    end: Pc(finally_start),
                },
                exception: Reg(5),
            }),
            finally: Some(PcRange {
                start: Pc(finally_start),
                end: Pc(finally_end),
            }),
        }];
        let mut vm = load(p);
        let marker = vm.heap_mut().alloc_array(&[Value::int(0)]).unwrap();
        let root = vm
            .heap_mut()
            .add_root(RootOwner(9), RootKind::Host, marker)
            .unwrap();
        let task = vm
            .start(FunctionId(0), &[marker], RootOwner(9), Context::Ordinary)
            .unwrap();
        // The driver interleaves bounded slices with provider-aware GC. Its
        // unwrap also asserts these author errors never return VmError::Heap.
        let error = returned(&mut vm, task);
        assert_eq!(
            vm.heap().core_error_info(error).unwrap().code.kind,
            FaultKind::Uninitialized,
            "case {case}"
        );
        assert_eq!(
            vm.heap().sequence_get(marker, 0).unwrap().as_int(),
            Some(1),
            "finally case {case}"
        );
        assert!(vm.active.equality_cursors.data.is_empty());
        assert_eq!(vm.stats().frames, 0);
        vm.heap_mut().release_root(root).unwrap();
    }
}

fn packed_destination_package(len: u32, opcode: Opcode) -> BytecodePackage {
    let mut code = (0..len)
        .map(|reg| op(Opcode::LoadConst, reg, 0, 0))
        .collect::<Vec<_>>();
    code.extend([
        op(Opcode::MakeArray, len, 0, 0),
        op(Opcode::MakeArray, len + 1, 1, 0),
        op(
            opcode,
            len,
            len + 1,
            if opcode == Opcode::ArgsNamed { 4 } else { 0 },
        ),
        op(Opcode::Return, len, 0, 0),
    ]);
    package(
        vec![Const::Int(9)],
        code,
        vec![
            Descriptor::Sequence(RegWindow { start: Reg(0), len }),
            Descriptor::Sequence(RegWindow {
                start: Reg(0),
                len: 1,
            }),
        ],
        len + 2,
    )
}
fn reach_packed_append(vm: &mut Vm, task: TaskId, len: u32) -> Value {
    for _ in 0..10000 {
        if let Some(Frame::Script(s)) = vm.active.frames.data.last().copied()
            && s.pc == test_pc(vm, s.code.function.0, len + 2)
            && matches!(s.phase, ScriptPhase::Body)
        {
            return vm.value(s.regs, len).unwrap();
        }
        assert!(matches!(tick(vm, task), RunOutcome::BudgetYield(_)));
    }
    panic!("did not reach packed append");
}

fn collection_copy(table: bool) -> BytecodePackage {
    let mut p = package(
        vec![],
        vec![
            op(
                if table {
                    Opcode::MakeTable
                } else {
                    Opcode::MakeArray
                },
                1,
                0,
                0,
            ),
            op(
                if table {
                    Opcode::TableSpread
                } else {
                    Opcode::ArraySpread
                },
                1,
                0,
                0,
            ),
            op(Opcode::Return, 1, 0, 0),
        ],
        vec![if table {
            Descriptor::Table(vec![])
        } else {
            Descriptor::Sequence(RegWindow {
                start: Reg(1),
                len: 0,
            })
        }],
        2,
    );
    p.signatures[0].parameters.push(Parameter {
        name: SymbolId(4),
        ty: TypeId(0),
        mutable: false,
        default: None,
    });
    p
}

fn copy_source(vm: &mut Vm, table: bool, len: u32) -> Value {
    let value = if table {
        vm.heap_mut().alloc_table().unwrap()
    } else {
        vm.heap_mut().alloc_array(&[]).unwrap()
    };
    for index in 0..len {
        if table {
            vm.heap_mut()
                .table_store(
                    value,
                    index,
                    Value::int(index as i64),
                    Value::int(index as i64),
                )
                .unwrap();
        } else {
            vm.heap_mut()
                .array_push(value, Value::int(index as i64))
                .unwrap();
        }
    }
    value
}

fn method_package(method: &str) -> BytecodePackage {
    let mut p = package(
        vec![],
        vec![
            op(Opcode::MemberGet, 1, 0, 0),
            op(Opcode::Call, 2, 0, 0),
            op(Opcode::Return, 2, 0, 0),
        ],
        vec![Descriptor::Call(CallDesc {
            contract: None,
            target: CallTarget::Register(Reg(1)),
            arguments: Arguments::Window(RegWindow {
                start: Reg(0),
                len: 0,
            }),
            names: vec![],
        })],
        3,
    );
    let name = SymbolId(p.symbols.len() as u32);
    p.symbols.push(method.into());
    p.functions[0].code[0].operands[2] = name.0;
    p.signatures[0].parameters.push(Parameter {
        name: SymbolId(4),
        ty: TypeId(0),
        mutable: false,
        default: None,
    });
    p
}

fn point_lookup_package(indexed: bool, write: bool) -> BytecodePackage {
    let mut p = method_package("position");
    if write {
        let symbol = p.functions[0].code[0].operands[2];
        p.constants.push(Const::Int(126));
        p.functions[0].code = if indexed {
            p.constants.push(Const::Utf8("position".into()));
            vec![
                op(Opcode::LoadConst, 1, 1, 0),
                op(Opcode::LoadConst, 2, 0, 0),
                op(Opcode::IndexSet, 0, 1, 2),
                op(Opcode::Return, 2, 0, 0),
            ]
        } else {
            vec![
                op(Opcode::LoadConst, 1, 0, 0),
                op(Opcode::MemberSet, 0, 1, symbol),
                op(Opcode::Return, 1, 0, 0),
            ]
        };
        p.functions[0].source_map.clear();
    } else if indexed {
        p.constants.push(Const::Utf8("position".into()));
        p.functions[0].code = vec![
            op(Opcode::LoadConst, 1, 0, 0),
            op(Opcode::IndexGet, 2, 0, 1),
            op(Opcode::Return, 2, 0, 0),
        ];
    } else {
        p.functions[0].code = vec![p.functions[0].code[0], op(Opcode::Return, 1, 0, 0)];
        p.functions[0].source_map.truncate(2);
    }
    p
}

fn pause_point_lookup(vm: &mut Vm, table: Value, context: Context) -> TaskId {
    let task = vm
        .start(FunctionId(0), &[table], RootOwner(9), context)
        .unwrap();
    for attempt in 0..1000 {
        assert!(attempt < 999, "lookup did not reach its bounded scan");
        let outcome = vm
            .run_slice(
                task,
                SliceBudget {
                    instructions: 1,
                    work: 1.max(vm.last_usage.required_work),
                    allocation_bytes: VALUE_CHUNK_BYTES.max(vm.last_usage.required_allocation),
                    ..SliceBudget::default()
                },
                || 0,
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::BudgetYield(_)));
        if matches!(vm.active.frames.data.last(), Some(Frame::Script(s)) if s.table_work.is_some())
        {
            break;
        }
    }
    task
}

fn warm_point_lookup(vm: &mut Vm, table: Value) -> Value {
    let root = vm
        .heap_mut()
        .add_root(RootOwner(8), RootKind::Host, table)
        .unwrap();
    let task = vm
        .start(FunctionId(0), &[table], RootOwner(9), Context::Ordinary)
        .unwrap();
    let RunOutcome::Returned(delivery) = terminal(vm, task) else {
        panic!("warm lookup failed")
    };
    let value = vm.delivery_value(delivery).unwrap();
    vm.discard_delivery(delivery).unwrap();
    vm.heap_mut().release_root(root).unwrap();
    value
}

fn pause_matched_lookup(vm: &mut Vm, task: TaskId) {
    for _ in 0..1000 {
        if matches!(vm.active.frames.data.last(),Some(Frame::Script(s)) if s.table_work.is_some_and(|work|work.matched()))
        {
            return;
        }
        let outcome = vm
            .run_slice(
                task,
                SliceBudget {
                    instructions: 1,
                    work: vm.last_usage.required_work.max(1),
                    allocation_bytes: VALUE_CHUNK_BYTES.max(vm.last_usage.required_allocation),
                    ..Default::default()
                },
                || 0,
            )
            .unwrap();
        gc(vm);
        assert!(matches!(outcome, RunOutcome::BudgetYield(_)), "{outcome:?}");
    }
    panic!("lookup never matched");
}

fn check_point_lookup_value_and_structure_changes(restore: bool) {
    use crate::snapshot::{SnapshotBudget, SnapshotError, SnapshotRegistry, VmSnapshot};
    for model in [false, true] {
        for (indexed, write) in [(false, false), (true, false), (false, true), (true, true)] {
            for mutation in 0..4 {
                let mut vm = load(point_lookup_package(indexed, write));
                let mut table = vm.heap_mut().alloc_table().unwrap();
                let mut position = vm.heap_mut().alloc_string("position").unwrap();
                let mut repeat = vm.heap_mut().alloc_string("repeat").unwrap();
                vm.heap_mut()
                    .table_store(table, 0, position, Value::int(41))
                    .unwrap();
                vm.heap_mut()
                    .table_store(table, 1, repeat, Value::bool(false))
                    .unwrap();
                if model {
                    vm.heap_mut().observe_model_object(table).unwrap();
                }
                if !indexed {
                    warm_point_lookup(&mut vm, table);
                }
                // Diverge content and structure versions before capture. Equal
                // counters would hide restoring the wrong version semantics.
                vm.heap_mut()
                    .table_store(table, 0, position, Value::int(42))
                    .unwrap();
                assert_ne!(
                    vm.heap().info(table).unwrap().content_version,
                    vm.heap().info(table).unwrap().structure_version
                );
                let mut task = pause_point_lookup(&mut vm, table, Context::Ordinary);
                if !indexed {
                    pause_matched_lookup(&mut vm, task);
                    assert!(vm.work_probe.member_hint_hits.get() > 0);
                }
                if restore {
                    let registry = SnapshotRegistry::default();
                    let mut capture = vm.snapshot_begin(&registry).unwrap();
                    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
                    let mut image = capture.finish().unwrap();
                    if !model && !indexed && !write && mutation == 0 {
                        image.version = 2;
                        assert!(matches!(
                            VmSnapshot::decode(&image.encode().unwrap()),
                            Err(SnapshotError::Version {
                                format: "VM",
                                actual: 2,
                                expected: 1,
                            })
                        ));
                        assert!(matches!(
                            Vm::restore_begin(
                                &image,
                                vm.program().clone(),
                                RuntimeSupport {
                                    host_types: Vec::new(),
                                    opcodes: vec![],
                                    capabilities: vec![],
                                    natives: vec![],
                                },
                                VmLimits::default(),
                                SessionEpoch(82),
                                &registry,
                            ),
                            Err(SnapshotError::Version {
                                format: "VM",
                                actual: 2,
                                expected: 1,
                            })
                        ));
                        image.version = VmSnapshot::FORMAT_VERSION;
                    }
                    let image = VmSnapshot::decode(&image.encode().unwrap()).unwrap();
                    let mut restore = Vm::restore_begin(
                        &image,
                        vm.program().clone(),
                        RuntimeSupport {
                            host_types: Vec::new(),
                            opcodes: vec![],
                            capabilities: vec![],
                            natives: vec![],
                        },
                        VmLimits::default(),
                        SessionEpoch(82),
                        &registry,
                    )
                    .unwrap();
                    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
                    let (restored, map) = restore.finish().unwrap();
                    table = map.value(table).unwrap();
                    position = map.value(position).unwrap();
                    repeat = map.value(repeat).unwrap();
                    task = map.task(task).unwrap();
                    vm = restored;
                }
                match mutation {
                    0 => vm
                        .heap_mut()
                        .table_store(table, 0, position, Value::int(84))
                        .unwrap(),
                    1 => vm
                        .heap_mut()
                        .table_store(table, 1, repeat, Value::bool(true))
                        .unwrap(),
                    2 => vm
                        .heap_mut()
                        .table_store(table, 2, Value::int(99), Value::NULL)
                        .unwrap(),
                    _ => vm.heap_mut().clear_collection(table).unwrap(),
                }
                if mutation < 2 {
                    let expected = if write {
                        126
                    } else if mutation == 0 {
                        84
                    } else {
                        42
                    };
                    assert_eq!(returned(&mut vm, task).as_int(), Some(expected));
                    assert_eq!(
                        vm.heap().table_entry(table, 0).unwrap().1.as_int(),
                        Some(expected)
                    );
                } else {
                    fault(&mut vm, task, FaultKind::ConcurrentModification);
                }
            }
        }
    }
}

fn text_method_package(method: &str) -> BytecodePackage {
    let mut p = method_package(method);
    let f = &mut p.functions[0];
    f.register_count = 4;
    f.code[0].operands[0] = 2;
    f.code[1].operands[0] = 3;
    f.code[2].operands[0] = 3;
    let Descriptor::Call(call) = &mut f.descriptors[0] else {
        panic!()
    };
    call.target = CallTarget::Register(Reg(2));
    call.arguments = Arguments::Window(RegWindow {
        start: Reg(1),
        len: 1,
    });
    let name = SymbolId(p.symbols.len() as u32);
    p.symbols.push("argument".into());
    p.signatures[0].parameters.push(Parameter {
        name,
        ty: TypeId(0),
        mutable: false,
        default: None,
    });
    p
}

mod lookup;
mod mutation;
mod text;
