use super::*;
use crate::snapshot::{SnapshotBudget, SnapshotRegistry};

fn next(vm: &mut Vm, task: TaskId) -> RunOutcome {
    for _ in 0..10000 {
        let result = vm
            .run_slice(
                task,
                SliceBudget {
                    instructions: 1,
                    work: vm.last_usage.required_work.max(1),
                    allocation_bytes: vm.last_usage.required_allocation,
                    ..Default::default()
                },
                || 0,
            )
            .unwrap();
        if !matches!(result, RunOutcome::BudgetYield(_)) {
            return result;
        }
    }
    panic!("native did not settle");
}

fn ticket(vm: &mut Vm, task: TaskId) -> HostTicket {
    let RunOutcome::HostCall(ticket) = next(vm, task) else {
        panic!("expected native call");
    };
    ticket
}

fn collect(vm: &mut Vm) {
    while vm.heap.phase() != GcPhase::Idle {
        vm.gc_step(Default::default()).unwrap();
    }
    vm.begin_collection().unwrap();
    while vm.heap.phase() != GcPhase::Idle {
        vm.gc_step(Default::default()).unwrap();
    }
}

fn restore(vm: Vm, task: TaskId, support: &RuntimeSupport) -> (Vm, TaskId) {
    let registry = SnapshotRegistry::default();
    let mut capture = vm.snapshot_begin(&registry).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let image = capture.finish().unwrap();
    let mut restore = Vm::restore_begin(
        &image,
        vm.program().clone(),
        support.clone(),
        vm.limits,
        SessionEpoch(vm.heap.epoch().0 + 1),
        &registry,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let (vm, map) = restore.finish().unwrap();
    (vm, map.task(task).unwrap())
}

#[test]
fn native_leaf_roots_grow_atomically_for_real_continuations_callbacks_gc_and_restore() {
    for encoded in [false, true] {
        for restore_at in [0, 1, 2] {
            let package = fixtures::helper_native();
            let program = if encoded {
                VerifiedProgram::decode(
                    &encode(&package, Limits::default()).unwrap(),
                    Limits::default(),
                )
                .unwrap()
            } else {
                verify(package, Limits::default()).unwrap()
            };
            let support = crate::executor::review_tests::echo_registry();
            let limits = VmLimits {
                heap: HeapLimits {
                    max_bytes: 1024 * 1024,
                    ..Default::default()
                },
                slots: FrameSlotsLimits {
                    initial_capacity: 1024,
                    max_capacity: 1024,
                    max_window_len: 1024,
                },
                ..Default::default()
            };
            let mut vm = Vm::load(program, support.clone(), limits, SessionEpoch(1)).unwrap();
            let mut task = vm
                .start(FunctionId(0), &[], RootOwner(1), Context::Ordinary)
                .unwrap();
            let entry = ticket(&mut vm, task);
            let roots = vm.ticket(entry).unwrap().roots;
            assert_eq!(
                vm.slots.len(roots).unwrap(),
                3,
                "leaf storage excludes the 520 unused continuation/callback/fault slots"
            );
            assert!(vm.host_continuation(entry).unwrap().is_empty());
            assert!(vm.host_faults(entry).unwrap().is_empty());

            // Exhaust actual physical capacity. Admission must preserve the
            // original issued ticket and avoid reserving a second descriptor.
            let capacity = vm.slots.stats().capacity;
            vm.slots.relocate(&mut vm.heap, capacity).unwrap();
            let remaining = capacity - vm.slots.stats().active_values;
            let filler = vm
                .slots
                .open(&mut vm.heap, RootOwner(2), RootKind::TaskFrame, remaining)
                .unwrap();
            let ranges = vm.heap.stats().root_ranges;
            let response_values = [Value::int(17), Value::int(18), Value::int(19)];
            let response = || {
                HostResponse::Continue(Continuation {
                    version: 1,
                    stage: 9,
                    values: &response_values,
                    faults: FaultRetention::Preserve,
                })
            };
            let denied = vm.respond(entry, response());
            assert!(
                matches!(denied, Err(VmError::Heap(HeapError::WindowCapacity))),
                "{denied:?}"
            );
            assert_eq!(vm.slots.len(roots).unwrap(), 3);
            assert_eq!(vm.heap.stats().root_ranges, ranges);
            assert_eq!(vm.host_arguments(entry).unwrap()[0].as_int(), Some(41));
            vm.slots.release(&mut vm.heap, filler).unwrap();

            // Force replacement buffers, with an incremental collector already
            // active. The first admitted response still owns this same range.
            vm.slots
                .relocate(&mut vm.heap, vm.slots.stats().active_values)
                .unwrap();
            let retained = vm.heap.alloc_array(&[Value::int(73)]).unwrap();
            let usage = vm.heap.budget_usage();
            let pressure = usage
                .reserve(usage.limit() - usage.charged_bytes())
                .unwrap();
            let charged = usage.charged_bytes();
            assert!(matches!(
                vm.respond(
                    entry,
                    HostResponse::Continue(Continuation {
                        version: 1,
                        stage: 9,
                        values: &[retained, retained, Value::int(19)],
                        faults: FaultRetention::Preserve,
                    })
                ),
                Err(VmError::Heap(HeapError::ResourceLimit { .. }))
            ));
            assert_eq!(usage.charged_bytes(), charged);
            assert_eq!(vm.slots.len(roots).unwrap(), 3);
            assert!(vm.host_continuation(entry).unwrap().is_empty());
            drop(pressure);
            vm.begin_collection().unwrap();
            vm.gc_step(GcBudget {
                units: 1,
                ..Default::default()
            })
            .unwrap();
            vm.respond(
                entry,
                HostResponse::Continue(Continuation {
                    version: 1,
                    stage: 9,
                    values: &[retained, retained, Value::int(19)],
                    faults: FaultRetention::Preserve,
                }),
            )
            .unwrap();
            assert_eq!(vm.slots.len(roots).unwrap(), 5);
            collect(&mut vm);
            if restore_at == 1 {
                (vm, task) = restore(vm, task, &support);
            }
            let resumed = ticket(&mut vm, task);
            assert_eq!(
                resumed.input,
                NativeInput::Continue {
                    version: 1,
                    stage: 9
                }
            );
            let values = vm.host_continuation(resumed).unwrap();
            assert!(values[0].same_bits(values[1]));
            assert_eq!(
                vm.heap.sequence_get(values[0], 0).unwrap().as_int(),
                Some(73)
            );
            let retained = values[0];
            let closure = vm
                .heap
                .alloc_record(
                    ObjectKind::Closure,
                    &[Value::int(1), Value::int(1), Value::int(0)],
                )
                .unwrap();
            vm.respond(
                resumed,
                HostResponse::CallScript {
                    closure,
                    arguments: &[Value::int(41)],
                    continuation: Continuation {
                        version: 1,
                        stage: 10,
                        values: &[retained],
                        faults: FaultRetention::Preserve,
                    },
                },
            )
            .unwrap();
            assert_eq!(
                vm.slots
                    .len(
                        vm.active
                            .frames
                            .data
                            .last()
                            .map(|f| match f {
                                Frame::Native(n) => n.roots,
                                _ => unreachable!(),
                            })
                            .unwrap()
                    )
                    .unwrap(),
                259
            );
            collect(&mut vm);
            if restore_at == 2 {
                (vm, task) = restore(vm, task, &support);
            }
            let inner = ticket(&mut vm, task);
            assert_eq!(vm.host_arguments(inner).unwrap()[0].as_int(), Some(41));
            vm.respond(inner, HostResponse::Ready(Value::int(43)))
                .unwrap();
            let outer = ticket(&mut vm, task);
            assert_eq!(outer.input, NativeInput::CallbackReturned);
            assert_eq!(vm.host_resume_value(outer).unwrap().as_int(), Some(43));
            vm.respond(outer, HostResponse::Ready(Value::int(43)))
                .unwrap();
            let RunOutcome::Returned(delivery) = next(&mut vm, task) else {
                panic!("expected native result");
            };
            assert_eq!(vm.delivery_value(delivery).unwrap().as_int(), Some(43));
            assert_eq!(
                vm.native_entries, 2,
                "neither response retry nor restore replays a native entrance"
            );
            vm.discard_delivery(delivery).unwrap();
        }
    }
}

#[test]
fn forbidden_pending_builds_its_effect_fault_after_root_growth_pressure_clears() {
    let program = verify(fixtures::helper_native(), Limits::default()).unwrap();
    let limits = VmLimits {
        slots: FrameSlotsLimits {
            initial_capacity: 2048,
            max_capacity: 2048,
            max_window_len: 2048,
        },
        ..Default::default()
    };
    let mut vm = Vm::load(
        program,
        crate::executor::review_tests::echo_registry(),
        limits,
        SessionEpoch(1),
    )
    .unwrap();
    let task = vm
        .start(FunctionId(0), &[], RootOwner(1), Context::NoWait)
        .unwrap();
    let entry = ticket(&mut vm, task);
    let roots = vm.ticket(entry).unwrap().roots;
    assert_eq!(vm.slots.len(roots).unwrap(), 3);
    let capacity = vm.slots.stats().capacity;
    vm.slots.relocate(&mut vm.heap, capacity).unwrap();
    let remaining = capacity - vm.slots.stats().active_values;
    let filler = vm
        .slots
        .open(&mut vm.heap, RootOwner(2), RootKind::TaskFrame, remaining)
        .unwrap();
    vm.respond(entry, HostResponse::Pending(entry.wait_id()))
        .unwrap();
    assert_eq!(vm.slots.len(roots).unwrap(), 3);
    let RunOutcome::ResourceBlocked(blocked) = next(&mut vm, task) else {
        panic!("fault roots must retain capacity pressure");
    };
    assert_eq!(blocked.resource, BlockedResource::WindowValues);
    collect(&mut vm);
    vm.slots.release(&mut vm.heap, filler).unwrap();
    let outcome = next(&mut vm, task);
    let RunOutcome::Fault(delivery) = outcome else {
        panic!("effect fault must resume after admission: {outcome:?}");
    };
    let fault = vm.delivery_fault(delivery).unwrap().unwrap();
    assert_eq!(fault.code.kind, FaultKind::Effect);
    assert_eq!(fault.location, entry.location);
    assert_eq!(vm.native_entries, 1);
    vm.discard_delivery(delivery).unwrap();
}
