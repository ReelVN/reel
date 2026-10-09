//! Author checks of charged selection and saved ExitApply at real finally exits.
use super::review_tests::{collect, echo_registry, gc, reach, retry, tiny};
use super::*;

const OWNER: RootOwner = RootOwner(17);

fn package(inner_catch: bool, extra_regions: u32) -> BytecodePackage {
    let mut p = fixtures::helper_native();
    p.signatures[1].return_type = TypeId(0);
    p.signatures[1].parameters = vec![Parameter {
        name: SymbolId(4),
        ty: TypeId(0),
        mutable: false,
        default: None,
    }];
    let f = &mut p.functions[0];
    f.register_count = 2;
    f.source_map.clear();
    f.bindings = vec![BindingDef {
        name: SymbolId(4),
        ty: TypeId(0),
        mutable: false,
        storage: BindingStorage::Value(Reg(0)),
        initialization: BindingInitialization::Parameter(0),
    }];
    let i = Instruction::new;
    f.code = if inner_catch {
        vec![
            i(Opcode::Return, 0, 0, 0),
            i(Opcode::LoadConst, 1, 2, 0),
            i(Opcode::Throw, 1, 0, 0),
            i(Opcode::LoadConst, 1, 2, 0),
            i(Opcode::Leave, 5, 0, 0),
            i(Opcode::LoadConst, 1, 2, 0),
            i(Opcode::EndFinally, 0, 0, 0),
            i(Opcode::Return, 0, 0, 0),
        ]
    } else {
        vec![
            i(Opcode::Return, 0, 0, 0),
            i(Opcode::LoadConst, 1, 2, 0),
            i(Opcode::Throw, 1, 0, 0),
            i(Opcode::EndFinally, 0, 0, 0),
            i(Opcode::Return, 0, 0, 0),
        ]
    };
    f.regions = vec![ExceptionRegion {
        protected: PcRange {
            start: Pc(0),
            end: Pc(1),
        },
        catch: None,
        finally: Some(PcRange {
            start: Pc(1),
            end: Pc(if inner_catch { 7 } else { 4 }),
        }),
    }];
    if inner_catch {
        f.regions.push(ExceptionRegion {
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
        });
    }
    // Valid disjoint unreachable regions make the real shared selector scan a
    // larger table without changing the active exit or installing fake state.
    for _ in 0..extra_regions {
        let base = f.code.len() as u32;
        let region = f.regions.len() as u32;
        f.code.extend([
            i(Opcode::LoadConst, 1, 2, 0),
            i(Opcode::Leave, base + 4, 0, 0),
            i(Opcode::LoadConst, 1, 2, 0),
            i(Opcode::EndFinally, region, 0, 0),
            i(Opcode::Return, 0, 0, 0),
        ]);
        f.regions.push(ExceptionRegion {
            protected: PcRange {
                start: Pc(base),
                end: Pc(base + 2),
            },
            catch: None,
            finally: Some(PcRange {
                start: Pc(base + 2),
                end: Pc(base + 4),
            }),
        });
    }
    p
}

fn load(inner_catch: bool, extra_regions: u32, max_objects: Option<u32>) -> Vm {
    let mut limits = VmLimits::default();
    if let Some(max_objects) = max_objects {
        limits.heap.max_objects = max_objects;
    }
    Vm::load(
        verify(package(inner_catch, extra_regions), Limits::default()).unwrap(),
        echo_registry(),
        limits,
        SessionEpoch(4),
    )
    .unwrap()
}

fn begin(vm: &mut Vm) -> (TaskId, Value) {
    let old = vm.heap.alloc_cell(Value::int(41)).unwrap();
    let task = vm
        .start(fixtures::ENTRY, &[old], OWNER, Context::Ordinary)
        .unwrap();
    reach(
        vm,
        task,
        |v| matches!(v.active.frames.data.last(), Some(Frame::Script(s)) if matches!(s.phase, ScriptPhase::Exit { origin: Pc(2), reason: ExitReason::Throw(seed), .. } if seed.fault.is_some())),
    );
    assert_eq!(vm.active.unwinds.data.len(), 1);
    vm.work_probe.exit_handler_queries.set(0);
    (task, old)
}

fn selected(vm: &Vm) -> (ScriptFrame, FaultId) {
    let Frame::Script(s) = *vm.active.frames.data.last().unwrap() else {
        panic!()
    };
    let ScriptPhase::ExitApply {
        origin: Pc(2),
        reason: ExitReason::Throw(seed),
        ..
    } = s.phase
    else {
        panic!()
    };
    (s, seed.fault.unwrap())
}

