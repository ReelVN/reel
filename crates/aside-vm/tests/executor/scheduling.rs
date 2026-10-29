//! Task scheduling and bounded execution.
use super::*;

#[test]
fn scalar_chain_preserves_progress_at_every_instruction_and_clock_boundary() {
    let mut package = loop_program(0);
    let code = &mut package.functions[0].code;
    code.truncate(2);
    for _ in 0..97 {
        code.push(Instruction::new(Opcode::Add, 0, 0, 1));
    }
    code.push(Instruction::new(Opcode::Return, 0, 0, 0));
    for quota in [1, 2, 7, 31, 32, 33, 4096] {
        let mut v = vm(package.clone());
        warm_code(&v);
        let task = v
            .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
            .unwrap();
        let mut total = 0;
        let mut clock = 0;
        loop {
            let deadline = clock + 1;
            let out = v
                .run_slice(
                    task,
                    SliceBudget {
                        instructions: quota,
                        deadline_ns: Some(deadline),
                        ..Default::default()
                    },
                    || {
                        let now = clock;
                        clock += 1;
                        now
                    },
                )
                .unwrap();
            total += v.stats().usage.instructions;
            match out {
                RunOutcome::BudgetYield(usage) => {
                    assert!(usage.instructions <= quota.min(32));
                    gc(&mut v);
                }
                RunOutcome::Returned(id) => {
                    assert_eq!(v.delivery_value(id).unwrap().as_int(), Some(97));
                    assert_eq!(total, 100);
                    break;
                }
                other => panic!("{other:?}"),
            }
        }
    }
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
#[test]
fn numeric_hot_loop_has_zero_allocator_calls_and_reports_real_work() {
    let mut v = vm(loop_program(100_000));
    warm_code(&v);
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
fn soft_deadline_batches_clock_reads_without_relaxing_instruction_quota() {
    let mut v = vm(loop_program(10_000));
    warm_code(&v);
    let task = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let mut clock_reads = 0;
    let outcome = v
        .run_slice(
            task,
            SliceBudget {
                instructions: 2048,
                deadline_ns: Some(10),
                ..Default::default()
            },
            || {
                clock_reads += 1;
                0
            },
        )
        .unwrap();
    let RunOutcome::BudgetYield(usage) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(usage.instructions, 2048);
    assert!(
        clock_reads > 1 && clock_reads < 256,
        "clock reads: {clock_reads}"
    );
}

#[test]
fn soft_deadline_expiry_retains_the_next_instruction_for_resumption() {
    let mut v = vm(loop_program(1000));
    warm_code(&v);
    let task = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let mut clock_reads = 0;
    let outcome = v
        .run_slice(
            task,
            SliceBudget {
                deadline_ns: Some(10),
                ..Default::default()
            },
            || {
                clock_reads += 1;
                if clock_reads == 1 { 0 } else { 10 }
            },
        )
        .unwrap();
    let RunOutcome::BudgetYield(usage) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(clock_reads, 2);
    assert!(usage.instructions <= 32);
    loop {
        match v.run_slice(task, SliceBudget::default(), || 0).unwrap() {
            RunOutcome::BudgetYield(_) => {}
            RunOutcome::Returned(delivery) => {
                assert_eq!(v.delivery_value(delivery).unwrap().as_int(), Some(1000));
                break;
            }
            other => panic!("{other:?}"),
        }
    }
}
#[test]
fn slice_deadline_allocation_and_work_limits_preserve_unexecuted_operation() {
    let mut package = fixtures::cell_closures();
    // A closure with six captures crosses the inline payload boundary.
    let Descriptor::Closure { captures, .. } = &mut package.functions[0].descriptors[0] else {
        unreachable!()
    };
    captures.resize(6, CaptureSource::Binding(BindingId(0)));
    package.functions[1].captures.resize(6, CaptureKind::Cell);
    let mut v = vm(package);
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
    // Admit the initial frame windows before isolating the closure's payload.
    assert!(matches!(
        v.run_slice(
            v.task_id().unwrap(),
            SliceBudget {
                instructions: 1,
                ..SliceBudget::default()
            },
            || 0,
        )
        .unwrap(),
        RunOutcome::BudgetYield(_)
    ));
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
                assert_eq!(u.required_allocation, 32 * size_of::<Value>() as u64);
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
            contract: None,
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
fn task_pool_growth_failure_releases_partial_roots_and_preserves_the_live_task() {
    let p = fixtures::helper_native();
    let mut limits = VmLimits::default();
    // Module/pool (4) + caller (4) + first new pool range (1). The next
    // delivery range cannot be admitted and must roll back the new range.
    limits.heap.max_root_ranges = 9;
    let mut v = Vm::load(
        verify(p.clone(), Limits::default()).unwrap(),
        support(&p),
        limits,
        SessionEpoch(4),
    )
    .unwrap();
    let task = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let before = v.stats().slots.active_ranges;
    assert!(matches!(
        v.start(fixtures::ENTRY, &[], RootOwner(18), Context::Ordinary),
        Err(VmError::Heap(HeapError::RootRangeCapacity))
    ));
    assert_eq!(v.stats().slots.active_ranges, before);
    v.cancel_task(task).unwrap();
    while !matches!(
        v.run_slice(task, SliceBudget::default(), || 0),
        Err(VmError::Idle)
    ) {}
    assert_eq!(v.stats().slots.active_ranges, v.pooled_root_ranges());
    v.start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
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
    assert_instruction(&v, info.location, 1);
    assert_eq!(v.stats().native_entries, 0);
    assert_eq!(v.stats().slots.active_ranges, v.pooled_root_ranges());
    v.discard_delivery(d).unwrap();
    collect(&mut v);
    assert_eq!(v.heap().stats().live_objects, 0);
}
