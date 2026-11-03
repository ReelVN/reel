use super::*;
use crate::class::{InstanceSlot, MemberSlot, State as ConstructionState};

#[test]
fn disabled_residual_state_reserves_no_cache_storage() {
    let heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
    let before = heap.budget_usage().charged_bytes();
    let state = State::new(&heap.budget_usage(), false).unwrap();
    assert!(state.cache.is_none());
    assert_eq!(heap.budget_usage().charged_bytes(), before);
    assert_eq!(state.counts(), (0, 0));
    let state = State::new(&heap.budget_usage(), true).unwrap();
    assert!(heap.budget_usage().charged_bytes() > before);
    drop(state);
    assert_eq!(heap.budget_usage().charged_bytes(), before);
}

fn fixture(enabled: bool, encoded: bool, reactive: bool) -> (Vm, Value) {
    fixture_case(enabled, encoded, reactive, false)
}

fn fixture_case(enabled: bool, encoded: bool, reactive: bool, backedge: bool) -> (Vm, Value) {
    let mut p = fixtures::helper_native();
    p.natives.clear();
    p.capabilities.clear();
    let self_name = SymbolId(p.symbols.len() as u32);
    p.symbols.push("receiver".into());
    let time_name = SymbolId(p.symbols.len() as u32);
    p.symbols.push("time".into());
    let flag = SymbolId(p.symbols.len() as u32);
    p.symbols.push("flag".into());
    let threshold = SymbolId(p.symbols.len() as u32);
    p.symbols.push("threshold".into());
    for i in 0..6 {
        p.symbols.push(format!("unused{i}"));
    }
    let float = TypeId(p.types.len() as u32);
    p.types.push(TypeDef::Builtin(BuiltinType::Float));
    p.constants = vec![
        Const::FloatBits(0.0f64.to_bits()),
        Const::FloatBits(2.0f64.to_bits()),
        Const::Null,
    ];
    p.signatures[0].parameters = vec![
        Parameter {
            name: self_name,
            ty: TypeId(0),
            mutable: false,
            default: None,
        },
        Parameter {
            name: time_name,
            ty: float,
            mutable: false,
            default: None,
        },
    ];
    p.signatures[0].return_type = TypeId(0);
    p.signatures[1].parameters.clear();
    p.signatures[1].return_type = TypeId(0);
    p.functions[0].code = vec![
        Instruction::new(Opcode::LoadConst, 0, 2, 0),
        Instruction::new(Opcode::Return, 0, 0, 0),
    ];
    p.functions[0].descriptors.clear();
    p.functions[0].source_map.clear();
    let f = &mut p.functions[1];
    f.register_count = 5;
    f.descriptors.clear();
    f.source_map.clear();
    f.regions.clear();
    f.bindings = vec![
        BindingDef {
            name: self_name,
            ty: TypeId(0),
            mutable: false,
            storage: BindingStorage::Value(Reg(0)),
            initialization: BindingInitialization::Parameter(0),
        },
        BindingDef {
            name: time_name,
            ty: float,
            mutable: false,
            storage: BindingStorage::Value(Reg(1)),
            initialization: BindingInitialization::Parameter(1),
        },
    ];
    let i = Instruction::new;
    f.code = vec![
        i(Opcode::MemberGet, 2, 0, flag.0),
        i(Opcode::BranchBool, 2, 2, 11),
        i(Opcode::Clear, 2, 0, 0),
        i(Opcode::MemberGet, 3, 0, threshold.0),
        i(Opcode::LoadConst, 2, 0, 0),
        i(Opcode::Ge, 4, 3, 2),
        i(Opcode::Clear2, 3, 2, 0),
        i(Opcode::Move, 2, 4, 0),
        i(Opcode::BranchBool, 4, 9, 10),
        i(Opcode::Clear, 4, 0, 0),
        i(Opcode::Clear, 2, 0, 0),
        i(Opcode::LoadConst, 2, 1, 0),
        i(Opcode::Mul, 3, 1, 2),
        i(Opcode::Return, 3, 0, 0),
    ];
    f.code.insert(6, i(Opcode::Eq, 4, 3, 2));
    for instruction in &mut f.code {
        if instruction.opcode == Opcode::BranchBool as u16 {
            for target in &mut instruction.operands[1..] {
                if *target >= 6 {
                    *target += 1;
                }
            }
        }
    }
    if backedge {
        f.code.pop();
        f.code
            .extend([i(Opcode::Move, 0, 1, 0), i(Opcode::Jump, 0, 0, 0)]);
    }
    let program = verify(p, Limits::default()).unwrap();
    let program = if encoded {
        VerifiedProgram::decode(
            program.encode(Limits::default()).unwrap().as_bytes(),
            Limits::default(),
        )
        .unwrap()
    } else {
        program
    };
    let mut vm = Vm::load(
        program,
        RuntimeSupport::default(),
        VmLimits::default(),
        SessionEpoch(99),
    )
    .unwrap();
    vm.residual_body = State::new(&vm.heap.budget_usage(), enabled).unwrap();
    vm.legacy_scalar_suffixes = true;
    let class = vm
        .heap
        .alloc_record(ObjectKind::Class, &[Value::NULL; 9])
        .unwrap();
    let members = vm.heap.alloc_array(&[]).unwrap();
    let fields = vm.heap.alloc_array(&[]).unwrap();
    for index in 0..8 {
        let (name, value, ty) = match index {
            0 => (flag, Value::bool(true), BuiltinType::Bool),
            1 => (threshold, Value::float(-1.0), BuiltinType::Float),
            _ => (
                SymbolId(threshold.0 + index - 1),
                Value::NULL,
                BuiltinType::Any,
            ),
        };
        let contract = crate::CellContract {
            mutable: false,
            ty,
            type_id: None,
            module: ModuleId(0),
            code_version: 1,
            module_version: 1,
        };
        let cell = vm.heap.alloc_language_cell(contract).unwrap();
        vm.heap.cell_initialize(cell, value).unwrap();
        vm.heap.array_push(fields, cell).unwrap();
        let member = vm
            .heap
            .alloc_tuple(&[
                Value::int(i64::from(name.0) * 2),
                class,
                Value::int(index.into()),
                Value::int(index.into()),
                Value::bool(false),
            ])
            .unwrap();
        assert_eq!(
            vm.heap
                .get_field(member, MemberSlot::Offset as u32)
                .unwrap()
                .as_int(),
            Some(index.into())
        );
        vm.heap.array_push(members, member).unwrap();
    }
    for (slot, value) in [
        (ClassSlot::Members, members),
        (ClassSlot::Fields, members),
        (
            ClassSlot::State,
            Value::int(ConstructionState::Ready as i64),
        ),
    ] {
        vm.heap.set_field(class, slot as u32, value).unwrap();
    }
    let instance = vm
        .heap
        .alloc_record(
            ObjectKind::Instance,
            &[
                class,
                fields,
                Value::int(ConstructionState::Ready as i64),
                Value::NULL,
            ],
        )
        .unwrap();
    vm.heap
        .add_root(RootOwner(8), RootKind::Host, instance)
        .unwrap();
    if reactive {
        let output = vm.heap.alloc_derived(Value::NULL).unwrap();
        vm.heap
            .add_root(RootOwner(8), RootKind::Host, output)
            .unwrap();
        vm.heap.begin_derived(output).unwrap();
    }
    (vm, instance)
}