fn select(vm: &mut Vm, task: TaskId) -> SliceUsage {
    let RunOutcome::BudgetYield(u) = vm
        .run_slice(task, SliceBudget { work: 0, ..tiny() }, || 0)
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(vm.work_probe.exit_handler_queries.get(), 0);
    let RunOutcome::BudgetYield(u) = vm
        .run_slice(
            task,
            SliceBudget {
                work: u.required_work,
                ..tiny()
            },
            || 0,
        )
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(vm.work_probe.exit_handler_queries.get(), 1);
    assert_eq!(vm.stats().usage.instructions, 0);
    u
}

fn finish(vm: &mut Vm, task: TaskId) -> (DeliveryId, bool) {
    let mut budget = tiny();
    for _ in 0..10000 {
        let out = vm.run_slice(task, budget, || 0).unwrap();
        gc(vm);
        match out {
            RunOutcome::BudgetYield(u) => retry(&mut budget, u),
            RunOutcome::Returned(d) => return (d, false),
            RunOutcome::Fault(d) => return (d, true),
            other => panic!("{other:?}"),
        }
    }
    panic!("reported grants did not advance exit");
}

#[test]
fn finally_throw_selection_is_admitted_before_scan_and_saved_across_work_and_allocation_retries() {
    let mut vm = load(false, 64, None);
    let (task, old) = begin(&mut vm);
    let roots = vm.slots.stats().active_ranges;
    let unwind = *vm.active.unwinds.data.last().unwrap();
    let RunOutcome::BudgetYield(u) = vm
        .run_slice(task, SliceBudget { work: 0, ..tiny() }, || 0)
        .unwrap()
    else {
        panic!()
    };
    let selection_work = u.required_work;
    for _ in 0..3 {
        let RunOutcome::BudgetYield(u) = vm
            .run_slice(
                task,
                SliceBudget {
                    work: selection_work - 1,
                    ..tiny()
                },
                || 0,
            )
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(u.required_work, selection_work);
        assert_eq!(vm.work_probe.exit_handler_queries.get(), 0);
        assert_eq!(vm.stats().usage.instructions, 0);
        assert!(
            matches!(vm.active.frames.data.last(), Some(Frame::Script(s)) if matches!(s.phase, ScriptPhase::Exit { origin: Pc(2), .. }))
        );
        gc(&mut vm);
    }
    let RunOutcome::BudgetYield(u) = vm
        .run_slice(
            task,
            SliceBudget {
                work: selection_work,
                ..tiny()
            },
            || 0,
        )
        .unwrap()
    else {
        panic!()
    };
    let (s, fault) = selected(&vm);
    assert!(matches!(
        s.phase,
        ScriptPhase::ExitApply { handler: None, .. }
    ));
    assert_eq!(vm.work_probe.exit_handler_queries.get(), 1);
    assert_eq!(u.required_allocation, crate::fault::FAULT_NODE_BYTES);
    for (work, allocation) in [
        (u.required_work - 1, u.required_allocation),
        (u.required_work, 0),
        (u.required_work, u.required_allocation - 1),
        (u.required_work, 0),
    ] {
        assert!(matches!(
            vm.run_slice(
                task,
                SliceBudget {
                    work,
                    allocation_bytes: allocation,
                    ..tiny()
                },
                || 0
            )
            .unwrap(),
            RunOutcome::BudgetYield(_)
        ));
        assert_eq!(selected(&vm).1, fault);
        assert_eq!(vm.work_probe.exit_handler_queries.get(), 1);
        assert_eq!(vm.slots.stats().active_ranges, roots);
        assert_eq!(
            vm.active.unwinds.data.last().unwrap().replacement_event,
            None
        );
        assert!(vm.value(unwind.value, 0).unwrap().same_bits(old));
        assert!(
            vm.heap
                .fault_causes(fault)
                .unwrap()
                .next(&vm.heap)
                .unwrap()
                .is_none()
        );
        gc(&mut vm);
    }
    assert!(matches!(
        vm.run_slice(
            task,
            SliceBudget {
                work: u.required_work,
                allocation_bytes: u.required_allocation,
                ..tiny()
            },
            || 0
        )
        .unwrap(),
        RunOutcome::BudgetYield(_)
    ));
    assert_eq!(vm.work_probe.exit_handler_queries.get(), 1);
    assert_eq!(vm.active.unwinds.data.len(), 0);
    assert!(vm.value(unwind.value, 0).is_err());
    let (d, failed) = finish(&mut vm, task);
    assert!(failed);
    assert_eq!(vm.delivery_fault(d).unwrap().unwrap().id, fault);
    collect(&mut vm);
    let mut causes = vm.heap.fault_causes(fault).unwrap();
    let cause = causes.next(&vm.heap).unwrap().unwrap();
    assert_eq!(cause.kind, CauseKind::ReplacedExit(ReplacedExit::Return));
    assert!(cause.value.same_bits(old));
    assert!(causes.next(&vm.heap).unwrap().is_none());
    assert_eq!(vm.heap.cell_read(old).unwrap().as_int(), Some(41));
    assert_eq!(vm.work_probe.exit_handler_queries.get(), 1);
    assert_eq!(vm.slots.stats().active_ranges, vm.pooled_root_ranges());
    vm.discard_delivery(d).unwrap();
    collect(&mut vm);
    assert!(vm.heap.cell_info(old).is_err());
    assert_eq!(vm.heap.stats().live_objects, 0);
}

