//! Class plans remain local to one admission/execute attempt.
use super::*;
use crate::snapshot::{SnapshotBudget, SnapshotRegistry, VmSnapshot};

fn loaded(reference: bool, encoded: bool, case: u8, derived: bool) -> (Vm, TaskId, Value) {
    let mut p = fixtures::helper_native();
    p.natives.clear();
    p.capabilities.clear();
    p.functions.truncate(1);
    p.signatures.truncate(1);
    p.signatures[0].parameters.clear();
    p.signatures[0].return_type = TypeId(0);
    let f = &mut p.functions[0];
    f.signature = SignatureId(0);
    f.bindings.clear();
    f.descriptors.clear();
    f.regions.clear();
    f.register_count = 3;
    f.code = if case == 3 {
        vec![
            Instruction::new(Opcode::MemberGet, 1, 0, 4),
            Instruction::new(Opcode::Move, 2, 1, 0),
            Instruction::new(Opcode::MemberGet, 1, 0, 4),
            Instruction::new(Opcode::Return, 1, 0, 0),
        ]
    } else if case == 10 {
        vec![
            Instruction::new(Opcode::IndexGet, 1, 0, 2),
            Instruction::new(Opcode::Return, 1, 0, 0),
        ]
    } else {
        vec![
            Instruction::new(Opcode::MemberGet, 1, 0, 4),
            Instruction::new(Opcode::Return, 1, 0, 0),
        ]
    };
    f.source_map = (0..f.code.len())
        .map(|pc| SourceMapEntry {
            pc: Pc(pc as u32),
            span: SourceSpan {
                file: SourceId(0),
                start: 0,
                end: 1,
            },
        })
        .collect();
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
        SessionEpoch(71),
    )
    .unwrap();
    vm.work_probe.general_class_reads = reference;
    vm.work_probe.checked_class_metadata = reference;
    let cell = if case == 5 {
        vm.heap
            .alloc_language_cell(crate::CellContract::untyped_mutable())
            .unwrap()
    } else {
        vm.heap.alloc_cell(Value::int(42)).unwrap()
    };
    if derived {
        vm.heap.observe_model_object(cell).unwrap();
    }
    let class = vm
        .heap
        .alloc_record(ObjectKind::Class, &[Value::NULL; 9])
        .unwrap();
    vm.heap
        .set_field(
            class,
            ClassSlot::State as u32,
            Value::int(State::Ready as i64),
        )
        .unwrap();
    let member = vm
        .heap
        .alloc_tuple(&[
            Value::int(if case == 6 {
                10
            } else {
                8 + i64::from(case == 1)
            }),
            class,
            Value::int(0),
            Value::int(0),
            Value::bool(matches!(case, 3 | 8)),
        ])
        .unwrap();
    let members = vm.heap.alloc_array(&[member]).unwrap();
    let fields = vm.heap.alloc_array(&[cell]).unwrap();
    let closure = vm
        .heap
        .alloc_record(
            ObjectKind::Closure,
            &[Value::int(0), Value::int(1), Value::int(0)],
        )
        .unwrap();
    let closures = vm.heap.alloc_array(&[closure]).unwrap();
    for (slot, value) in [
        (ClassSlot::Members, members),
        (ClassSlot::Fields, fields),
        (ClassSlot::Statics, fields),
        (ClassSlot::Closures, closures),
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
                Value::int(if case == 4 {
                    State::Failed
                } else if case == 5 {
                    State::Constructing
                } else {
                    State::Ready
                } as i64),
                Value::NULL,
            ],
        )
        .unwrap();
    if case == 2 {
        // A super read keeps the derived receiver while looking in its base.
        let mut layout = [Value::NULL; 9];
        layout[ClassSlot::Base as usize] = class;
        layout[ClassSlot::Members as usize] = members;
        layout[ClassSlot::State as usize] = Value::int(State::Ready as i64);
        let derived_class = vm.heap.alloc_record(ObjectKind::Class, &layout).unwrap();
        vm.heap
            .set_field(instance, InstanceSlot::Class as u32, derived_class)
            .unwrap();
    }
    let object = if case == 1 {
        class
    } else if case == 2 {
        vm.heap
            .alloc_record(ObjectKind::Super, &[instance, class])
            .unwrap()
    } else {
        instance
    };
    if derived {
        let output = vm.heap.alloc_derived(Value::NULL).unwrap();
        vm.heap
            .add_root(RootOwner(8), RootKind::Host, output)
            .unwrap();
        vm.heap.begin_derived(output).unwrap();
    }
    let task = vm
        .start(
            FunctionId(0),
            &[],
            RootOwner(1),
            if derived {
                Context::Derived
            } else {
                Context::Ordinary
            },
        )
        .unwrap();
    let Frame::Script(mut frame) = *vm.active.frames.data.last().unwrap() else {
        unreachable!()
    };
    if case == 8 {
        let bound = vm.bind_script(closure, instance, class).unwrap();
        vm.heap
            .add_root(RootOwner(8), RootKind::Host, bound)
            .unwrap();
    }
    if matches!(case, 8 | 9) {
        frame.effects = Effects(frame.effects.0 & !Effects::ALLOCATE);
    }
    frame.phase = ScriptPhase::Body;
    vm.put(frame.regs, 0, object).unwrap();
    if case == 10 {
        let key = vm.heap.alloc_string("x").unwrap();
        vm.put(frame.regs, 2, key).unwrap();
    }
    *vm.active.frames.data.last_mut().unwrap() = Frame::Script(frame);
    // Keep code residency outside the retry window being compared.
    vm.program.package().functions[0]
        .code
        .fetch_budgeted(0, &mut vm.code_window, &mut |_, _| true)
        .unwrap()
        .unwrap();
    (vm, task, cell)
}