fn start(vm: &mut Vm, instance: Value) -> TaskId {
    vm.start(
        FunctionId(1),
        &[instance, Value::float(7.0)],
        RootOwner(1),
        Context::Derived,
    )
    .unwrap()
}

fn warm(vm: &mut Vm, instance: Value) {
    let task = start(vm, instance);
    let result = vm
        .run_slice(
            task,
            SliceBudget {
                instructions: u32::MAX,
                work: u64::MAX,
                allocation_bytes: u64::MAX,
                deadline_ns: None,
            },
            || 0,
        )
        .unwrap();
    let RunOutcome::Returned(delivery) = result else {
        panic!("warm failed {result:?}")
    };
    assert_eq!(vm.delivery_value(delivery).unwrap().as_float(), Some(14.0));
    vm.discard_delivery(delivery).unwrap();
}

fn canonical(vm: &Vm) -> (Vec<u8>, Vec<u8>) {
    let mut values = Vec::new();
    for frame in &vm.active.frames.data {
        let Frame::Script(frame) = frame else {
            panic!("unexpected native")
        };
        for range in [frame.regs, frame.env, frame.args, frame.bound] {
            values.push(
                (0..vm.slots.len(range).unwrap())
                    .map(|i| {
                        vm.slots.read(at(range, i)).ok().map(|v| {
                            v.object_parts()
                                .map_or(v, |(_, key)| Value::object(HeapId(0), key))
                        })
                    })
                    .collect::<Vec<_>>(),
            );
        }
    }
    (
        postcard::to_allocvec(&vm.active.frames.data).unwrap(),
        postcard::to_allocvec(&values).unwrap(),
    )
}