#[test]
fn saved_exit_handler_and_cause_event_survive_hard_capacity_until_gc_releases_space() {
    let mut vm = load(false, 8, Some(4));
    let blocker = vm.heap.alloc_cell(Value::NULL).unwrap();
    let blocker_root = vm
        .heap
        .add_root(RootOwner(77), RootKind::Host, blocker)
        .unwrap();
    let (task, old) = begin(&mut vm);
    let u = select(&mut vm, task);
    let (_, fault) = selected(&vm);
    let mut event = None;
    for _ in 0..3 {
        let RunOutcome::ResourceBlocked(blocked) = vm
            .run_slice(
                task,
                SliceBudget {
                    work: u.required_work,
                    allocation_bytes: u.required_allocation,
                    ..tiny()
                },
                || 0,
            )
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(blocked.resource, BlockedResource::HeapObjects);
        assert_eq!(vm.work_probe.exit_handler_queries.get(), 1);
        assert_eq!(selected(&vm).1, fault);
        let recorded = vm
            .active
            .unwinds
            .data
            .last()
            .unwrap()
            .replacement_event
            .unwrap();
        if let Some(event) = event {
            assert_eq!(recorded, event);
        }
        event = Some(recorded);
        assert!(
            vm.heap
                .fault_causes(fault)
                .unwrap()
                .next(&vm.heap)
                .unwrap()
                .is_none()
        );
        gc(&mut vm);
    }
    vm.heap.release_root(blocker_root).unwrap();
    collect(&mut vm);
    assert!(vm.heap.cell_info(blocker).is_err());
    let (d, failed) = finish(&mut vm, task);
    assert!(failed);
    assert_eq!(vm.work_probe.exit_handler_queries.get(), 1);
    let mut causes = vm.heap.fault_causes(fault).unwrap();
    let cause = causes.next(&vm.heap).unwrap().unwrap();
    assert_eq!(Some(cause.event), event);
    assert!(cause.value.same_bits(old));
    assert!(causes.next(&vm.heap).unwrap().is_none());
    vm.discard_delivery(d).unwrap();
    collect(&mut vm);
    assert_eq!(vm.heap.stats().live_objects, 0);
}

#[test]
fn finally_inner_catch_needs_no_replacement_allocation_and_preserves_pending_return() {
    let mut vm = load(true, 8, None);
    let (task, old) = begin(&mut vm);
    let u = select(&mut vm, task);
    assert_eq!(u.required_allocation, 0);
    let (s, fault) = selected(&vm);
    assert!(matches!(
        s.phase,
        ScriptPhase::ExitApply {
            handler: Some(HandlerTarget {
                kind: HandlerKind::Catch,
                pc: Pc(3),
                ..
            }),
            ..
        }
    ));
    let unwind = *vm.active.unwinds.data.last().unwrap();
    assert!(matches!(
        vm.run_slice(
            task,
            SliceBudget {
                work: u.required_work,
                allocation_bytes: 0,
                ..tiny()
            },
            || 0
        )
        .unwrap(),
        RunOutcome::BudgetYield(_)
    ));
    assert_eq!(vm.work_probe.exit_handler_queries.get(), 1);
    assert_eq!(vm.active.caught.data.len(), 1);
    assert_eq!(vm.active.unwinds.data.len(), 1);
    assert_eq!(
        vm.active.unwinds.data.last().unwrap().replacement_event,
        None
    );
    assert!(vm.value(unwind.value, 0).unwrap().same_bits(old));
    assert!(
        vm.heap
            .fault_causes(fault)
            .unwrap()
            .next(&vm.heap)
            .unwrap()
            .is_none()
    );
    let (d, failed) = finish(&mut vm, task);
    assert!(!failed);
    collect(&mut vm);
    assert!(vm.delivery_value(d).unwrap().same_bits(old));
    assert!(vm.heap.fault_info(fault).is_err());
    assert_eq!(vm.active.unwinds.data.len(), 0);
    vm.discard_delivery(d).unwrap();
    collect(&mut vm);
    assert_eq!(vm.heap.stats().live_objects, 0);
}

