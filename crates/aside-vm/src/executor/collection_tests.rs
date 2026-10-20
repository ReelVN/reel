//! Execution regressions for bounded collections, spread/rest and their GC roots.
use super::*;

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
fn host_own_string_lookup_is_bounded_and_does_not_confuse_null_with_missing() {
    let mut vm = load(table_package(
        vec![Const::Utf8("ab".into()), Const::Utf8("ac".into())],
        vec![Const::Int(1), Const::Null],
    ));
    let task = vm
        .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
        .unwrap();
    let table = returned(&mut vm, task);
    for query in ["ac", "missing"] {
        let mut cursor = vm.heap().table_own_string_lookup(table, query).unwrap();
        let zero = vm.heap().table_own_string_step(&mut cursor, 0).unwrap();
        assert_eq!(zero.work, 0);
        assert!(matches!(zero.status, TableLookupStatus::Pending));
        let mut complete = false;
        for _ in 0..30 {
            let step = vm.heap().table_own_string_step(&mut cursor, 1).unwrap();
            assert!(step.work <= 1);
            if let TableLookupStatus::Complete(value) = step.status {
                assert_eq!(
                    value.map(Value::tag),
                    if query == "ac" {
                        Some(ValueTag::Null)
                    } else {
                        None
                    }
                );
                complete = true;
                break;
            }
            gc(&mut vm);
        }
        assert!(complete);
    }
    let mut cursor = vm.heap().table_own_string_lookup(table, "ab").unwrap();
    vm.heap_mut()
        .table_store(table, 0, Value::NULL, Value::int(9))
        .unwrap();
    assert_eq!(
        vm.heap().table_own_string_step(&mut cursor, 1).unwrap_err(),
        HeapError::ConcurrentModification
    );
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
fn packed_spread_and_script_rest_capture_fresh_outer_arrays() {
    let mut vm = load(spread_rest_package());
    let first = vm
        .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
        .unwrap();
    let a = returned(&mut vm, first);
    let second = vm
        .start(FunctionId(0), &[], RootOwner(10), Context::Ordinary)
        .unwrap();
    let b = returned(&mut vm, second);
    assert!(!a.same_bits(b));
    for array in [a, b] {
        assert_eq!(vm.heap().sequence_len(array).unwrap(), 2);
        assert_eq!(vm.heap().sequence_get(array, 0).unwrap().as_int(), Some(10));
        assert_eq!(vm.heap().sequence_get(array, 1).unwrap().as_int(), Some(20));
    }
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
                if s.pc == Pc(4) && matches!(s.phase, ScriptPhase::Body))
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
                assert_eq!(info.location.pc, Pc(if action == 3 { 5 } else { 4 }));
                vm.discard_delivery(id).unwrap();
            }
            other => panic!("action {action}: {other:?}"),
        }
        assert_eq!(vm.stats().frames, 0);
        assert!(vm.is_idle());
    }
}
#[test]
fn spread_detects_overwrite_between_capture_steps() {
    let mut vm = load(spread_rest_package());
    let task = vm
        .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
        .unwrap();
    let mut source = None;
    for _ in 0..1000 {
        if let Some(Frame::Script(s)) = vm.active.frames.data.last().copied()
            && matches!(s.phase, ScriptPhase::Body)
            && s.pc == Pc(4)
        {
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
            assert!(matches!(s.phase, ScriptPhase::Spread { .. }));
            source = Some(vm.value(s.env, 2).unwrap());
            break;
        }
        // Exactly one instruction admission ends at the spread phase.
        tick(&mut vm, task);
    }
    let source = source.expect("spread pause");
    vm.heap_mut().set_field(source, 1, Value::int(999)).unwrap();
    fault(&mut vm, task, FaultKind::ConcurrentModification);
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
fn table_lookup_version_change_and_cooperative_cancel_release_key_stack() {
    for cancel in [false, true] {
        let mut p = table_package(vec![Const::Utf8("x".repeat(256))], vec![Const::Int(7)]);
        p.functions[0].code.pop();
        p.functions[0]
            .code
            .extend([op(Opcode::IndexGet, 1, 2, 0), op(Opcode::Return, 1, 0, 0)]);
        p.functions[0].source_map.clear();
        let mut vm = load(p);
        let task = vm
            .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
            .unwrap();
        let mut target = None;
        for _ in 0..1000 {
            if let Some(Frame::Script(s)) = vm.active.frames.data.last().copied()
                && s.table_work.is_some()
                && matches!(s.phase, ScriptPhase::Equality { .. })
            {
                target = Some(vm.value(s.env, 1).unwrap());
                break;
            }
            tick(&mut vm, task);
        }
        let table = target.expect("lookup must yield in its long string comparison");
        if cancel {
            vm.cancel_cooperatively(task).unwrap();
        } else {
            vm.heap_mut()
                .table_store(table, 0, Value::NULL, Value::int(8))
                .unwrap();
        }
        fault(
            &mut vm,
            task,
            if cancel {
                FaultKind::Cancelled
            } else {
                FaultKind::ConcurrentModification
            },
        );
        assert!(vm.active.equality_cursors.data.is_empty());
    }
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
            && s.pc == Pc(len + 2)
            && matches!(s.phase, ScriptPhase::Body)
        {
            return vm.value(s.regs, len).unwrap();
        }
        assert!(matches!(tick(vm, task), RunOutcome::BudgetYield(_)));
    }
    panic!("did not reach packed append");
}
#[test]
fn forged_odd_packed_destinations_fail_before_any_half_pair_or_allocation() {
    for opcode in [Opcode::ArgsPush, Opcode::ArgsNamed, Opcode::ArgsSpread] {
        let mut vm = load(packed_destination_package(63, opcode));
        let task = vm
            .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
            .unwrap();
        let packed = reach_packed_append(&mut vm, task, 63);
        let before = vm.heap().info(packed).unwrap();
        let chunks = vm.heap().stats().chunk_allocations;
        let RunOutcome::BudgetYield(usage) = vm
            .run_slice(
                task,
                SliceBudget {
                    instructions: 1,
                    work: 2,
                    allocation_bytes: 0,
                    ..SliceBudget::default()
                },
                || 0,
            )
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(usage.allocation_bytes, 0);
        assert_eq!(vm.heap().stats().chunk_allocations, chunks);
        let after = vm.heap().info(packed).unwrap();
        assert_eq!(after.len, 63);
        assert_eq!(after.content_version, before.content_version);
        fault(&mut vm, task, FaultKind::Type);
    }
}
#[test]
fn packed_pair_allocation_is_admitted_before_either_element_is_published() {
    for len in [crate::payload::INLINE_VALUES as u32, 64] {
        for opcode in [Opcode::ArgsPush, Opcode::ArgsNamed, Opcode::ArgsSpread] {
            let mut vm = load(packed_destination_package(len, opcode));
            let task = vm
                .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
                .unwrap();
            let packed = reach_packed_append(&mut vm, task, len);
            let version = vm.heap().info(packed).unwrap().content_version;
            for _ in 0..3 {
                let RunOutcome::BudgetYield(usage) = vm
                    .run_slice(
                        task,
                        SliceBudget {
                            instructions: 1,
                            work: 3,
                            allocation_bytes: VALUE_CHUNK_BYTES - 1,
                            ..SliceBudget::default()
                        },
                        || 0,
                    )
                    .unwrap()
                else {
                    panic!()
                };
                assert_eq!(usage.allocation_bytes, 0);
                assert_eq!(usage.required_allocation, VALUE_CHUNK_BYTES);
                let info = vm.heap().info(packed).unwrap();
                assert_eq!(info.len, len);
                assert_eq!(info.content_version, version);
                gc(&mut vm);
            }
            let RunOutcome::BudgetYield(usage) = vm
                .run_slice(
                    task,
                    SliceBudget {
                        instructions: 1,
                        work: 2,
                        allocation_bytes: VALUE_CHUNK_BYTES,
                        ..SliceBudget::default()
                    },
                    || 0,
                )
                .unwrap()
            else {
                panic!()
            };
            assert_eq!(usage.allocation_bytes, VALUE_CHUNK_BYTES);
            assert_eq!(vm.heap().info(packed).unwrap().len, len + 2);
            assert_eq!(
                vm.heap().sequence_get(packed, len).unwrap().as_int(),
                Some(if opcode == Opcode::ArgsNamed { 4 } else { -1 })
            );
            assert!(vm.heap().sequence_get(packed, len + 1).is_ok());
        }
    }
}
#[test]
fn spread_destination_overwrite_and_odd_append_invalidate_resume() {
    for append in [false, true] {
        let mut vm = load(spread_rest_package());
        let task = vm
            .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
            .unwrap();
        let mut packed = None;
        for _ in 0..1000 {
            let Some(Frame::Script(s)) = vm.active.frames.data.last().copied() else {
                panic!()
            };
            if s.pc == Pc(4) && matches!(s.phase, ScriptPhase::Body) {
                vm.run_slice(
                    task,
                    SliceBudget {
                        instructions: 1,
                        work: 3,
                        ..SliceBudget::default()
                    },
                    || 0,
                )
                .unwrap();
                let Frame::Script(s) = *vm.active.frames.data.last().unwrap() else {
                    panic!()
                };
                assert!(matches!(
                    s.phase,
                    ScriptPhase::Spread(collections::SpreadCapture { index: 1, .. })
                ));
                packed = Some(vm.value(s.env, 1).unwrap());
                break;
            }
            tick(&mut vm, task);
        }
        let packed = packed.expect("spread capture pause");
        if append {
            vm.heap_mut().array_push(packed, Value::int(99)).unwrap();
        } else {
            vm.heap_mut().set_field(packed, 1, Value::int(99)).unwrap();
        }
        let root = vm
            .heap_mut()
            .add_root(RootOwner(9), RootKind::Host, packed)
            .unwrap();
        let before = vm.heap().info(packed).unwrap();
        fault(&mut vm, task, FaultKind::ConcurrentModification);
        let after = vm.heap().info(packed).unwrap();
        assert_eq!(after.len, before.len);
        assert_eq!(after.content_version, before.content_version);
        vm.heap_mut().release_root(root).unwrap();
    }
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

#[test]
fn table_remove_cancel_and_concurrent_write_preserve_the_original_until_commit() {
    use crate::snapshot::{SnapshotBudget, SnapshotRegistry, VmSnapshot};
    for observed in [false, true] {
        for action in 0..3 {
            let mut p = method_package("remove");
            let key_name = SymbolId(p.symbols.len() as u32);
            p.symbols.push("key".into());
            p.signatures[0].parameters.push(Parameter {
                name: key_name,
                ty: TypeId(0),
                mutable: false,
                default: None,
            });
            p.functions[0].register_count = 4;
            p.functions[0].code[0].operands[0] = 2;
            p.functions[0].code[1].operands[0] = 3;
            p.functions[0].code[2].operands[0] = 3;
            let Descriptor::Call(call) = &mut p.functions[0].descriptors[0] else {
                unreachable!()
            };
            call.target = CallTarget::Register(Reg(2));
            call.arguments = Arguments::Window(RegWindow {
                start: Reg(1),
                len: 1,
            });
            let mut vm = load(p);
            let mut table = vm.heap_mut().alloc_table().unwrap();
            for n in 0..130 {
                let child = vm.heap_mut().alloc_array(&[Value::int(n)]).unwrap();
                vm.heap_mut()
                    .table_store(table, n as u32, Value::int(n), child)
                    .unwrap();
            }
            if observed {
                vm.heap_mut().observe_model_object(table).unwrap();
            }
            let mut root = vm
                .heap_mut()
                .add_root(RootOwner(8), RootKind::Host, table)
                .unwrap();
            let mut task = vm
                .start(
                    FunctionId(0),
                    &[table, Value::int(64)],
                    RootOwner(9),
                    Context::Ordinary,
                )
                .unwrap();
            loop {
                let result = tick(&mut vm, task);
                assert!(matches!(result, RunOutcome::BudgetYield(_)));
                let Frame::Script(s) = *vm.active.frames.data.last().unwrap() else {
                    panic!()
                };
                if matches!(s.phase, ScriptPhase::Bulk(_))
                    && vm.heap().info(vm.value(s.env, 2).unwrap()).unwrap().len >= 65
                {
                    assert_eq!(vm.heap().table_len(table).unwrap(), 130);
                    break;
                }
            }
            if action == 0 {
                let registry = SnapshotRegistry::default();
                let mut capture = vm.snapshot_begin(&registry).unwrap();
                while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
                let image = capture.finish().unwrap();
                let image = VmSnapshot::decode(&image.encode().unwrap()).unwrap();
                let mut restore = Vm::restore_begin(
                    &image,
                    vm.program().clone(),
                    RuntimeSupport {
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
                root = map.root(root).unwrap();
                task = map.task(task).unwrap();
                vm = restored;
            }
            match action {
                0 => {
                    let RunOutcome::Returned(id) = terminal(&mut vm, task) else {
                        panic!()
                    };
                    let removed = vm.delivery_value(id).unwrap();
                    assert_eq!(
                        vm.heap().sequence_get(removed, 0).unwrap().as_int(),
                        Some(64)
                    );
                    vm.discard_delivery(id).unwrap();
                }
                1 => {
                    vm.heap_mut()
                        .table_store(table, 0, Value::int(0), Value::NULL)
                        .unwrap();
                    fault(&mut vm, task, FaultKind::ConcurrentModification);
                }
                _ => {
                    vm.cancel(RootOwner(9)).unwrap();
                    while let Ok(outcome) = vm.run_slice(task, SliceBudget::default(), || 0) {
                        assert!(matches!(outcome, RunOutcome::BudgetYield(_)));
                    }
                }
            }
            let len = if action == 0 { 129 } else { 130 };
            for n in 0..len {
                let source = n + u32::from(action == 0 && n >= 64);
                let (key, value) = vm.heap().table_entry(table, n).unwrap();
                assert_eq!(key.as_int(), Some(i64::from(source)));
                if action == 1 && n == 0 {
                    assert!(value.same_bits(Value::NULL));
                } else {
                    assert_eq!(
                        vm.heap().sequence_get(value, 0).unwrap().as_int(),
                        Some(i64::from(source))
                    );
                }
            }
            vm.heap_mut().release_root(root).unwrap();
        }
    }
}

#[test]
fn bulk_edit_commit_cancel_and_concurrent_write_preserve_identity_and_gc_edges() {
    for action in 0..3 {
        let mut vm = load(method_package("reverse"));
        let source = vm.heap_mut().alloc_array(&[]).unwrap();
        for n in 0..130 {
            let child = vm.heap_mut().alloc_array(&[Value::int(n)]).unwrap();
            vm.heap_mut().array_push(source, child).unwrap();
        }
        let root = vm
            .heap_mut()
            .add_root(RootOwner(8), RootKind::Host, source)
            .unwrap();
        let task = vm
            .start(FunctionId(0), &[source], RootOwner(9), Context::Ordinary)
            .unwrap();
        let mut admitted = 6;
        loop {
            let outcome = vm
                .run_slice(
                    task,
                    SliceBudget {
                        instructions: 1,
                        work: admitted,
                        allocation_bytes: VALUE_CHUNK_BYTES,
                        deadline_ns: None,
                    },
                    || 0,
                )
                .unwrap();
            let RunOutcome::BudgetYield(usage) = outcome else {
                panic!("{outcome:?}");
            };
            admitted = usage.required_work.max(6);
            let Frame::Script(s) = *vm.active.frames.data.last().unwrap() else {
                panic!()
            };
            if matches!(s.phase, ScriptPhase::Bulk(_))
                && vm.heap().info(vm.value(s.env, 2).unwrap()).unwrap().len >= 65
            {
                for n in 0..130 {
                    let child = vm.heap().sequence_get(source, n).unwrap();
                    assert_eq!(
                        vm.heap().sequence_get(child, 0).unwrap().as_int(),
                        Some(n as i64)
                    );
                }
                break;
            }
        }
        // Begin a new mark cycle after part of the private copy exists. A commit
        // must not move still-unscanned edges behind a completed object cursor.
        vm.begin_collection().unwrap();
        match action {
            0 => loop {
                match tick(&mut vm, task) {
                    RunOutcome::BudgetYield(_) => {}
                    RunOutcome::Returned(id) => {
                        assert!(vm.delivery_value(id).unwrap().same_bits(source));
                        vm.discard_delivery(id).unwrap();
                        break;
                    }
                    other => panic!("{other:?}"),
                }
            },
            1 => {
                vm.heap_mut().set_field(source, 0, Value::NULL).unwrap();
                fault(&mut vm, task, FaultKind::ConcurrentModification);
            }
            _ => {
                vm.cancel(RootOwner(9)).unwrap();
                while let Ok(outcome) = vm.run_slice(task, SliceBudget::default(), || 0) {
                    assert!(matches!(outcome, RunOutcome::BudgetYield(_)));
                }
            }
        }
        while vm.heap().phase() != GcPhase::Idle {
            gc(&mut vm);
        }
        vm.begin_collection().unwrap();
        while vm.heap().phase() != GcPhase::Idle {
            gc(&mut vm);
        }
        for n in 0..130 {
            let child = vm.heap().sequence_get(source, n).unwrap();
            if action == 1 && n == 0 {
                assert!(child.same_bits(Value::NULL));
            } else {
                assert_eq!(
                    vm.heap().sequence_get(child, 0).unwrap().as_int(),
                    Some(if action == 0 { 129 - n } else { n } as i64)
                );
            }
        }
        assert_eq!(vm.stats().frames, 0);
        vm.heap_mut().release_root(root).unwrap();
    }
}

#[test]
fn bulk_edit_work_grows_with_elements_and_commit_has_no_payload_allocation() {
    let mut costs = Vec::new();
    for len in [1024, 4096, 16384] {
        let mut vm = load(method_package("reverse"));
        let source = copy_source(&mut vm, false, len);
        let task = vm
            .start(FunctionId(0), &[source], RootOwner(9), Context::Ordinary)
            .unwrap();
        let before = vm.heap().stats().chunk_allocations;
        let mut work = 0;
        loop {
            let result = vm.run_slice(task, SliceBudget::default(), || 0).unwrap();
            work += vm.stats().usage.work;
            match result {
                RunOutcome::BudgetYield(_) => {}
                RunOutcome::Returned(id) => {
                    assert!(vm.delivery_value(id).unwrap().same_bits(source));
                    assert_eq!(
                        vm.heap().sequence_get(source, 0).unwrap().as_int(),
                        Some(len as i64 - 1)
                    );
                    assert_eq!(
                        vm.heap().sequence_get(source, len - 1).unwrap().as_int(),
                        Some(0)
                    );
                    break;
                }
                other => panic!("{other:?}"),
            }
        }
        let chunks = vm.heap().stats().chunk_allocations - before;
        assert_eq!(chunks, (len as usize).div_ceil(VALUE_CHUNK_LEN) as u64);
        println!("reverse len={len} work={work} chunks={chunks}");
        costs.push(work);
    }
    assert_eq!(costs[1] - costs[0], 6 * (4096 - 1024));
    assert_eq!(costs[2] - costs[1], 6 * (16384 - 4096));
}

#[test]
fn collection_copy_cost_is_linear_and_allocates_only_payload_chunks() {
    for table in [false, true] {
        let mut measurements = vec![];
        for len in [1024, 4096, 16384] {
            let mut vm = load(collection_copy(table));
            let source = copy_source(&mut vm, table, len);
            let task = vm
                .start(FunctionId(0), &[source], RootOwner(9), Context::Ordinary)
                .unwrap();
            let before = vm.heap().stats().chunk_allocations;
            let now = std::time::Instant::now();
            let mut work = 0;
            loop {
                let outcome = vm.run_slice(task, SliceBudget::default(), || 0).unwrap();
                work += vm.stats().usage.work;
                match outcome {
                    RunOutcome::BudgetYield(_) => {}
                    RunOutcome::Returned(delivery) => {
                        let copy = vm.delivery_value(delivery).unwrap();
                        assert!(!copy.same_bits(source));
                        assert_eq!(vm.heap().info(copy).unwrap().len, len);
                        for index in 0..len {
                            if table {
                                let (key, value) = vm.heap().table_entry(copy, index).unwrap();
                                assert_eq!(key.as_int(), Some(index as i64));
                                assert_eq!(value.as_int(), Some(index as i64));
                            } else {
                                assert_eq!(
                                    vm.heap().sequence_get(copy, index).unwrap().as_int(),
                                    Some(index as i64)
                                );
                            }
                        }
                        break;
                    }
                    other => panic!("{other:?}"),
                }
            }
            let chunks = vm.heap().stats().chunk_allocations - before;
            assert_eq!(
                chunks,
                (len as usize * if table { 2 } else { 1 }).div_ceil(VALUE_CHUNK_LEN) as u64
            );
            println!(
                "collection_copy table={table} len={len} work={work} chunks={chunks} elapsed={:?}",
                now.elapsed()
            );
            measurements.push(work);
        }
        assert_eq!(measurements[1] - measurements[0], 2 * (4096 - 1024));
        assert_eq!(measurements[2] - measurements[1], 2 * (16384 - 4096));
    }
}

#[test]
fn collection_spread_detects_source_and_destination_mutation_and_cancels() {
    for mode in 0..3 {
        let table = mode != 0;
        for action in 0..3 {
            let mut vm = load(collection_copy(table));
            let source = copy_source(&mut vm, table, 130);
            let task = vm
                .start(FunctionId(0), &[source], RootOwner(9), Context::Ordinary)
                .unwrap();
            let builder = loop {
                assert!(matches!(tick(&mut vm, task), RunOutcome::BudgetYield(_)));
                let Frame::Script(s) = *vm.active.frames.data.last().unwrap() else {
                    panic!()
                };
                if mode == 2 && s.pc == Pc(1) && matches!(s.phase, ScriptPhase::Body) {
                    // Force the merge path and suspend inside its key scan.
                    let builder = vm.value(s.regs, 1).unwrap();
                    vm.heap_mut()
                        .table_store(builder, 0, Value::int(-1), Value::NULL)
                        .unwrap();
                }
                if matches!(
                    s.phase,
                    ScriptPhase::Spread(_) | ScriptPhase::TableSpread(_)
                ) || (mode == 2 && s.table_work.is_some())
                {
                    let builder = vm.value(s.env, 1).unwrap();
                    if vm.heap().info(builder).unwrap().len > 0 {
                        break builder;
                    }
                }
            };
            if action == 2 {
                vm.cancel(RootOwner(9)).unwrap();
                loop {
                    match vm.run_slice(task, SliceBudget::default(), || 0) {
                        Ok(RunOutcome::BudgetYield(_)) => gc(&mut vm),
                        Err(VmError::Idle) => break,
                        other => panic!("cancel: {other:?}"),
                    }
                }
                assert_eq!(vm.stats().frames, 0);
            } else {
                let target = if action == 0 { source } else { builder };
                if table {
                    vm.heap_mut()
                        .table_store(target, 0, Value::int(0), Value::NULL)
                        .unwrap();
                } else {
                    vm.heap_mut().set_field(target, 0, Value::NULL).unwrap();
                }
                fault(&mut vm, task, FaultKind::ConcurrentModification);
                assert_eq!(vm.stats().frames, 0);
            }
            while vm.heap().phase() != GcPhase::Idle {
                gc(&mut vm);
            }
            vm.begin_collection().unwrap();
            while vm.heap().phase() != GcPhase::Idle {
                gc(&mut vm);
            }
            assert!(
                vm.heap().info(builder).is_err(),
                "partial copy must not escape"
            );
        }
    }
}

#[test]
fn collection_self_spread_uses_the_initial_sequence_or_existing_table_slots() {
    for table in [false, true] {
        let mut p = collection_copy(table);
        p.functions[0].code = vec![
            op(
                if table {
                    Opcode::TableSpread
                } else {
                    Opcode::ArraySpread
                },
                0,
                0,
                0,
            ),
            op(Opcode::Return, 0, 0, 0),
        ];
        p.functions[0].source_map.clear();
        let mut vm = load(p);
        let source = copy_source(&mut vm, table, 3);
        let task = vm
            .start(FunctionId(0), &[source], RootOwner(9), Context::Ordinary)
            .unwrap();
        let result = returned(&mut vm, task);
        assert!(result.same_bits(source));
        assert_eq!(
            vm.heap().info(result).unwrap().len,
            if table { 3 } else { 6 }
        );
        if !table {
            for index in 0..6 {
                assert_eq!(
                    vm.heap().sequence_get(result, index).unwrap().as_int(),
                    Some((index % 3) as i64)
                );
            }
        }
    }
}

#[test]
fn table_merge_scans_only_the_original_prefix_and_retains_overwrite_order() {
    let mut measurements = vec![];
    for len in [1024, 4096, 16384] {
        let mut p = collection_copy(true);
        let name = SymbolId(p.symbols.len() as u32);
        p.symbols.push("target".into());
        p.signatures[0].parameters.push(Parameter {
            name,
            ty: TypeId(0),
            mutable: false,
            default: None,
        });
        p.functions[0].code.remove(0);
        p.functions[0].source_map.clear();
        let mut vm = load(p);
        let source = copy_source(&mut vm, true, len);
        let target = vm.heap_mut().alloc_table().unwrap();
        // One overlapping key and one prefix-only key. Both keep their order.
        vm.heap_mut()
            .table_store(target, 0, Value::int((len - 1) as i64), Value::NULL)
            .unwrap();
        vm.heap_mut()
            .table_store(target, 1, Value::int(-1), Value::NULL)
            .unwrap();
        let task = vm
            .start(
                FunctionId(0),
                &[source, target],
                RootOwner(9),
                Context::Ordinary,
            )
            .unwrap();
        let mut work = 0;
        loop {
            let outcome = vm.run_slice(task, SliceBudget::default(), || 0).unwrap();
            work += vm.stats().usage.work;
            match outcome {
                RunOutcome::BudgetYield(_) => {}
                RunOutcome::Returned(delivery) => {
                    assert!(vm.delivery_value(delivery).unwrap().same_bits(target));
                    break;
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(vm.heap().table_len(target).unwrap(), len + 1);
        assert_eq!(
            vm.heap().table_entry(target, 0).unwrap().1.as_int(),
            Some((len - 1) as i64)
        );
        assert_eq!(
            vm.heap().table_entry(target, 1).unwrap().0.as_int(),
            Some(-1)
        );
        for index in 0..len - 1 {
            let (key, value) = vm.heap().table_entry(target, index + 2).unwrap();
            assert_eq!(key.as_int(), Some(index as i64));
            assert_eq!(value.as_int(), Some(index as i64));
        }
        println!("table_merge prefix=2 len={len} work={work}");
        measurements.push(work);
    }
    assert_eq!(
        measurements[2] - measurements[1],
        4 * (measurements[1] - measurements[0])
    );
}
#[test]
fn packed_pair_barrier_failure_keeps_payload_versions_and_charge_unchanged() {
    let mut heap = Heap::new(SessionEpoch(91), HeapLimits::default()).unwrap();
    let mut foreign = Heap::new(SessionEpoch(91), HeapLimits::default()).unwrap();
    let packed = heap.alloc_array(&[Value::NULL; 64]).unwrap();
    let invalid = foreign.alloc_array(&[]).unwrap();
    let before = heap.info(packed).unwrap();
    let bytes = heap.stats().charged_bytes;
    assert_eq!(
        heap.packed_pair_push(packed, Value::int(-1), invalid),
        Err(HeapError::WrongHeap)
    );
    let after = heap.info(packed).unwrap();
    assert_eq!(after.len, before.len);
    assert_eq!(after.content_version, before.content_version);
    assert_eq!(after.structure_version, before.structure_version);
    assert_eq!(heap.stats().charged_bytes, bytes);
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

#[test]
fn text_finish_uses_strict_utf8_across_one_byte_and_chunked_reads() {
    let valid: &[&[u8]] = &[b"", b"ASCII\0", "中文é🙂é👩‍🚀".as_bytes()];
    let invalid: &[&[u8]] = &[
        &[0x80],
        &[0xc0, 0xaf],
        &[0xe0, 0x80, 0xaf],
        &[0xed, 0xa0, 0x80],
        &[0xf4, 0x90, 0x80, 0x80],
        &[0xf0, 0x9f, 0x99],
        &[0xf0, 0x28, 0x8c, 0xbc],
    ];
    for step in [1, 2, 3, 1024] {
        for (input, valid) in valid
            .iter()
            .map(|s| (*s, true))
            .chain(invalid.iter().map(|s| (*s, false)))
        {
            let mut p = method_package("unused");
            p.functions[0].code =
                vec![op(Opcode::TextFinish, 1, 0, 0), op(Opcode::Return, 1, 0, 0)];
            p.functions[0].source_map.truncate(2);
            p.functions[0].descriptors.clear();
            let mut limits = VmLimits::default();
            limits.heap.max_mutation_bytes = step;
            let mut vm = load_with_limits(p, limits);
            let buffer = vm.heap_mut().alloc_byte_buffer(&[]).unwrap();
            for part in input.chunks(step as usize) {
                vm.heap_mut().bytes_append(buffer, part).unwrap();
            }
            let task = vm
                .start(FunctionId(0), &[buffer], RootOwner(9), Context::Ordinary)
                .unwrap();
            if valid {
                let value = returned(&mut vm, task);
                assert_eq!(vm.heap().info(value).unwrap().kind, ObjectKind::String);
                let mut actual = vec![];
                for offset in (0..input.len()).step_by(step as usize) {
                    actual.extend_from_slice(vm.heap().byte_segment(value, offset as u32).unwrap());
                }
                assert_eq!(actual, input);
            } else {
                fault(&mut vm, task, FaultKind::Type);
            }
        }
    }
}

#[test]
fn string_join_roots_results_cancels_and_detects_mutated_sources() {
    for action in 0..3 {
        let mut vm = load(text_method_package("join"));
        let separator = vm.heap_mut().alloc_string(" · ").unwrap();
        let text = vm.heap_mut().alloc_string(&"中🙂é".repeat(257)).unwrap();
        let source = vm.heap_mut().alloc_array(&[text, text, text]).unwrap();
        let root = vm
            .heap_mut()
            .add_root(RootOwner(8), RootKind::Host, source)
            .unwrap();
        let task = vm
            .start(
                FunctionId(0),
                &[separator, source],
                RootOwner(9),
                Context::Ordinary,
            )
            .unwrap();
        let buffer;
        loop {
            assert!(matches!(tick(&mut vm, task), RunOutcome::BudgetYield(_)));
            let Frame::Script(s) = *vm.active.frames.data.last().unwrap() else {
                panic!()
            };
            if matches!(s.phase, ScriptPhase::TextBuiltin(_)) {
                let candidate = vm.value(s.env, 2).unwrap();
                if vm.heap().info(candidate).unwrap().len >= 1024 {
                    assert_eq!(
                        vm.heap().info(candidate).unwrap().kind,
                        ObjectKind::ByteBuffer
                    );
                    buffer = candidate;
                    break;
                }
            }
        }
        match action {
            0 => {
                let value = returned(&mut vm, task);
                assert!(value.same_bits(buffer));
                let expected = vec!["中🙂é".repeat(257); 3].join(" · ");
                let mut actual = Vec::new();
                let mut offset = 0;
                while offset < expected.len() {
                    let segment = vm.heap().byte_segment(value, offset as u32).unwrap();
                    actual.extend_from_slice(segment);
                    offset += segment.len();
                }
                assert_eq!(actual, expected.as_bytes());
            }
            1 => {
                vm.heap_mut().set_field(source, 0, Value::NULL).unwrap();
                fault(&mut vm, task, FaultKind::ConcurrentModification);
            }
            _ => {
                vm.cancel(RootOwner(9)).unwrap();
                while let Ok(outcome) = vm.run_slice(task, SliceBudget::default(), || 0) {
                    assert!(matches!(outcome, RunOutcome::BudgetYield(_)));
                }
            }
        }
        while vm.heap().phase() != GcPhase::Idle {
            gc(&mut vm);
        }
        vm.begin_collection().unwrap();
        while vm.heap().phase() != GcPhase::Idle {
            gc(&mut vm);
        }
        if action != 0 {
            assert!(vm.heap().info(buffer).is_err(), "unpublished text retained");
        }
        assert_eq!(vm.heap().info(source).unwrap().len, 3);
        assert!(vm.heap().sequence_get(source, 1).unwrap().same_bits(text));
        assert_eq!(vm.stats().frames, 0);
        vm.heap_mut().release_root(root).unwrap();
    }
}

#[test]
fn text_count_and_join_work_scale_with_bytes_without_copied_prefixes() {
    let mut costs = Vec::new();
    for len in [1024, 4096, 16384] {
        let mut vm = load(text_method_package("join"));
        let separator = vm.heap_mut().alloc_string("").unwrap();
        let part = vm.heap_mut().alloc_string(&"中🙂é".repeat(128)).unwrap();
        let source = vm.heap_mut().alloc_array(&[]).unwrap();
        for _ in 0..len / 128 {
            vm.heap_mut().array_push(source, part).unwrap();
        }
        let task = vm
            .start(
                FunctionId(0),
                &[separator, source],
                RootOwner(9),
                Context::Ordinary,
            )
            .unwrap();
        let before = vm.heap().stats().chunk_allocations;
        let mut work = 0;
        loop {
            let result = vm.run_slice(task, SliceBudget::default(), || 0).unwrap();
            work += vm.stats().usage.work;
            match result {
                RunOutcome::BudgetYield(_) => {}
                RunOutcome::Returned(id) => {
                    let value = vm.delivery_value(id).unwrap();
                    assert_eq!(vm.heap().info(value).unwrap().len, len * 10);
                    break;
                }
                other => panic!("{other:?}"),
            }
        }
        let chunks = vm.heap().stats().chunk_allocations - before;
        assert_eq!(chunks, (len as u64 * 10).div_ceil(BYTE_CHUNK_LEN as u64));
        println!("join bytes={} work={work} chunks={chunks}", len * 10);
        costs.push(work);
    }
    assert_eq!(costs[2] - costs[1], 4 * (costs[1] - costs[0]));

    // Counting retains only the bound method object, irrespective of text size,
    // and std must decode scalars even when the host admits one byte per step.
    for step in [1, 3, 1024] {
        let mut limits = VmLimits::default();
        limits.heap.max_mutation_bytes = step;
        let mut vm = load_with_limits(method_package("scalar_count"), limits);
        let text = "中🙂é👩‍🚀".repeat(129);
        let source = vm.heap_mut().alloc_string(&text).unwrap();
        let task = vm
            .start(FunctionId(0), &[source], RootOwner(9), Context::Ordinary)
            .unwrap();
        let before = vm.heap().stats().chunk_allocations;
        assert_eq!(
            returned(&mut vm, task).as_int(),
            Some(text.chars().count() as i64)
        );
        assert_eq!(vm.heap().stats().chunk_allocations - before, 0);
    }
}