#[test]
fn residual_spans_preserve_every_budget_clock_pc_and_initialized_root() {
    for encoded in [false, true] {
        for reactive in [false, true] {
            for work in [1, 7, 31, 32, 64, 4096] {
                for instructions in [1, 7, 64] {
                    for deadline in [false, true] {
                        let trace = |enabled| {
                            let (mut vm, instance) = fixture(enabled, encoded, reactive);
                            warm(&mut vm, instance);
                            if enabled {
                                assert!(
                                    vm.residual_body
                                        .cache
                                        .as_ref()
                                        .unwrap()
                                        .borrow()
                                        .plans
                                        .data
                                        .iter()
                                        .any(|p| p.function.is_some())
                                );
                            }
                            let task = start(&mut vm, instance);
                            let mut budget = SliceBudget {
                                instructions,
                                work,
                                allocation_bytes: 0,
                                deadline_ns: deadline.then_some(1),
                            };
                            let mut observations = Vec::new();
                            for _ in 0..1000 {
                                let mut clocks = 0;
                                let outcome = vm
                                    .run_slice(task, budget, || {
                                        let result = u64::from(clocks != 0);
                                        clocks += 1;
                                        result
                                    })
                                    .unwrap();
                                observations.push((vm.last_usage, clocks, canonical(&vm)));
                                match outcome {
                                    RunOutcome::BudgetYield(usage) => {
                                        budget.work = work.max(usage.required_work);
                                        budget.allocation_bytes = usage.required_allocation;
                                    }
                                    RunOutcome::Returned(delivery) => {
                                        assert_eq!(
                                            vm.delivery_value(delivery).unwrap().as_float(),
                                            Some(14.0)
                                        );
                                        return observations;
                                    }
                                    other => panic!("unexpected {other:?}"),
                                }
                            }
                            panic!("residual execution stalled")
                        };
                        assert_eq!(
                            trace(false),
                            trace(true),
                            "encoded={encoded} reactive={reactive} work={work} instructions={instructions} deadline={deadline}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn warmed_residual_spans_have_real_coverage_and_keep_clock_unwind_publication() {
    for panic_at in 1..7 {
        let run = |enabled| {
            let (mut vm, instance) = fixture(enabled, true, true);
            warm(&mut vm, instance);
            let task = start(&mut vm, instance);
            let mut clocks = 0;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                vm.run_slice(
                    task,
                    SliceBudget {
                        instructions: u32::MAX,
                        work: u64::MAX,
                        allocation_bytes: u64::MAX,
                        deadline_ns: Some(u64::MAX),
                    },
                    || {
                        clocks += 1;
                        if clocks == panic_at {
                            panic!("supplied clock")
                        }
                        0
                    },
                )
            }));
            (
                result.is_err(),
                clocks,
                canonical(&vm),
                vm.residual_body.counts(),
            )
        };
        let old = run(false);
        let new = run(true);
        assert_eq!((old.0, old.1, old.2), (new.0, new.1, new.2));
        if panic_at >= 5 {
            assert!(new.3.0 > 0 && new.3.1 > new.3.0);
        }
    }
}

#[test]
fn cached_shapes_reject_changed_readonly_values_observable_cells_and_failed_instances() {
    for case in 0..4 {
        let (mut vm, instance) = fixture(true, true, true);
        warm(&mut vm, instance);
        warm(&mut vm, instance);
        let before = vm.residual_body.counts();
        let (_, fields) = vm.heap.instance_data(instance).unwrap();
        match case {
            0 => {
                let old = vm.heap.get_field(fields, 1).unwrap();
                let contract = vm.heap.cell_info(old).unwrap().contract;
                let cell = vm.heap.alloc_language_cell(contract).unwrap();
                vm.heap.cell_initialize(cell, Value::float(1.0)).unwrap();
                vm.heap.set_field(fields, 1, cell).unwrap();
            }
            1 => {
                let cell = vm.heap.get_field(fields, 1).unwrap();
                vm.heap.observe_model_object(cell).unwrap();
            }
            2 => vm
                .heap
                .set_field(
                    instance,
                    InstanceSlot::State as u32,
                    Value::int(ConstructionState::Failed as i64),
                )
                .unwrap(),
            _ => {
                let (class, _) = vm.heap.instance_data(instance).unwrap();
                let members = vm.heap.get_field(class, ClassSlot::Members as u32).unwrap();
                let member = vm.heap.get_field(members, 0).unwrap();
                vm.heap.set_field(members, 0, member).unwrap();
            }
        }
        let task = start(&mut vm, instance);
        let outcome = vm
            .run_slice(
                task,
                SliceBudget {
                    instructions: u32::MAX,
                    work: u64::MAX,
                    allocation_bytes: u64::MAX,
                    deadline_ns: None,
                },
                || 0,
            )
            .unwrap();
        if case == 2 {
            let RunOutcome::Fault(delivery) = outcome else {
                panic!("expected failed construction")
            };
            assert_eq!(
                vm.delivery_fault(delivery).unwrap().unwrap().kind,
                FaultKind::ConstructionFailed
            );
        } else {
            let RunOutcome::Returned(delivery) = outcome else {
                panic!("expected field result")
            };
            assert_eq!(vm.delivery_value(delivery).unwrap().as_float(), Some(14.0));
        }
        assert_eq!(vm.residual_body.counts(), before);
        assert!(vm.residual_body.guard_fallbacks.get() > 0);
    }
}

#[test]
fn residual_scope_ends_before_unrecorded_receiver_writes_and_backedges() {
    let run = |enabled| {
        let (mut vm, instance) = fixture_case(enabled, true, true, true);
        for pass in 0..2 {
            let task = start(&mut vm, instance);
            let outcome = vm
                .run_slice(
                    task,
                    SliceBudget {
                        instructions: 128,
                        work: 4096,
                        allocation_bytes: 64 << 10,
                        deadline_ns: None,
                    },
                    || 0,
                )
                .unwrap();
            let RunOutcome::Fault(delivery) = outcome else {
                panic!("receiver rewrite should fault {outcome:?}")
            };
            let fault = vm.delivery_fault(delivery).unwrap().unwrap();
            assert_eq!(fault.kind, FaultKind::Type);
            assert_eq!(fault.location.code.function, FunctionId(1));
            assert_eq!(fault.location.pc, Pc(0));
            vm.discard_delivery(delivery).unwrap();
            if pass == 1 && enabled {
                assert!(vm.residual_body.counts().1 > 0);
            }
        }
        vm.last_usage
    };
    assert_eq!(run(false), run(true));
}

#[test]
fn one_residual_shape_serves_distinct_receivers_without_retaining_their_cells() {
    let (mut vm, instance) = fixture(true, true, true);
    warm(&mut vm, instance);
    let (class, old_fields) = vm.heap.instance_data(instance).unwrap();
    let fields = vm.heap.alloc_array(&[]).unwrap();
    for index in 0..vm.heap.sequence_len(old_fields).unwrap() {
        let old = vm.heap.get_field(old_fields, index).unwrap();
        let cell = vm
            .heap
            .alloc_language_cell(vm.heap.cell_info(old).unwrap().contract)
            .unwrap();
        vm.heap
            .cell_initialize(cell, vm.heap.cell_read(old).unwrap())
            .unwrap();
        vm.heap.array_push(fields, cell).unwrap();
    }
    let other = vm
        .heap
        .alloc_record(
            ObjectKind::Instance,
            &[
                class,
                fields,
                Value::int(ConstructionState::Ready as i64),
                Value::NULL,
            ],
        )
        .unwrap();
    let before = vm.residual_body.counts();
    warm(&mut vm, other);
    let after = vm.residual_body.counts();
    assert!(after.0 > before.0 && after.1 > before.1);
    assert_eq!(
        vm.residual_body
            .cache
            .as_ref()
            .unwrap()
            .borrow()
            .plans
            .data
            .iter()
            .filter(|plan| plan.function.is_some())
            .count(),
        1
    );
}

#[cfg(feature = "execution-counts")]
#[test]
fn residual_spans_keep_original_opcode_and_general_body_phase_counts() {
    let run = |enabled| {
        let (mut vm, instance) = fixture(enabled, true, true);
        warm(&mut vm, instance);
        warm(&mut vm, instance);
        let counts = vm.execution_counts();
        (
            counts.script_steps,
            counts.opcode_admissions().collect::<Vec<_>>(),
        )
    };
    assert_eq!(run(false), run(true));
}

#[test]
fn residual_cache_is_cold_after_snapshot_and_never_keeps_cancelled_frame_values_alive() {
    use crate::snapshot::{SnapshotBudget, SnapshotRegistry, VmSnapshot};
    for cancel in [false, true] {
        let (mut vm, instance) = fixture(true, true, true);
        warm(&mut vm, instance);
        warm(&mut vm, instance);
        assert!(vm.residual_body.counts().1 > 0);
        let task = start(&mut vm, instance);
        let budget = SliceBudget {
            instructions: 7,
            work: 4096,
            allocation_bytes: 64 << 10,
            deadline_ns: None,
        };
        loop {
            assert!(matches!(
                vm.run_slice(task, budget, || 0).unwrap(),
                RunOutcome::BudgetYield(_)
            ));
            if matches!(vm.active.frames.data.last(), Some(Frame::Script(frame)) if frame.pc.0 != 0)
            {
                break;
            }
        }
        let registry = SnapshotRegistry::default();
        let mut capture = vm.snapshot_begin(&registry).unwrap();
        while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let image = VmSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
        let mut restore = Vm::restore_begin(
            &image,
            vm.program().clone(),
            RuntimeSupport::default(),
            VmLimits::default(),
            SessionEpoch(100),
            &registry,
        )
        .unwrap();
        while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let (mut vm, map) = restore.finish().unwrap();
        vm.residual_body = State::new(&vm.heap.budget_usage(), true).unwrap();
        assert_eq!(vm.residual_body.counts(), (0, 0));
        let task = map.task(task).unwrap();
        if cancel {
            vm.cancel_cooperatively(task).unwrap();
        }
        loop {
            match vm.run_slice(task, budget, || 0).unwrap() {
                RunOutcome::BudgetYield(_) => {}
                RunOutcome::Returned(delivery) => {
                    assert!(!cancel);
                    assert_eq!(vm.delivery_value(delivery).unwrap().as_float(), Some(14.0));
                    vm.discard_delivery(delivery).unwrap();
                    break;
                }
                RunOutcome::Fault(delivery) => {
                    assert!(cancel);
                    assert_eq!(
                        vm.delivery_fault(delivery).unwrap().unwrap().kind,
                        FaultKind::Cancelled
                    );
                    vm.discard_delivery(delivery).unwrap();
                    break;
                }
                other => panic!("unexpected restored outcome {other:?}"),
            }
            if vm.heap.phase() == GcPhase::Idle {
                vm.begin_collection().unwrap();
            }
            while vm.heap.phase() != GcPhase::Idle {
                vm.gc_step(GcBudget::default()).unwrap();
            }
        }
        let instance = map.value(instance).unwrap();
        let mut cursor = 0;
        vm.heap
            .release_owner_step(RootOwner(8), &mut cursor, u32::MAX);
        vm.begin_collection().unwrap();
        while vm.heap.phase() != GcPhase::Idle {
            vm.gc_step(GcBudget::default()).unwrap();
        }
        assert!(vm.heap.info(instance).is_err());
    }
}

#[test]
fn hard_cancellation_releases_active_plan_registers_and_weak_cache_values() {
    let (mut vm, instance) = fixture(true, true, true);
    warm(&mut vm, instance);
    warm(&mut vm, instance);
    let task = start(&mut vm, instance);
    assert!(matches!(
        vm.run_slice(
            task,
            SliceBudget {
                instructions: 1,
                ..SliceBudget::default()
            },
            || 0
        )
        .unwrap(),
        RunOutcome::BudgetYield(_)
    ));
    vm.cancel_task(task).unwrap();
    assert!(matches!(
        vm.run_slice(task, SliceBudget::default(), || 0),
        Err(VmError::Idle)
    ));
    let mut cursor = 0;
    vm.heap
        .release_owner_step(RootOwner(8), &mut cursor, u32::MAX);
    vm.begin_collection().unwrap();
    while vm.heap.phase() != GcPhase::Idle {
        vm.gc_step(GcBudget::default()).unwrap();
    }
    assert!(vm.heap.info(instance).is_err());
}