#[test]
fn cancellation_invalidates_saved_exit_selection_and_retains_the_complete_prior_fault() {
    let mut vm = load(false, 8, None);
    let (task, old) = begin(&mut vm);
    select(&mut vm, task);
    let (_, prior) = selected(&vm);
    vm.cancel_cooperatively(task).unwrap();
    let (d, failed) = finish(&mut vm, task);
    assert!(failed);
    let info = vm.delivery_fault(d).unwrap().unwrap();
    assert_eq!(info.kind, FaultKind::Cancelled);
    assert_ne!(info.id, prior);
    collect(&mut vm);
    let mut causes = vm.heap.fault_causes(info.id).unwrap();
    let replaced = causes.next(&vm.heap).unwrap().unwrap();
    assert_eq!(replaced.kind, CauseKind::ReplacedExit(ReplacedExit::Return));
    assert!(replaced.value.same_bits(old));
    let explicit = causes.next(&vm.heap).unwrap().unwrap();
    assert_eq!(explicit.kind, CauseKind::ExplicitCause);
    assert_eq!(explicit.fault, Some(prior));
    assert!(causes.next(&vm.heap).unwrap().is_none());
    assert_eq!(vm.heap.fault_info(prior).unwrap().kind, FaultKind::Thrown);
    assert_eq!(vm.work_probe.exit_handler_queries.get(), 2); // one new selection for cancellation
    vm.discard_delivery(d).unwrap();
    collect(&mut vm);
    assert_eq!(vm.heap.stats().live_objects, 0);
}

#[test]
fn ordinary_exit_application_precharges_no_hypothetical_root_release() {
    let mut p = package(false, 0);
    p.functions[0].code = vec![Instruction::new(Opcode::Return, 0, 0, 0)];
    p.functions[0].regions.clear();
    let mut vm = Vm::load(
        verify(p, Limits::default()).unwrap(),
        echo_registry(),
        VmLimits::default(),
        SessionEpoch(4),
    )
    .unwrap();
    let old = vm.heap.alloc_cell(Value::int(41)).unwrap();
    let task = vm
        .start(fixtures::ENTRY, &[old], OWNER, Context::Ordinary)
        .unwrap();
    reach(
        &mut vm,
        task,
        |v| matches!(v.active.frames.data.last(), Some(Frame::Script(s)) if matches!(s.phase, ScriptPhase::Exit { reason: ExitReason::Return, .. })),
    );
    vm.work_probe.exit_handler_queries.set(0);
    let RunOutcome::BudgetYield(u) = vm
        .run_slice(task, SliceBudget { work: 1, ..tiny() }, || 0)
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(vm.work_probe.exit_handler_queries.get(), 1);
    assert_eq!(u.required_work, 12); // bounded exit bookkeeping; no free-list release or open
    assert_eq!(u.required_allocation, 0);
    let roots = vm.slots.stats().active_ranges;
    assert!(matches!(
        vm.run_slice(
            task,
            SliceBudget {
                work: u.required_work,
                ..tiny()
            },
            || 0
        )
        .unwrap(),
        RunOutcome::BudgetYield(_)
    ));
    assert!(
        matches!(vm.active.frames.data.last(), Some(Frame::Script(s)) if matches!(s.phase, ScriptPhase::Release { .. }))
    );
    assert_eq!(vm.slots.stats().active_ranges, roots); // actual frame teardown is a separate admitted stage
    let (d, failed) = finish(&mut vm, task);
    assert!(!failed);
    assert!(vm.delivery_value(d).unwrap().same_bits(old));
    vm.discard_delivery(d).unwrap();
    collect(&mut vm);
    assert_eq!(vm.heap.stats().live_objects, 0);
}
