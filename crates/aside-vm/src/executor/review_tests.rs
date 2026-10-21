//! Author regressions for the source findings on review head 4fef7b6.
//! These execute hand-authored B01 packages, not independent acceptance.
use super::*;

const OWNER: RootOwner = RootOwner(17);

pub(super) fn echo_registry() -> RuntimeSupport {
    RuntimeSupport {
        host_types: Vec::new(),
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

fn default_registry() -> RuntimeSupport {
    // Trusted adapter declaration independent of the package's NativeDef.
    let parameter = |name, mutable, default| Parameter {
        name: SymbolId(name),
        ty: TypeId(1),
        mutable,
        default,
    };
    RuntimeSupport {
        host_types: Vec::new(),
        opcodes: vec![],
        capabilities: vec!["host.parameter_defaults".into()],
        natives: vec![NativeSupport {
            symbol: "host::parameter_defaults".into(),
            abi_version: 1,
            signature: Signature {
                parameters: vec![
                    parameter(4, true, None),
                    parameter(14, false, Some(FunctionId(1))),
                    parameter(15, true, Some(FunctionId(2))),
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

fn load(p: BytecodePackage, registry: RuntimeSupport) -> Vm {
    Vm::load(
        verify(p, Limits::default()).unwrap(),
        registry,
        VmLimits::default(),
        SessionEpoch(4),
    )
    .unwrap()
}

pub(super) fn tiny() -> SliceBudget {
    SliceBudget {
        instructions: 1,
        work: 1,
        allocation_bytes: 0,
        deadline_ns: None,
    }
}

pub(super) fn gc(vm: &mut Vm) {
    if vm.heap.phase() == GcPhase::Idle {
        vm.begin_collection().unwrap();
    }
    vm.gc_step(GcBudget {
        units: 1,
        ..GcBudget::default()
    })
    .unwrap();
}

pub(super) fn collect(vm: &mut Vm) {
    while vm.heap.phase() != GcPhase::Idle {
        gc(vm);
    }
    vm.begin_collection().unwrap();
    while vm.heap.phase() != GcPhase::Idle {
        gc(vm);
    }
}

pub(super) fn retry(budget: &mut SliceBudget, usage: SliceUsage) {
    budget.work = usage.required_work.max(1);
    budget.allocation_bytes = usage.required_allocation;
}

pub(super) fn reach(vm: &mut Vm, task: TaskId, goal: impl Fn(&Vm) -> bool) {
    let mut budget = tiny();
    for _ in 0..5000 {
        if goal(vm) {
            return;
        }
        let out = vm.run_slice(task, budget, || 0).unwrap();
        gc(vm);
        match out {
            RunOutcome::BudgetYield(u) => retry(&mut budget, u),
            RunOutcome::HostCall(t) => vm.respond(t, HostResponse::Ready(Value::int(42))).unwrap(),
            other => panic!("stage was not reached: {other:?}"),
        }
    }
    panic!("stage did not make bounded progress");
}

#[test]
fn release_rejects_insufficient_work_before_any_catch_predicate_then_exact_retry_advances() {
    let mut p = fixtures::helper_native();
    let f = &mut p.functions[0];
    let i = Instruction::new;
    f.source_map.clear();
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
    let mut vm = load(p, echo_registry());
    let task = vm
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    reach(
        &mut vm,
        task,
        |v| matches!(v.active.frames.data.last(), Some(Frame::Script(s)) if s.code.function == FunctionId(1) && matches!(s.phase, ScriptPhase::Release { .. })),
    );
    assert_eq!(vm.active.caught.data.len(), 1);
    let frame = vm.active.frames.data.last().unwrap().id();
    let frames = vm.active.frames.data.len();
    vm.work_probe.release_catch_checks.set(0);
    let RunOutcome::BudgetYield(u) = vm
        .run_slice(task, SliceBudget { work: 0, ..tiny() }, || 0)
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(vm.work_probe.release_catch_checks.get(), 0);
    let work = u.required_work;
    for _ in 0..3 {
        let RunOutcome::BudgetYield(u) = vm
            .run_slice(
                task,
                SliceBudget {
                    work: work - 1,
                    ..tiny()
                },
                || 0,
            )
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(u.required_work, work);
        assert_eq!(vm.work_probe.release_catch_checks.get(), 0);
        assert_eq!(vm.active.frames.data.last().unwrap().id(), frame);
        gc(&mut vm);
    }
    assert!(matches!(
        vm.run_slice(task, SliceBudget { work, ..tiny() }, || 0)
            .unwrap(),
        RunOutcome::BudgetYield(_)
    ));
    assert_eq!(vm.work_probe.release_catch_checks.get(), 1);
    assert_eq!(vm.active.frames.data.len(), frames - 1);
    assert_eq!(vm.active.caught.data.len(), 1); // the outer catch still owns its record
    assert_eq!(vm.native_entries, 1);
}

#[test]
fn native_call_admission_uses_charged_cache_and_parameter_queries_wait_for_work() {
    let mut vm = load(fixtures::native_parameter_defaults(), default_registry());
    assert_eq!(vm.native_cells.data, [true]);
    assert_eq!(vm.native_cells.bytes(), 1);
    let task = vm
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    reach(
        &mut vm,
        task,
        |v| matches!(v.active.frames.data.last(), Some(Frame::Script(s)) if s.pc == Pc(1) && matches!(s.phase, ScriptPhase::Body)),
    );
    let roots = vm.slots.stats().active_ranges;
    for _ in 0..3 {
        assert!(matches!(
            vm.run_slice(task, SliceBudget { work: 0, ..tiny() }, || 0)
                .unwrap(),
            RunOutcome::BudgetYield(_)
        ));
        assert_eq!(vm.work_probe.native_layout_queries.get(), 0);
        assert_eq!(vm.slots.stats().active_ranges, roots);
    }
    reach(
        &mut vm,
        task,
        |v| matches!(v.active.frames.data.last(), Some(Frame::Native(n)) if matches!(n.phase, NativePhase::Defaults { parameter: 0 })),
    );
    assert_eq!(vm.work_probe.native_layout_queries.get(), 0);
    let RunOutcome::BudgetYield(u) = vm
        .run_slice(task, SliceBudget { work: 0, ..tiny() }, || 0)
        .unwrap()
    else {
        panic!()
    };
    let work = u.required_work;
    for _ in 0..3 {
        assert!(matches!(
            vm.run_slice(
                task,
                SliceBudget {
                    work: work - 1,
                    ..tiny()
                },
                || 0
            )
            .unwrap(),
            RunOutcome::BudgetYield(_)
        ));
        assert_eq!(vm.work_probe.native_layout_queries.get(), 0);
        gc(&mut vm);
    }
    assert!(matches!(
        vm.run_slice(task, SliceBudget { work, ..tiny() }, || 0)
            .unwrap(),
        RunOutcome::BudgetYield(_)
    ));
    assert_eq!(vm.work_probe.native_layout_queries.get(), 1);
    assert!(
        matches!(vm.active.frames.data.last(), Some(Frame::Native(n)) if matches!(n.phase, NativePhase::Parameter { parameter: 0, storage: BindingStorage::Cell(_) }))
    );
}

fn lexical_defaults() -> BytecodePackage {
    let mut p = fixtures::native_parameter_defaults();
    let path = p.symbols.len() as u32;
    p.symbols.push("game::defaults".into());
    p.modules.push(ModuleDef {
        path: SymbolId(path),
        imports: vec![],
        exports: vec![],
        globals: vec![],
        initializer: None,
    });
    for f in &mut p.functions[1..] {
        f.module = ModuleId(1);
    }
    p
}

#[test]
fn cross_module_value_default_fault_has_lexical_code_source_and_stack_module_before_native_entry() {
    let mut p = lexical_defaults();
    let mut registry = default_registry();
    registry.natives[0].signature.parameters[0].mutable = false;
    for signature in [1, 2, 3] {
        p.signatures[signature].parameters[0].mutable = false;
    }
    for function in [1, 2] {
        p.functions[function].bindings[0].mutable = false;
        p.functions[function].bindings[0].storage = BindingStorage::Value(Reg(0));
    }
    p.functions[2].bindings[1].storage = BindingStorage::Value(Reg(1));
    p.functions[3].captures = vec![CaptureKind::Value];
    p.functions[3].code = vec![
        Instruction::new(Opcode::CaptureRead, 0, 0, 0),
        Instruction::new(Opcode::Return, 0, 0, 0),
    ];
    let path = p.symbols.len() as u32;
    p.symbols.push("defaults.aside".into());
    p.sources.push(SourceFile {
        path: SymbolId(path),
        text: "throw 41;".into(),
    });
    p.functions[1].code = vec![
        Instruction::new(Opcode::LoadConst, 1, 1, 0),
        Instruction::new(Opcode::Throw, 1, 0, 0),
    ];
    let source = SourceSpan {
        file: SourceId(1),
        start: 0,
        end: 9,
    };
    p.functions[1].source_map = vec![SourceMapEntry {
        pc: Pc(1),
        span: source,
    }];
    let mut vm = load(p, registry);
    assert_eq!(vm.native_cells.data, [false]);
    let task = vm
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let mut budget = tiny();
    let mut delivery = None;
    for _ in 0..5000 {
        let out = vm.run_slice(task, budget, || 0).unwrap();
        gc(&mut vm);
        match out {
            RunOutcome::BudgetYield(u) => retry(&mut budget, u),
            RunOutcome::Fault(d) => {
                delivery = Some(d);
                break;
            }
            other => panic!("{other:?}"),
        }
    }
    let d = delivery.unwrap();
    collect(&mut vm);
    let info = vm.delivery_fault(d).unwrap().unwrap();
    assert_eq!(info.kind, FaultKind::Thrown);
    assert_eq!(info.location.code.function, FunctionId(1));
    assert_eq!(info.location.pc, Pc(1));
    assert_eq!(info.location.source, Some(source));
    let mut stack = vm.heap.fault_stack(info.id).unwrap();
    let default = stack.next(&vm.heap).unwrap().unwrap();
    assert_eq!(default.module, ModuleInstanceId(1));
    assert_eq!(default.location, info.location);
    let native = stack.next(&vm.heap).unwrap().unwrap();
    assert_eq!(native.module, ModuleInstanceId(0));
    assert!(matches!(native.kind, StackFrameKind::Native { .. }));
    assert_eq!(
        stack.next(&vm.heap).unwrap().unwrap().module,
        ModuleInstanceId(0)
    );
    assert!(stack.next(&vm.heap).unwrap().is_none());
    assert_eq!(vm.native_entries, 0);
    assert_eq!(vm.delivery_value(d).unwrap().as_int(), Some(41));
    vm.discard_delivery(d).unwrap();
    collect(&mut vm);
    assert_eq!(vm.heap.stats().live_objects, 0);
}

#[test]
fn cross_module_default_keeps_shared_binder_cells_and_lexical_local_cells_in_its_captures() {
    let mut p = lexical_defaults();
    let local_name = p.symbols.len() as u32;
    p.symbols.push("local".into());
    let z = &mut p.functions[2];
    z.bindings.push(BindingDef {
        name: SymbolId(local_name),
        ty: TypeId(1),
        mutable: true,
        storage: BindingStorage::Cell(Reg(2)),
        initialization: BindingInitialization::Explicit,
    });
    z.code.splice(
        4..4,
        [
            Instruction::new(Opcode::CellNew, 2, 0, 0),
            Instruction::new(Opcode::LoadConst, 3, 2, 0),
            Instruction::new(Opcode::BindInit, 2, 3, 0),
        ],
    );
    let Descriptor::Closure { captures, .. } = &mut z.descriptors[0] else {
        panic!()
    };
    captures.push(CaptureSource::Binding(BindingId(2)));
    let read = &mut p.functions[3];
    read.register_count = 4;
    read.captures.push(CaptureKind::Cell);
    read.code = vec![
        Instruction::new(Opcode::CaptureRead, 0, 0, 0),
        Instruction::new(Opcode::CellRead, 1, 0, 0),
        Instruction::new(Opcode::CaptureRead, 2, 1, 0),
        Instruction::new(Opcode::CellRead, 3, 2, 0),
        Instruction::new(Opcode::Add, 1, 1, 3),
        Instruction::new(Opcode::Return, 1, 0, 0),
    ];
    let mut vm = load(p, default_registry());
    let task = vm
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    reach(
        &mut vm,
        task,
        |v| matches!(v.active.frames.data.last(), Some(Frame::Script(s)) if s.code.function == FunctionId(3) && matches!(s.phase, ScriptPhase::Body)),
    );
    let Frame::Script(read) = *vm.active.frames.data.last().unwrap() else {
        panic!()
    };
    let Frame::Script(default) = vm.active.frames.data[vm.active.frames.data.len() - 2] else {
        panic!()
    };
    assert_eq!(
        (default.module, default.parameter_module),
        (ModuleInstanceId(1), ModuleInstanceId(0))
    );
    let closure = vm.value(read.env, 0).unwrap();
    assert_eq!(vm.heap.get_field(closure, 2).unwrap().as_int(), Some(1));
    let shared = vm.heap.get_field(closure, 3).unwrap();
    let local = vm.heap.get_field(closure, 4).unwrap();
    let native = vm
        .active
        .frames
        .data
        .iter()
        .find_map(|f| match f {
            Frame::Native(n) => Some(*n),
            _ => None,
        })
        .unwrap();
    assert!(shared.same_bits(vm.value(native.bindings.unwrap(), 1).unwrap()));
    for (cell, module, expected) in [(shared, ModuleId(0), 42), (local, ModuleId(1), 1)] {
        let info = vm.heap.cell_info(cell).unwrap();
        assert_eq!(info.contract.module, module);
        assert_eq!(
            info.contract.code_version,
            vm.program.package().content_version
        );
        assert_eq!(
            info.contract.module_version,
            vm.program.package().content_version
        );
        assert_eq!(vm.heap.cell_read(cell).unwrap().as_int(), Some(expected));
    }
    let mut budget = tiny();
    let mut delivery = None;
    for _ in 0..5000 {
        let out = vm.run_slice(task, budget, || 0).unwrap();
        gc(&mut vm);
        match out {
            RunOutcome::BudgetYield(u) => retry(&mut budget, u),
            RunOutcome::HostCall(t) => {
                let arguments = vm.host_arguments(t).unwrap();
                assert_eq!(
                    arguments[..3]
                        .iter()
                        .map(|v| v.as_int().unwrap())
                        .collect::<Vec<_>>(),
                    [43, 42, 43]
                );
                vm.respond(t, HostResponse::Pending(t.wait_id())).unwrap();
            }
            RunOutcome::Waiting(w) => {
                collect(&mut vm);
                vm.complete(w, Completion::Ready(Value::NULL)).unwrap();
            }
            RunOutcome::Returned(d) => {
                delivery = Some(d);
                break;
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(vm.native_entries, 1);
    vm.discard_delivery(delivery.unwrap()).unwrap();
    collect(&mut vm);
    assert!(vm.heap.cell_info(shared).is_err());
    assert!(vm.heap.cell_info(local).is_err());
    assert_eq!(vm.heap.stats().live_objects, 0);
    assert_eq!(vm.slots.stats().active_ranges, vm.pooled_root_ranges());
}