#[derive(Debug, PartialEq)]
struct Observation {
    usage: SliceUsage,
    frames: Vec<u8>,
    values: Vec<u8>,
    clocks: u32,
}

fn trace(
    reference: bool,
    encoded: bool,
    case: u8,
    derived: bool,
    work: u64,
    deadline: bool,
    hints: bool,
) -> Vec<Observation> {
    trace_metadata(
        reference, encoded, case, derived, work, deadline, hints, reference,
    )
}

#[allow(clippy::too_many_arguments)]
fn trace_metadata(
    reference: bool,
    encoded: bool,
    case: u8,
    derived: bool,
    work: u64,
    deadline: bool,
    hints: bool,
    checked_metadata: bool,
) -> Vec<Observation> {
    let (mut vm, task, cell) = loaded(reference, encoded, case, derived);
    vm.work_probe.checked_class_metadata = checked_metadata;
    vm.class_member_hints.identities = hints;
    let mut budget = SliceBudget {
        instructions: 1,
        work,
        allocation_bytes: 0,
        deadline_ns: deadline.then_some(1),
    };
    let mut observations = Vec::new();
    let mut changed = false;
    for _ in 0..10000 {
        let mut clocks = 0;
        let outcome = vm
            .run_slice(task, budget, || {
                let now = u64::from(clocks != 0);
                clocks += 1;
                now
            })
            .unwrap();
        let values = vm
            .active
            .frames
            .data
            .iter()
            .map(|frame| {
                let Frame::Script(frame) = frame else {
                    unreachable!()
                };
                (0..3)
                    .map(|reg| vm.value(frame.regs, reg).ok())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        observations.push(Observation {
            usage: vm.last_usage,
            frames: postcard::to_allocvec(&vm.active.frames.data).unwrap(),
            values: postcard::to_allocvec(&values).unwrap(),
            clocks,
        });
        match outcome {
            RunOutcome::BudgetYield(usage) => {
                if case == 7
                    && !changed
                    && usage.instructions == 0
                    && matches!(vm.active.frames.data.last(), Some(Frame::Script(frame))
                        if frame.pc == Pc(0) && matches!(frame.phase, ScriptPhase::Body))
                {
                    vm.heap.cell_write(cell, Value::int(99)).unwrap();
                    changed = true;
                }
                budget.work = work.max(usage.required_work);
                budget.allocation_bytes = usage.required_allocation;
                review_tests::gc(&mut vm);
            }
            RunOutcome::Returned(delivery) => {
                let value = vm.delivery_value(delivery).unwrap();
                if case == 3 {
                    assert_eq!(vm.heap.info(value).unwrap().kind, ObjectKind::BoundScript);
                    assert_eq!(vm.script_binding_misses, 1);
                    assert_eq!(vm.script_binding_hits, 1);
                } else {
                    assert_eq!(value.as_int(), Some(if changed { 99 } else { 42 }));
                }
                return observations;
            }
            RunOutcome::Fault(delivery) => {
                assert_eq!(
                    vm.delivery_fault(delivery).unwrap().unwrap().kind,
                    match case {
                        4 => FaultKind::ConstructionFailed,
                        5 => FaultKind::UninitializedField,
                        6 => FaultKind::MissingKey,
                        8 => FaultKind::Effect,
                        _ => panic!("unexpected class fault"),
                    }
                );
                return observations;
            }
            other => panic!("unexpected class read outcome {other:?}"),
        }
    }
    panic!("class read did not progress");
}

#[test]
fn borrowed_metadata_preserves_checked_planning_at_each_budget_and_gc_boundary() {
    for encoded in [false, true] {
        for case in 0..11 {
            for derived in [false, true] {
                for work in [1, 31, 4096] {
                    for deadline in [false, true] {
                        for hints in [false, true] {
                            assert_eq!(
                                trace_metadata(
                                    false, encoded, case, derived, work, deadline, hints, false
                                ),
                                trace_metadata(
                                    false, encoded, case, derived, work, deadline, hints, true
                                ),
                                "case={case} encoded={encoded} derived={derived} work={work} deadline={deadline} hints={hints}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn one_attempt_class_plans_preserve_work_faults_observation_and_weak_binding() {
    for encoded in [false, true] {
        for case in 0..11 {
            for derived in [false, true] {
                for work in [1, 7, 31, 32, 64, 4096] {
                    for deadline in [false, true] {
                        assert_eq!(
                            trace(false, encoded, case, derived, work, deadline, true),
                            trace(true, encoded, case, derived, work, deadline, false),
                            "case={case} encoded={encoded} derived={derived} work={work} deadline={deadline}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn position_hints_preserve_the_shared_plan_with_cache_disabled() {
    for encoded in [false, true] {
        for case in 0..11 {
            for derived in [false, true] {
                for work in [1, 31, 64, 4096] {
                    for deadline in [false, true] {
                        assert_eq!(
                            trace(false, encoded, case, derived, work, deadline, true),
                            trace(false, encoded, case, derived, work, deadline, false),
                            "case={case} encoded={encoded} derived={derived} work={work} deadline={deadline}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn a_plan_is_discarded_at_budget_yield_and_snapshot_restore() {
    let (mut vm, task, _) = loaded(false, true, 0, false);
    let RunOutcome::BudgetYield(usage) = vm
        .run_slice(
            task,
            SliceBudget {
                instructions: 1,
                work: 1,
                allocation_bytes: 0,
                deadline_ns: None,
            },
            || 0,
        )
        .unwrap()
    else {
        panic!("must stop before member admission")
    };
    assert_eq!(usage.instructions, 0);
    assert!(usage.required_work >= 12);
    let registry = SnapshotRegistry::default();
    let mut capture = vm.snapshot_begin(&registry).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let image = VmSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
    let mut restore = Vm::restore_begin(
        &image,
        vm.program().clone(),
        RuntimeSupport::default(),
        VmLimits::default(),
        SessionEpoch(72),
        &registry,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let (mut restored, map) = restore.finish().unwrap();
    let Frame::Script(frame) = *restored.active.frames.data.last().unwrap() else {
        unreachable!()
    };
    let object = restored.value(frame.regs, 0).unwrap();
    let class = restored
        .heap
        .get_field(object, InstanceSlot::Class as u32)
        .unwrap();
    let members = restored
        .heap
        .get_field(class, ClassSlot::Members as u32)
        .unwrap();
    assert!(
        restored
            .class_member_hints
            .get(
                FunctionId(0),
                Pc(0),
                members,
                restored.heap.info(members).unwrap().content_version
            )
            .is_none()
    );
    loop {
        match restored
            .run_slice(map.task(task).unwrap(), SliceBudget::default(), || 0)
            .unwrap()
        {
            RunOutcome::BudgetYield(_) => {}
            RunOutcome::Returned(delivery) => {
                assert_eq!(
                    restored.delivery_value(delivery).unwrap().as_int(),
                    Some(42)
                );
                break;
            }
            other => panic!("unexpected restored class outcome {other:?}"),
        }
    }
}

fn object(vm: &Vm) -> Value {
    let Some(Frame::Script(frame)) = vm.active.frames.data.last() else {
        unreachable!()
    };
    vm.value(frame.regs, 0).unwrap()
}

fn finish(vm: &mut Vm, task: TaskId) -> Result<i64, FaultKind> {
    let mut budget = SliceBudget {
        instructions: 64,
        work: 4096,
        allocation_bytes: 64 << 10,
        deadline_ns: None,
    };
    loop {
        match vm.run_slice(task, budget, || 0).unwrap() {
            RunOutcome::BudgetYield(usage) => {
                budget.work = budget.work.max(usage.required_work);
                budget.allocation_bytes = budget.allocation_bytes.max(usage.required_allocation);
            }
            RunOutcome::Returned(delivery) => {
                let value = vm.delivery_value(delivery).unwrap().as_int().unwrap();
                vm.discard_delivery(delivery).unwrap();
                return Ok(value);
            }
            RunOutcome::Fault(delivery) => {
                let fault = vm.delivery_fault(delivery).unwrap().unwrap().kind;
                vm.discard_delivery(delivery).unwrap();
                return Err(fault);
            }
            other => panic!("unexpected class outcome {other:?}"),
        }
    }
}

fn read(vm: &mut Vm, object: Value) -> Result<i64, FaultKind> {
    let task = vm
        .start(FunctionId(0), &[], RootOwner(1), Context::Ordinary)
        .unwrap();
    let Frame::Script(mut frame) = *vm.active.frames.data.last().unwrap() else {
        unreachable!()
    };
    frame.phase = ScriptPhase::Body;
    vm.put(frame.regs, 0, object).unwrap();
    *vm.active.frames.data.last_mut().unwrap() = Frame::Script(frame);
    finish(vm, task)
}

#[test]
fn same_site_rechecks_static_keys_current_cells_and_failed_initialization() {
    let (mut vm, task, cell) = loaded(false, true, 0, false);
    let instance = object(&vm);
    vm.heap
        .add_root(RootOwner(8), RootKind::Host, instance)
        .unwrap();
    let class = vm
        .heap
        .get_field(instance, InstanceSlot::Class as u32)
        .unwrap();
    let members = vm.heap.get_field(class, ClassSlot::Members as u32).unwrap();
    assert_eq!(finish(&mut vm, task), Ok(42));
    assert_eq!(vm.work_probe.member_hint_hits.get(), 0);
    assert_eq!(read(&mut vm, instance), Ok(42));
    assert_eq!(vm.work_probe.member_hint_hits.get(), 1);
    let static_cell = vm.heap.alloc_cell(Value::int(77)).unwrap();
    let statics = vm.heap.alloc_array(&[static_cell]).unwrap();
    vm.heap
        .set_field(class, ClassSlot::Statics as u32, statics)
        .unwrap();
    let static_member = vm
        .heap
        .alloc_tuple(&[
            Value::int(9),
            class,
            Value::int(0),
            Value::int(0),
            Value::bool(false),
        ])
        .unwrap();
    vm.heap.array_push(members, static_member).unwrap();
    let before = vm.work_probe.member_hint_hits.get();
    assert_eq!(read(&mut vm, instance), Ok(42)); // Changed content version.
    assert_eq!(vm.work_probe.member_hint_hits.get(), before);
    assert_eq!(read(&mut vm, class), Ok(77)); // Same layout/site, different static bit.
    assert_eq!(vm.work_probe.member_hint_hits.get(), before);
    assert_eq!(read(&mut vm, class), Ok(77));
    assert_eq!(vm.work_probe.member_hint_hits.get(), before + 1);
    assert_eq!(read(&mut vm, instance), Ok(42));
    assert_eq!(vm.work_probe.member_hint_hits.get(), before + 1);
    vm.heap.cell_write(cell, Value::int(99)).unwrap();
    assert_eq!(read(&mut vm, instance), Ok(99));
    assert_eq!(vm.work_probe.member_hint_hits.get(), before + 2);
    vm.heap
        .set_field(
            instance,
            InstanceSlot::State as u32,
            Value::int(State::Failed as i64),
        )
        .unwrap();
    assert_eq!(read(&mut vm, instance), Err(FaultKind::ConstructionFailed));
    let fields = vm
        .heap
        .get_field(instance, InstanceSlot::Fields as u32)
        .unwrap();
    let uninitialized = vm
        .heap
        .alloc_language_cell(crate::CellContract::untyped_mutable())
        .unwrap();
    vm.heap.set_field(fields, 0, uninitialized).unwrap();
    vm.heap
        .set_field(
            instance,
            InstanceSlot::State as u32,
            Value::int(State::Constructing as i64),
        )
        .unwrap();
    assert_eq!(read(&mut vm, instance), Err(FaultKind::UninitializedField));
}

#[test]
fn position_guards_reject_other_generations_and_layout_versions_without_owning_them() {
    let (mut vm, task, _) = loaded(false, true, 0, false);
    let instance = object(&vm);
    vm.heap
        .add_root(RootOwner(8), RootKind::Host, instance)
        .unwrap();
    let class = vm
        .heap
        .get_field(instance, InstanceSlot::Class as u32)
        .unwrap();
    let members = vm.heap.get_field(class, ClassSlot::Members as u32).unwrap();
    assert_eq!(finish(&mut vm, task), Ok(42));
    let (_, bits) = members.object_parts().unwrap();
    vm.class_member_hints.remember(
        FunctionId(0),
        Pc(0),
        0,
        Value::object(vm.heap.id(), bits ^ (1 << 32)),
        vm.heap.info(members).unwrap().content_version,
    );
    assert_eq!(read(&mut vm, instance), Ok(42));
    assert_eq!(vm.work_probe.member_hint_hits.get(), 0);
    let fields = vm
        .heap
        .get_field(instance, InstanceSlot::Fields as u32)
        .unwrap();
    let new_cell = vm.heap.alloc_cell(Value::int(88)).unwrap();
    vm.heap.array_push(fields, new_cell).unwrap();
    let replacement = vm
        .heap
        .alloc_tuple(&[
            Value::int(8),
            class,
            Value::int(0),
            Value::int(1),
            Value::bool(false),
        ])
        .unwrap();
    vm.heap.set_field(members, 0, replacement).unwrap();
    assert_eq!(read(&mut vm, instance), Ok(88));
    assert_eq!(vm.work_probe.member_hint_hits.get(), 0);
    assert_eq!(read(&mut vm, instance), Ok(88));
    assert_eq!(vm.work_probe.member_hint_hits.get(), 1);
    let prefix = vm
        .heap
        .alloc_tuple(&[
            Value::int(6),
            class,
            Value::int(0),
            Value::int(0),
            Value::bool(false),
        ])
        .unwrap();
    let layout = vm.heap.alloc_array(&[prefix, replacement]).unwrap();
    vm.heap
        .set_field(class, ClassSlot::Members as u32, layout)
        .unwrap();
    if vm.heap.phase() != GcPhase::Idle {
        while vm.heap.phase() != GcPhase::Idle {
            vm.gc_step(GcBudget::default()).unwrap();
        }
    }
    vm.begin_collection().unwrap();
    while vm.heap.phase() != GcPhase::Idle {
        vm.gc_step(GcBudget::default()).unwrap();
    }
    assert!(
        vm.heap.info(members).is_err(),
        "the position hint must not retain its old member array"
    );
    assert_eq!(read(&mut vm, instance), Ok(88));
    assert_eq!(vm.work_probe.member_hint_hits.get(), 1);
    assert_eq!(read(&mut vm, instance), Ok(88));
    assert_eq!(vm.work_probe.member_hint_hits.get(), 2);
}

#[test]
fn cancellation_precedes_a_warm_position_at_the_retry_boundary() {
    for hard in [false, true] {
        let (mut vm, task, _) = loaded(false, true, 0, false);
        let instance = object(&vm);
        vm.heap
            .add_root(RootOwner(8), RootKind::Host, instance)
            .unwrap();
        assert!(matches!(
            vm.run_slice(
                task,
                SliceBudget {
                    instructions: 1,
                    work: 1,
                    allocation_bytes: 0,
                    deadline_ns: None
                },
                || 0
            )
            .unwrap(),
            RunOutcome::BudgetYield(_)
        ));
        let class = vm
            .heap
            .get_field(instance, InstanceSlot::Class as u32)
            .unwrap();
        let members = vm.heap.get_field(class, ClassSlot::Members as u32).unwrap();
        assert_eq!(
            vm.class_member_hints.get(
                FunctionId(0),
                Pc(0),
                members,
                vm.heap.info(members).unwrap().content_version
            ),
            Some((0, true))
        );
        assert_eq!(vm.work_probe.member_hint_hits.get(), 0);
        if hard {
            vm.cancel_task(task).unwrap();
            loop {
                match vm.run_slice(task, SliceBudget::default(), || 0) {
                    Ok(RunOutcome::BudgetYield(_)) => {}
                    Err(VmError::Idle) => break,
                    other => panic!("unexpected hard-cancel result {other:?}"),
                }
            }
        } else {
            vm.cancel_cooperatively(task).unwrap();
            assert_eq!(finish(&mut vm, task), Err(FaultKind::Cancelled));
        }
        assert_eq!(vm.work_probe.member_hint_hits.get(), 0);
        assert_eq!(read(&mut vm, instance), Ok(42));
        assert_eq!(vm.work_probe.member_hint_hits.get(), 1);
    }
}

#[test]
fn dynamic_class_indices_keep_current_name_resolution_without_position_hints() {
    let (mut vm, task, _) = loaded(false, true, 10, false);
    let instance = object(&vm);
    vm.heap
        .add_root(RootOwner(8), RootKind::Host, instance)
        .unwrap();
    let class = vm
        .heap
        .get_field(instance, InstanceSlot::Class as u32)
        .unwrap();
    let members = vm.heap.get_field(class, ClassSlot::Members as u32).unwrap();
    assert_eq!(finish(&mut vm, task), Ok(42));
    for (name, expected) in [("host.echo", Err(FaultKind::MissingKey)), ("x", Ok(42))] {
        let key = vm.heap.alloc_string(name).unwrap();
        let task = vm
            .start(FunctionId(0), &[], RootOwner(1), Context::Ordinary)
            .unwrap();
        let Frame::Script(mut frame) = *vm.active.frames.data.last().unwrap() else {
            unreachable!()
        };
        frame.phase = ScriptPhase::Body;
        vm.put(frame.regs, 0, instance).unwrap();
        vm.put(frame.regs, 2, key).unwrap();
        *vm.active.frames.data.last_mut().unwrap() = Frame::Script(frame);
        assert_eq!(finish(&mut vm, task), expected);
        assert!(
            vm.class_member_hints
                .get(
                    FunctionId(0),
                    Pc(0),
                    members,
                    vm.heap.info(members).unwrap().content_version
                )
                .is_none()
        );
    }
    assert_eq!(vm.work_probe.member_hint_hits.get(), 0);
}
