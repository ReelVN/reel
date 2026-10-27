//! Exception regions, fault retention and cleanup.
use super::*;

#[test]
fn unmatched_branch_reports_a_typed_fault() {
    let mut p = fixtures::helper_native();
    p.functions[0].code[0] = Instruction::new(Opcode::MatchFail, 0, 0, 0);
    let mut v = vm(p);
    v.start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    loop {
        match slice(&mut v) {
            RunOutcome::BudgetYield(_) => gc(&mut v),
            RunOutcome::Fault(id) => {
                let fault = v.delivery_fault(id).unwrap().unwrap();
                assert_eq!(fault.kind, FaultKind::MatchError);
                assert_eq!(fault.location.code.function, fixtures::ENTRY);
                assert_instruction(&v, fault.location, 0);
                v.discard_delivery(id).unwrap();
                break;
            }
            other => panic!("{other:?}"),
        }
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
                    assert_instruction(&v, t.location, 0);
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
                    assert_instruction(&v, info.location, 0);
                    break;
                }
                other => panic!("{other:?}"),
            }
        }
    }
}

#[test]
fn owner_with_only_a_fault_root_requires_explicit_host_cleanup() {
    let mut v = vm(fixtures::uninitialized_cell_read());
    let task = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let delivery = task_finish(&mut v, task);
    let root = v.take_fault_delivery(delivery, OWNER).unwrap();
    let fault = root.id();
    assert!(v.is_idle());
    assert!(matches!(v.cancel(OWNER), Err(VmError::Idle)));
    collect(&mut v);
    assert_eq!(root.info(v.heap()).unwrap().id, fault);

    let end = v.heap().root_slots();
    let mut cursor = 0;
    let mut released = 0;
    while cursor < end {
        let before = cursor;
        released += v.heap_mut().release_owner_step(OWNER, &mut cursor, 1);
        assert_eq!(cursor - before, 1);
    }
    assert_eq!(released, 1);
    assert!(matches!(root.info(v.heap()), Err(HeapError::InvalidRoot)));
    collect(&mut v);
    assert!(v.heap().fault_info(fault).is_err());
    // The host retires this invalidated wrapper rather than releasing it twice.
    drop(root);
}

#[test]
fn native_default_type_failure_and_partial_preparation_resource_failure_never_issue_entry() {
    for limited_frames in [false, true] {
        let mut p = fixtures::native_parameter_defaults();
        if !limited_frames {
            p.functions[1].code[4] = Instruction::new(Opcode::LoadConst, 3, 0, 0);
            p.functions[1]
                .code
                .push(Instruction::new(Opcode::Return, 3, 0, 0));
        }
        let mut limits = VmLimits::default();
        if limited_frames {
            limits.max_frames = 2;
        }
        let mut v = Vm::load(
            verify(p, Limits::default()).unwrap(),
            native_default_registry(false),
            limits,
            SessionEpoch(4),
        )
        .unwrap();
        let task = v
            .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
            .unwrap();
        let d = task_finish(&mut v, task);
        let expected = if limited_frames {
            FaultKind::Resource
        } else {
            FaultKind::Type
        };
        let info = v.delivery_fault(d).unwrap().unwrap();
        assert_eq!(info.kind, expected);
        assert_eq!(v.stats().native_entries, 0);
        assert_eq!(
            v.heap()
                .core_error_info(v.delivery_value(d).unwrap())
                .unwrap()
                .code
                .kind,
            expected
        );
        if limited_frames {
            assert_instruction(&v, info.location, 1);
            assert_eq!(info.location.code.function, FunctionId(0));
        }
        v.discard_delivery(d).unwrap();
        collect(&mut v);
        assert_eq!(v.heap().stats().live_objects, 0);
        assert_eq!(v.stats().slots.active_ranges, v.pooled_root_ranges());
    }
}

#[test]
fn native_rest_collects_only_tail_positional_arguments_with_defaults_skipped() {
    let mut p = fixtures::native_parameter_defaults();
    let f = &mut p.functions[0];
    f.register_count = 6;
    f.code = vec![
        Instruction::new(Opcode::LoadConst, 0, 1, 0),
        Instruction::new(Opcode::LoadConst, 1, 2, 0),
        Instruction::new(Opcode::LoadConst, 2, 1, 0),
        Instruction::new(Opcode::LoadConst, 3, 2, 0),
        Instruction::new(Opcode::LoadConst, 4, 1, 0),
        Instruction::new(Opcode::Call, 5, 0, 0),
        Instruction::new(Opcode::Return, 5, 0, 0),
    ];
    let Descriptor::Call(call) = &mut f.descriptors[0] else {
        panic!()
    };
    call.arguments = Arguments::Window(RegWindow {
        start: Reg(0),
        len: 5,
    });
    let mut v = native_default_vm(p, false);
    let task = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let mut work = 1;
    let mut allocation_bytes = 0;
    let t = loop {
        match v
            .run_slice(
                task,
                SliceBudget {
                    instructions: 1,
                    work,
                    allocation_bytes,
                    ..SliceBudget::default()
                },
                || 0,
            )
            .unwrap()
        {
            RunOutcome::BudgetYield(u) => {
                work = u.required_work.max(1);
                allocation_bytes = u.required_allocation;
                gc(&mut v);
            }
            RunOutcome::HostCall(t) => break t,
            other => panic!("{other:?}"),
        }
    };
    let args = v.host_arguments(t).unwrap();
    assert_eq!(args[0].as_int(), Some(41));
    assert_eq!(args[1].as_int(), Some(1));
    assert_eq!(args[2].as_int(), Some(41));
    let rest = args[3];
    assert_eq!(v.heap().info(rest).unwrap().len, 2);
    assert_eq!(v.heap().get_field(rest, 0).unwrap().as_int(), Some(1));
    assert_eq!(v.heap().get_field(rest, 1).unwrap().as_int(), Some(41));
    collect(&mut v);
    v.cancel_task(task).unwrap();
    loop {
        match v.run_slice(
            task,
            SliceBudget {
                instructions: 1,
                work: 16416,
                ..SliceBudget::default()
            },
            || 0,
        ) {
            Ok(RunOutcome::BudgetYield(u)) => assert!(u.work <= 16416),
            Err(VmError::Idle) => break,
            other => panic!("{other:?}"),
        }
    }
    collect(&mut v);
    assert!(v.heap().info(rest).is_err());
    assert_eq!(v.heap().stats().live_objects, 0);
    assert_eq!(v.stats().slots.active_ranges, v.pooled_root_ranges());
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
                                    faults: FaultRetention::Preserve,
                                    version: 1,
                                    stage: 1,
                                    values: &[state],
                                }),
                            )
                            .unwrap();
                            v.heap_mut().release_root(state_root).unwrap();
                        }
                        NativeInput::Continue {
                            version: 1,
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
                                        faults: FaultRetention::Preserve,
                                        version: 1,
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
                                            faults: FaultRetention::Preserve,
                                            version: 1,
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
#[test]
fn defaults_read_definition_cell_after_defining_frame_is_gone() {
    let mut v = vm(fixtures::captured_default());
    v.start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let id = finish(&mut v);
    let closure = v.delivery_value(id).unwrap();
    let root = v
        .heap_mut()
        .add_root(OWNER, RootKind::Host, Value::NULL)
        .unwrap();
    v.take_delivery_root(id, root).unwrap();
    let cell = v.heap().get_field(closure, 3).unwrap();
    v.heap_mut().cell_write(cell, Value::int(99)).unwrap();
    collect(&mut v);
    v.start_closure(closure, &[], OWNER, Context::Ordinary)
        .unwrap();
    let id = finish(&mut v);
    assert_eq!(v.delivery_value(id).unwrap().as_int(), Some(99));
}
#[test]
fn dynamic_call_type_fault_is_at_call_and_caught_there() {
    let mut p = fixtures::exception_regions();
    let f = &mut p.functions[0];
    f.source_map.clear();
    f.regions.clear();
    f.descriptors = vec![Descriptor::Call(CallDesc {
        contract: None,
        target: CallTarget::Register(Reg(0)),
        arguments: Arguments::Window(RegWindow {
            start: Reg(0),
            len: 0,
        }),
        names: vec![],
    })];
    let i = Instruction::new;
    f.code = vec![
        i(Opcode::LoadConst, 0, 1, 0),
        i(Opcode::Call, 1, 0, 0),
        i(Opcode::Leave, 5, 0, 0),
        i(Opcode::LoadConst, 0, 2, 0),
        i(Opcode::Return, 0, 0, 0),
        i(Opcode::Return, 0, 0, 0),
    ];
    f.regions = vec![ExceptionRegion {
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
    let mut v = vm(p);
    let t = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let d = task_finish(&mut v, t);
    assert_eq!(v.delivery_value(d).unwrap().as_int(), Some(1));
    assert_eq!(v.delivery_fault(d).unwrap(), None);
}
#[test]
fn caught_errors_inside_finally_preserve_pending_return_and_throw() {
    for pending_throw in [false, true] {
        for fault in 0..4 {
            let mut v = vm(inner_fault_in_finally(pending_throw, fault));
            let factory = v
                .start(FunctionId(2), &[], OWNER, Context::Ordinary)
                .unwrap();
            let d = task_finish(&mut v, factory);
            let closure = v.delivery_value(d).unwrap();
            let root = v
                .heap_mut()
                .add_root(OWNER, RootKind::Host, closure)
                .unwrap();
            v.discard_delivery(d).unwrap();
            let task = v
                .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
                .unwrap();
            let mut entries = 0;
            let mut callbacks = 0;
            let delivery = loop {
                match task_slice(&mut v, task) {
                    RunOutcome::BudgetYield(_) => {}
                    RunOutcome::HostCall(t) => match t.input {
                        NativeInput::Entry => {
                            entries += 1;
                            if fault == 3 {
                                v.respond(
                                    t,
                                    HostResponse::CallScript {
                                        closure,
                                        arguments: &[],
                                        continuation: Continuation {
                                            faults: FaultRetention::Preserve,
                                            version: 1,
                                            stage: 73,
                                            values: &[],
                                        },
                                    },
                                )
                                .unwrap();
                            } else {
                                v.respond(t, HostResponse::Error(NewFault::thrown(Value::int(1))))
                                    .unwrap();
                            }
                        }
                        NativeInput::CallbackFault => {
                            callbacks += 1;
                            assert_eq!(
                                t.continuation,
                                Some(ContinuationState {
                                    version: 1,
                                    stage: 73
                                })
                            );
                            let info = v.host_callback_fault(t).unwrap().unwrap();
                            assert_eq!(info.location.code.function, FunctionId(3));
                            let _value = v.host_callback_value(t).unwrap();
                            v.respond(
                                t,
                                HostResponse::ForwardFault(
                                    v.host_callback_fault(t).unwrap().unwrap().id,
                                ),
                            )
                            .unwrap();
                        }
                        o => panic!("{o:?}"),
                    },
                    RunOutcome::Returned(d) if !pending_throw => break d,
                    RunOutcome::Fault(d) if pending_throw => break d,
                    o => panic!("pending={pending_throw}, fault={fault}: {o:?}"),
                }
            };
            assert_eq!(v.delivery_value(delivery).unwrap().as_int(), Some(41));
            assert_eq!(v.stats().unwinds, 0);
            if pending_throw {
                let info = v.delivery_fault(delivery).unwrap().unwrap();
                assert_eq!(info.kind, FaultKind::Thrown);
                assert_instruction(&v, info.location, 1);
                assert!(
                    v.heap()
                        .fault_causes(info.id)
                        .unwrap()
                        .next(v.heap())
                        .unwrap()
                        .is_none()
                );
            } else {
                assert_eq!(v.delivery_fault(delivery).unwrap(), None);
            }
            assert_eq!(entries, if fault >= 2 { 1 } else { 0 });
            assert_eq!(callbacks, if fault == 3 { 1 } else { 0 });
            v.heap_mut().release_root(root).unwrap();
        }
    }
}
#[test]
fn escaping_finally_error_preserves_pending_value_as_rooted_cause() {
    let mut p = inner_fault_in_finally(true, 2);
    p.functions[0].regions.truncate(1);
    for pc in [5, 7] {
        p.functions[0].code[pc] = Instruction::new(Opcode::Jump, 8, 0, 0);
    }
    let mut v = vm(p);
    let task = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let t = task_ticket(&mut v, task);
    let exception = v.heap_mut().alloc_string("cleanup failed").unwrap();
    v.respond(t, HostResponse::Error(NewFault::thrown(exception)))
        .unwrap();
    let d = task_finish(&mut v, task);
    collect(&mut v);
    let f = v.delivery_fault(d).unwrap().unwrap();
    assert_instruction(&v, f.location, 4);
    assert!(
        v.heap()
            .fault_causes(f.id)
            .unwrap()
            .next(v.heap())
            .unwrap()
            .is_some()
    );
    assert_eq!(v.delivery_cause(d).unwrap().unwrap().as_int(), Some(41));
    assert!(v.delivery_value(d).unwrap().same_bits(exception));
}
#[test]
fn preparation_faults_route_to_caller_without_entering_callee_handlers_or_replaying() {
    for missing in [false, true] {
        for caller_catch in [false, true] {
            let mut v = vm(preparation_package(missing, caller_catch));
            let t = v
                .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
                .unwrap();
            let d = task_finish(&mut v, t);
            if caller_catch {
                assert_eq!(v.delivery_fault(d).unwrap(), None);
                assert_eq!(v.delivery_value(d).unwrap().as_int(), Some(1));
            } else {
                let f = v.delivery_fault(d).unwrap().unwrap();
                assert_eq!(
                    f.kind,
                    if missing {
                        FaultKind::MissingArgument
                    } else {
                        FaultKind::Uninitialized
                    }
                );
                assert_eq!(f.location.code.function, FunctionId(0));
                assert_instruction(&v, f.location, 1);
            }
            assert_eq!(v.stats().native_entries, 0);
            assert_eq!(v.stats().frames, 0);
        }
    }
}
#[test]
fn resource_failures_during_default_and_callback_preparation_are_consumed_faults() {
    let p = fixtures::captured_default();
    let registry = support(&p);
    let mut v = Vm::load(
        verify(p, Limits::default()).unwrap(),
        registry,
        VmLimits {
            max_frames: 1,
            ..VmLimits::default()
        },
        SessionEpoch(4),
    )
    .unwrap();
    let factory = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let d = task_finish(&mut v, factory);
    let closure = v.delivery_value(d).unwrap();
    let root = v
        .heap_mut()
        .add_root(OWNER, RootKind::Host, closure)
        .unwrap();
    v.discard_delivery(d).unwrap();
    let t = v
        .start_closure(closure, &[], OWNER, Context::Ordinary)
        .unwrap();
    let d = task_finish(&mut v, t);
    assert_eq!(
        v.delivery_fault(d).unwrap().unwrap().kind,
        FaultKind::Resource
    );
    assert_eq!(v.stats().frames, 0);
    v.heap_mut().release_root(root).unwrap();
    let p = with_callback(fixtures::helper_native(), false);
    let registry = support(&p);
    let mut v = Vm::load(
        verify(p, Limits::default()).unwrap(),
        registry,
        VmLimits {
            max_frames: 3,
            ..VmLimits::default()
        },
        SessionEpoch(4),
    )
    .unwrap();
    let f = v
        .start(FunctionId(2), &[], OWNER, Context::Ordinary)
        .unwrap();
    let d = task_finish(&mut v, f);
    let closure = v.delivery_value(d).unwrap();
    let root = v
        .heap_mut()
        .add_root(OWNER, RootKind::Host, closure)
        .unwrap();
    v.discard_delivery(d).unwrap();
    let task = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let ticket = task_ticket(&mut v, task);
    v.respond(
        ticket,
        HostResponse::CallScript {
            closure,
            arguments: &[],
            continuation: Continuation {
                faults: FaultRetention::Preserve,
                version: 1,
                stage: 99,
                values: &[],
            },
        },
    )
    .unwrap();
    let resume = task_ticket(&mut v, task);
    assert_eq!(resume.input, NativeInput::CallbackFault);
    assert_eq!(resume.continuation.unwrap().stage, 99);
    let fault = v.host_callback_fault(resume).unwrap().unwrap();
    assert_eq!(fault.kind, FaultKind::Resource);
    assert_eq!(fault.location, ticket.location);
    let _value = v.host_callback_value(resume).unwrap();
    v.respond(
        resume,
        HostResponse::ForwardFault(v.host_callback_fault(resume).unwrap().unwrap().id),
    )
    .unwrap();
    let d = task_finish(&mut v, task);
    assert_eq!(
        v.delivery_fault(d).unwrap().unwrap().kind,
        FaultKind::Resource
    );
    assert_eq!(v.stats().native_entries, 1);
    v.heap_mut().release_root(root).unwrap();
}
#[test]
fn rethrow_keeps_original_fault_source_through_nested_catches_and_gc() {
    let mut p = fixtures::exception_regions();
    let f = &mut p.functions[0];
    let i = Instruction::new;
    f.source_map.clear();
    f.register_count = 2;
    f.code = vec![
        i(Opcode::LoadConst, 0, 1, 0),
        i(Opcode::Throw, 0, 0, 0),
        i(Opcode::Rethrow, 0, 0, 0),
        i(Opcode::Rethrow, 0, 0, 0),
    ];
    f.regions = vec![
        ExceptionRegion {
            protected: PcRange {
                start: Pc(0),
                end: Pc(2),
            },
            catch: Some(CatchRegion {
                range: PcRange {
                    start: Pc(2),
                    end: Pc(3),
                },
                exception: Reg(1),
            }),
            finally: None,
        },
        ExceptionRegion {
            protected: PcRange {
                start: Pc(0),
                end: Pc(3),
            },
            catch: Some(CatchRegion {
                range: PcRange {
                    start: Pc(3),
                    end: Pc(4),
                },
                exception: Reg(1),
            }),
            finally: None,
        },
    ];
    let mut v = vm(p);
    let t = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let d = task_finish(&mut v, t);
    assert_eq!(v.delivery_value(d).unwrap().as_int(), Some(41));
    assert_instruction(&v, v.delivery_fault(d).unwrap().unwrap().location, 1);
}
#[test]
fn shared_cleanup_selects_full_lexical_parents_for_return_and_leave_inside_catch_and_finally() {
    for in_finally in [false, true] {
        for leave in [false, true] {
            // The language/verifier forbids Return from finally; Leave still
            // exercises a cleanup nested in that lexical block.
            if in_finally && !leave {
                continue;
            }
            let mut v = vm(normal_cleanup_in_handler(in_finally, leave));
            let task = v
                .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
                .unwrap();
            let d = task_finish(&mut v, task);
            assert_eq!(v.delivery_fault(d).unwrap(), None);
            assert_eq!(
                v.delivery_value(d).unwrap().as_int(),
                Some(if in_finally && leave { 1 } else { 41 })
            );
            assert_eq!(v.stats().unwinds, 0);
            v.discard_delivery(d).unwrap();
            let task = v
                .start(FunctionId(1), &[], OWNER, Context::Ordinary)
                .unwrap();
            let d = task_finish(&mut v, task);
            assert_eq!(
                v.delivery_value(d).unwrap().as_int(),
                Some(if in_finally && !leave { 1 } else { 2 })
            );
        }
    }
}

#[test]
fn exhausted_unwind_roots_preserve_the_complete_fault_until_hard_teardown() {
    let p = finally_program(true);
    let registry = support(&p);
    let mut limits = VmLimits::default();
    // Four permanent pooled/module ranges plus five activation/unwind ranges.
    limits.heap.max_root_ranges = 9;
    let mut v = Vm::load(
        verify(p, Limits::default()).unwrap(),
        registry,
        limits,
        SessionEpoch(4),
    )
    .unwrap();
    let t = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let blocked = loop {
        match task_slice(&mut v, t) {
            RunOutcome::BudgetYield(_) => {}
            RunOutcome::ResourceBlocked(b) => break b,
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(blocked.resource, BlockedResource::RootRanges);
    assert_eq!(v.stats().unwinds, 1);
    v.cancel_task(t).unwrap();
    while !matches!(
        v.run_slice(t, SliceBudget::default(), || 0),
        Err(VmError::Idle)
    ) {}
    assert_eq!(v.stats().slots.active_ranges, v.pooled_root_ranges());
}

#[test]
fn explicit_throw_in_catch_sets_new_source_and_retains_original_value_as_cause() {
    let mut p = fixtures::exception_regions();
    let i = Instruction::new;
    let f = &mut p.functions[0];
    f.source_map.clear();
    f.register_count = 2;
    f.code = vec![
        i(Opcode::LoadConst, 0, 1, 0),
        i(Opcode::Throw, 0, 0, 0),
        i(Opcode::LoadConst, 0, 2, 0),
        i(Opcode::Throw, 0, 0, 0),
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
    let mut v = vm(p);
    let t = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let d = task_finish(&mut v, t);
    collect(&mut v);
    let f = v.delivery_fault(d).unwrap().unwrap();
    assert_instruction(&v, f.location, 3);
    assert!(
        v.heap()
            .fault_causes(f.id)
            .unwrap()
            .next(v.heap())
            .unwrap()
            .is_some()
    );
    assert_eq!(v.delivery_value(d).unwrap().as_int(), Some(1));
    assert_eq!(v.delivery_cause(d).unwrap().unwrap().as_int(), Some(41));
}

#[test]
fn sequential_catches_reuse_one_activation_and_helpers_observe_only_the_active_chain() {
    let mut p = fixtures::helper_native();
    p.constants.extend([Const::Int(0), Const::Int(1000)]);
    let i = Instruction::new;
    let f = &mut p.functions[0];
    f.source_map.clear();
    f.register_count = 5;
    f.code = vec![
        i(Opcode::LoadConst, 2, 3, 0),
        i(Opcode::LoadConst, 3, 4, 0),
        i(Opcode::LoadConst, 4, 2, 0),
        i(Opcode::LoadConst, 0, 1, 0),
        i(Opcode::Throw, 0, 0, 0),
        i(Opcode::Call, 1, 0, 0),
        i(Opcode::Leave, 7, 0, 0),
        i(Opcode::Add, 2, 2, 4),
        i(Opcode::Lt, 0, 2, 3),
        i(Opcode::BranchBool, 0, 3, 10),
        i(Opcode::Return, 2, 0, 0),
    ];
    let Descriptor::Call(call) = &mut f.descriptors[0] else {
        panic!()
    };
    call.arguments = Arguments::Window(RegWindow {
        start: Reg(1),
        len: 1,
    });
    f.regions = vec![ExceptionRegion {
        protected: PcRange {
            start: Pc(3),
            end: Pc(5),
        },
        catch: Some(CatchRegion {
            range: PcRange {
                start: Pc(5),
                end: Pc(7),
            },
            exception: Reg(1),
        }),
        finally: None,
    }];
    let registry = support(&p);
    let mut v = Vm::load(
        verify(p, Limits::default()).unwrap(),
        registry,
        VmLimits {
            max_unwinds: 1,
            ..VmLimits::default()
        },
        SessionEpoch(4),
    )
    .unwrap();
    let task = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let mut calls = 0;
    let mut last = None;
    let d = loop {
        match task_slice(&mut v, task) {
            RunOutcome::BudgetYield(_) => {}
            RunOutcome::HostCall(t) => {
                calls += 1;
                let info = v.error_info(task).unwrap().unwrap();
                assert_instruction(&v, info.location, 4);
                assert_eq!(info.location.code.function, FunctionId(0));
                assert_ne!(last, Some(info.id));
                last = Some(info.id);
                v.respond(t, HostResponse::Ready(Value::int(42))).unwrap();
                if calls % 50 == 0 {
                    collect(&mut v);
                }
            }
            RunOutcome::Returned(d) => break d,
            RunOutcome::ResourceBlocked(_) => collect(&mut v),
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(calls, 1000);
    assert_eq!(v.delivery_value(d).unwrap().as_int(), Some(1000));
    assert_eq!(v.error_info(task).unwrap(), None);
    collect(&mut v);
    assert!(v.heap().fault_info(last.unwrap()).is_err());
}

#[test]
fn cooperative_cancel_is_a_complete_catchable_core_error_and_wait_ticket_is_revoked() {
    let mut p = fixtures::helper_native();
    let f = &mut p.functions[1];
    let i = Instruction::new;
    f.source_map.clear();
    f.code = vec![
        i(Opcode::Call, 1, 0, 0),
        i(Opcode::Leave, 4, 0, 0),
        i(Opcode::LoadConst, 1, 2, 0),
        i(Opcode::Return, 1, 0, 0),
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
    let mut v = vm(p);
    let task = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let t = task_ticket(&mut v, task);
    v.respond(t, HostResponse::Pending(t.wait_id())).unwrap();
    v.cancel_cooperatively(task).unwrap();
    let d = task_finish(&mut v, task);
    assert_eq!(v.delivery_fault(d).unwrap(), None);
    assert_eq!(v.delivery_value(d).unwrap().as_int(), Some(1));
    assert!(matches!(
        v.complete(t.wait_id(), Completion::Ready(Value::int(42))),
        Err(VmError::InvalidWait)
    ));
    let mut v = vm(fixtures::helper_native());
    let task = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let t = task_ticket(&mut v, task);
    v.respond(t, HostResponse::Pending(t.wait_id())).unwrap();
    v.cancel_cooperatively(task).unwrap();
    let d = task_finish(&mut v, task);
    let info = v.delivery_fault(d).unwrap().unwrap();
    let value = v.delivery_value(d).unwrap();
    assert_eq!(info.kind, FaultKind::Cancelled);
    let error = v.heap().core_error_info(value).unwrap();
    assert_eq!(error.code, info.code);
    assert_eq!(error.message, "cooperative cancellation");
    assert_eq!(error.details.tag(), ValueTag::Null);
}

#[test]
fn native_fault_capacity_retains_pending_source_and_resumes_without_repeating_host_actions() {
    let p = with_callback(fixtures::helper_native(), true);
    let registry = support(&p);
    let mut v = Vm::load(
        verify(p, Limits::default()).unwrap(),
        registry,
        VmLimits {
            max_native_faults: 1,
            ..VmLimits::default()
        },
        SessionEpoch(4),
    )
    .unwrap();
    let factory = v
        .start(FunctionId(2), &[], OWNER, Context::Ordinary)
        .unwrap();
    let d = task_finish(&mut v, factory);
    let closure = v.delivery_value(d).unwrap();
    let root = v
        .heap_mut()
        .add_root(OWNER, RootKind::Host, closure)
        .unwrap();
    v.discard_delivery(d).unwrap();
    let task = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let mut first = None;
    let mut second = None;
    let mut final_fault = None;
    let mut entries = 0;
    let mut waits = 0;
    let mut callbacks = 0;
    let mut blocks = 0;
    let d = loop {
        match task_slice(&mut v, task) {
            RunOutcome::BudgetYield(_) => {}
            RunOutcome::HostCall(t) => {
                let stage = t.continuation.map_or(0, |c| c.stage);
                match (t.input, stage) {
                    (NativeInput::Entry, 0) => {
                        entries += 1;
                        v.respond(
                            t,
                            HostResponse::CallScript {
                                closure,
                                arguments: &[],
                                continuation: continue_at(1, FaultRetention::Preserve),
                            },
                        )
                        .unwrap();
                    }
                    (NativeInput::CallbackFault, 1) => {
                        callbacks += 1;
                        first = Some(v.host_callback_fault(t).unwrap().unwrap().id);
                        v.respond(
                            t,
                            HostResponse::Continue(continue_at(2, FaultRetention::Preserve)),
                        )
                        .unwrap();
                    }
                    (NativeInput::WaitingFault, 2) => {
                        assert_eq!(v.host_resume_value(t).unwrap().as_int(), Some(41));
                        // Forwarding an already retained fault needs no extra fault slot.
                        assert_eq!(
                            v.host_callback_fault(t).unwrap().unwrap().id,
                            first.unwrap()
                        );
                        assert_eq!(v.host_faults(t).unwrap().len(), 1);
                        v.respond(
                            t,
                            HostResponse::Continue(continue_at(3, FaultRetention::Preserve)),
                        )
                        .unwrap();
                    }
                    (NativeInput::Continue { .. }, 3) => v
                        .respond(
                            t,
                            HostResponse::CallScript {
                                closure,
                                arguments: &[],
                                continuation: continue_at(4, FaultRetention::Preserve),
                            },
                        )
                        .unwrap(),
                    (NativeInput::CallbackFault, 4) => {
                        callbacks += 1;
                        second = Some(v.host_callback_fault(t).unwrap().unwrap().id);
                        assert_ne!(second, first);
                        v.respond(
                            t,
                            HostResponse::Continue(continue_at(5, FaultRetention::Preserve)),
                        )
                        .unwrap();
                    }
                    (NativeInput::Continue { .. }, 2 | 5) => {
                        v.respond(t, HostResponse::Pending(t.wait_id())).unwrap()
                    }
                    (NativeInput::WaitingFault, 5) => {
                        let id = v.host_callback_fault(t).unwrap().unwrap().id;
                        final_fault = Some(id);
                        assert_ne!(Some(id), second);
                        let cause = v
                            .heap()
                            .fault_causes(id)
                            .unwrap()
                            .next(v.heap())
                            .unwrap()
                            .unwrap();
                        assert_eq!(cause.kind, CauseKind::ExplicitCause);
                        assert_eq!(cause.fault, second);
                        v.respond(
                            t,
                            HostResponse::Continue(continue_at(6, FaultRetention::Preserve)),
                        )
                        .unwrap();
                    }
                    (NativeInput::Continue { .. }, 6) => v
                        .respond(t, HostResponse::ForwardFault(final_fault.unwrap()))
                        .unwrap(),
                    other => panic!("{other:?}"),
                }
            }
            RunOutcome::Waiting(w) => {
                waits += 1;
                let result = if waits == 1 {
                    Completion::ForwardFault(first.unwrap())
                } else {
                    Completion::Error(NewFault {
                        value: Value::int(41),
                        code: FaultCode::core(FaultKind::Thrown),
                        cause: second,
                    })
                };
                v.complete(w, result).unwrap();
                assert!(matches!(
                    v.complete(w, Completion::Ready(Value::int(42))),
                    Err(VmError::InvalidWait)
                ));
            }
            RunOutcome::ResourceBlocked(b) => {
                blocks += 1;
                assert_eq!((b.current, b.required), (1, 2));
                let BlockedResource::NativeFaultSlots { frame } = b.resource else {
                    panic!("{b:?}")
                };
                assert_eq!(b.task, task);
                collect(&mut v);
                v.replace_native_faults(task, frame, &[]).unwrap();
                collect(&mut v);
                if blocks == 1 {
                    assert!(v.heap().fault_info(first.unwrap()).is_err());
                } else {
                    // The partially delivered new fault still roots its explicit cause.
                    assert!(v.heap().fault_info(second.unwrap()).is_ok());
                }
            }
            RunOutcome::Fault(d) => break d,
            other => panic!("{other:?}"),
        }
    };
    assert_eq!((entries, callbacks, waits, blocks), (1, 2, 2, 2));
    assert_eq!(
        v.delivery_fault(d).unwrap().unwrap().id,
        final_fault.unwrap()
    );
    assert_eq!(v.delivery_value(d).unwrap().as_int(), Some(41));
    v.discard_delivery(d).unwrap();
    v.heap_mut().release_root(root).unwrap();
    collect(&mut v);
    assert!(v.heap().fault_info(final_fault.unwrap()).is_err());
    assert!(v.heap().fault_info(second.unwrap()).is_err());
}

#[test]
fn continued_wait_delivery_gets_its_own_fault_slot_before_continuation_values_are_replaced() {
    let mut v = vm(with_callback(fixtures::helper_native(), true));
    let factory = v
        .start(FunctionId(2), &[], OWNER, Context::Ordinary)
        .unwrap();
    let d = task_finish(&mut v, factory);
    let closure = v.delivery_value(d).unwrap();
    let root = v
        .heap_mut()
        .add_root(OWNER, RootKind::Host, closure)
        .unwrap();
    v.discard_delivery(d).unwrap();
    let task = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let entry = task_ticket(&mut v, task);
    v.respond(
        entry,
        HostResponse::CallScript {
            closure,
            arguments: &[],
            continuation: continue_at(1, FaultRetention::Preserve),
        },
    )
    .unwrap();
    let callback = task_ticket(&mut v, task);
    let id = v.host_callback_fault(callback).unwrap().unwrap().id;
    v.respond(
        callback,
        HostResponse::Continue(Continuation {
            values: &[id.as_value()],
            faults: FaultRetention::Replace(&[]),
            ..continue_at(2, FaultRetention::Preserve)
        }),
    )
    .unwrap();
    let cont = task_ticket(&mut v, task);
    assert!(v.host_faults(cont).unwrap().is_empty());
    assert_eq!(v.host_continuation(cont).unwrap().len(), 1);
    assert_eq!(
        v.heap()
            .fault_id(v.host_continuation(cont).unwrap()[0])
            .unwrap(),
        id
    );
    v.respond(cont, HostResponse::Pending(cont.wait_id()))
        .unwrap();
    v.complete(cont.wait_id(), Completion::ForwardFault(id))
        .unwrap();
    let wait = task_ticket(&mut v, task);
    assert_eq!(wait.input, NativeInput::WaitingFault);
    assert_eq!(v.host_resume_value(wait).unwrap().as_int(), Some(41));
    assert_eq!(v.host_faults(wait).unwrap().len(), 1);
    assert_eq!(
        v.heap().fault_id(v.host_faults(wait).unwrap()[0]).unwrap(),
        id
    );
    v.respond(
        wait,
        HostResponse::Continue(continue_at(3, FaultRetention::Preserve)),
    )
    .unwrap();
    let cont = task_ticket(&mut v, task);
    assert!(v.host_continuation(cont).unwrap().is_empty());
    collect(&mut v);
    assert_eq!(v.host_callback_fault(cont).unwrap().unwrap().id, id);
    assert_eq!(v.heap().fault_value(id).unwrap().as_int(), Some(41));
    v.respond(cont, HostResponse::ForwardFault(id)).unwrap();
    let d = task_finish(&mut v, task);
    assert_eq!(v.delivery_fault(d).unwrap().unwrap().id, id);
    assert_eq!(v.stats().native_entries, 1);
    v.discard_delivery(d).unwrap();
    v.heap_mut().release_root(root).unwrap();
    collect(&mut v);
    assert!(v.heap().fault_info(id).is_err());
}

#[test]
fn cooperative_cancel_aborts_deep_equality_and_cleanup_without_retaining_work_roots() {
    for caught in [false, true] {
        for cleanup in [false, true] {
            let mut p = binary_program(Opcode::Eq);
            if caught {
                let f = &mut p.functions[0];
                f.register_count = 4;
                f.code[1] = Instruction::new(Opcode::Leave, 3, 0, 0);
                f.code.push(Instruction::new(Opcode::Return, 3, 0, 0));
                f.code.push(Instruction::new(Opcode::Return, 2, 0, 0));
                f.regions = vec![ExceptionRegion {
                    protected: PcRange {
                        start: Pc(0),
                        end: Pc(2),
                    },
                    catch: Some(CatchRegion {
                        range: PcRange {
                            start: Pc(2),
                            end: Pc(3),
                        },
                        exception: Reg(3),
                    }),
                    finally: None,
                }];
            }
            let mut v = vm(p);
            let mut a = v.heap_mut().alloc_string("a").unwrap();
            let mut b = v.heap_mut().alloc_string("b").unwrap();
            let leaf = a;
            for _ in 0..12 {
                a = v.heap_mut().alloc_tuple(&[a]).unwrap();
                b = v.heap_mut().alloc_tuple(&[b]).unwrap();
            }
            let task = v
                .start(fixtures::ENTRY, &[a, b], OWNER, Context::Ordinary)
                .unwrap();
            let mut work = 1;
            loop {
                let out = v
                    .run_slice(
                        task,
                        SliceBudget {
                            instructions: 1,
                            work,
                            ..SliceBudget::default()
                        },
                        || 0,
                    )
                    .unwrap();
                gc(&mut v);
                assert!(matches!(out, RunOutcome::BudgetYield(_)));
                if v.stats().comparison_depth == 13 {
                    break;
                }
                let RunOutcome::BudgetYield(u) = out else {
                    unreachable!()
                };
                work = u.required_work.max(1);
            }
            if cleanup {
                let out = v
                    .run_slice(
                        task,
                        SliceBudget {
                            instructions: 1,
                            work: 1,
                            ..SliceBudget::default()
                        },
                        || 0,
                    )
                    .unwrap();
                assert!(matches!(out, RunOutcome::BudgetYield(_)));
                assert_eq!(v.stats().comparison_depth, 13);
            }
            v.cancel_cooperatively(task).unwrap();
            let mut work = 1;
            let mut allocation_bytes = 0;
            let d = loop {
                let out = v
                    .run_slice(
                        task,
                        SliceBudget {
                            instructions: 1,
                            work,
                            allocation_bytes,
                            ..SliceBudget::default()
                        },
                        || 0,
                    )
                    .unwrap();
                gc(&mut v);
                match out {
                    RunOutcome::BudgetYield(u) => {
                        work = u.required_work.max(1);
                        allocation_bytes = u.required_allocation;
                    }
                    RunOutcome::Returned(d) if caught => break d,
                    RunOutcome::Fault(d) if !caught => break d,
                    other => panic!("{other:?}"),
                }
            };
            assert_eq!(
                v.heap()
                    .core_error_info(v.delivery_value(d).unwrap())
                    .unwrap()
                    .code
                    .kind,
                FaultKind::Cancelled
            );
            assert_eq!(v.stats().comparison_depth, 0);
            v.discard_delivery(d).unwrap();
            collect(&mut v);
            assert!(v.heap().info(a).is_err());
            assert!(v.heap().info(b).is_err());
            assert!(v.heap().info(leaf).is_err());
            assert!(v.is_idle());
        }
    }
}

#[test]
fn exact_b01_parameter_cell_fixture_shares_default_closure_and_body_but_ordinary_calls_are_independent()
 {
    let mut v = vm(fixtures::parameter_cell_default());
    let t = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let mut work = 1;
    let mut allocation_bytes = 0;
    let d = loop {
        match v
            .run_slice(
                t,
                SliceBudget {
                    instructions: 1,
                    work,
                    allocation_bytes,
                    ..SliceBudget::default()
                },
                || 0,
            )
            .unwrap()
        {
            RunOutcome::BudgetYield(u) => {
                work = u.required_work.max(1);
                allocation_bytes = u.required_allocation;
                gc(&mut v);
            }
            RunOutcome::Returned(d) => break d,
            other => panic!("{other:?}"),
        }
    };
    collect(&mut v);
    let result = v.delivery_value(d).unwrap();
    let owner_result = v.heap().get_field(result, 0).unwrap();
    let owner_closure = v.heap().get_field(owner_result, 1).unwrap();
    let ordinary_closure = v.heap().get_field(result, 1).unwrap();
    assert_eq!(
        v.heap().get_field(owner_result, 0).unwrap().as_int(),
        Some(43)
    );
    let owner_cell = v.heap().get_field(owner_closure, 3).unwrap();
    let ordinary_cell = v.heap().get_field(ordinary_closure, 3).unwrap();
    assert!(!owner_cell.same_bits(ordinary_cell));
    assert_eq!(v.heap().cell_read(owner_cell).unwrap().as_int(), Some(43));
    assert_eq!(
        v.heap().cell_read(ordinary_cell).unwrap().as_int(),
        Some(42)
    );
    // Two parameter Cells, two closures and two tuples; the prefix call adds no Cell.
    assert_eq!(v.heap().stats().live_objects, 6);
    for (closure, expected) in [(owner_closure, 43), (ordinary_closure, 42)] {
        let t = v
            .start_closure(closure, &[], OWNER, Context::Ordinary)
            .unwrap();
        let r = task_finish(&mut v, t);
        assert_eq!(v.delivery_value(r).unwrap().as_int(), Some(expected));
        v.discard_delivery(r).unwrap();
    }
    v.heap_mut().cell_write(owner_cell, Value::int(99)).unwrap();
    let t = v
        .start_closure(owner_closure, &[], OWNER, Context::Ordinary)
        .unwrap();
    let r = task_finish(&mut v, t);
    assert_eq!(v.delivery_value(r).unwrap().as_int(), Some(99));
    assert_eq!(
        v.heap().cell_read(ordinary_cell).unwrap().as_int(),
        Some(42)
    );
    v.discard_delivery(r).unwrap();
    v.discard_delivery(d).unwrap();
    collect(&mut v);
    assert!(v.heap().cell_info(owner_cell).is_err());
    assert!(v.heap().cell_info(ordinary_cell).is_err());
}

#[test]
fn readonly_value_native_defaults_use_no_cell_environment_and_allow_ready_in_nowait() {
    let mut p = fixtures::native_parameter_defaults();
    p.signatures[1].parameters[0].mutable = false;
    p.signatures[2].parameters[0].mutable = false;
    p.signatures[3].parameters[0].mutable = false;
    p.signatures[2].effects = Effects(0);
    p.signatures[3].effects = Effects(Effects::ALLOCATE);
    p.functions[1].bindings[0].mutable = false;
    p.functions[1].bindings[0].storage = BindingStorage::Value(Reg(0));
    p.functions[1].code = vec![
        Instruction::new(Opcode::LoadConst, 1, 1, 0),
        Instruction::new(Opcode::Return, 1, 0, 0),
    ];
    p.functions[2].bindings[0].mutable = false;
    p.functions[2].bindings[0].storage = BindingStorage::Value(Reg(0));
    p.functions[2].bindings[1].storage = BindingStorage::Value(Reg(1));
    p.functions[2].code = vec![
        Instruction::new(Opcode::MakeClosure, 2, 0, 0),
        Instruction::new(Opcode::Call, 3, 1, 0),
        Instruction::new(Opcode::Return, 3, 0, 0),
    ];
    p.functions[3].captures = vec![CaptureKind::Value];
    p.functions[3].code = vec![
        Instruction::new(Opcode::CaptureRead, 0, 0, 0),
        Instruction::new(Opcode::Return, 0, 0, 0),
    ];
    let mut registry = native_default_registry(false);
    registry.natives[0].signature.parameters[0].mutable = false;
    let program = verify(p, Limits::default()).unwrap();
    assert!(!program.native_parameters_need_cells(NativeId(0)).unwrap());
    let mut v = Vm::load(program, registry, VmLimits::default(), SessionEpoch(4)).unwrap();
    let task = v
        .start(fixtures::ENTRY, &[], OWNER, Context::NoWait)
        .unwrap();
    let t = task_ticket(&mut v, task);
    collect(&mut v);
    assert_eq!(
        v.stats().slots.active_ranges,
        v.pooled_root_ranges() + 4 + 3
    );
    assert_eq!(v.heap().stats().live_objects, 1); // Only the native's empty rest array.
    for value in &v.host_arguments(t).unwrap()[..3] {
        assert_eq!(value.as_int(), Some(41));
    }
    v.respond(t, HostResponse::Ready(Value::NULL)).unwrap();
    let d = task_finish(&mut v, task);
    assert_eq!(v.delivery_value(d).unwrap().tag(), ValueTag::Null);
    assert_eq!(v.stats().native_entries, 1);
    v.discard_delivery(d).unwrap();
    collect(&mut v);
    assert_eq!(v.heap().stats().live_objects, 0);
}
#[test]
fn exact_b01_native_default_fixture_prepares_cells_defaults_and_author_values_before_one_entry() {
    for via_value in [false, true] {
        for pending in [false, true] {
            let mut v = native_default_vm(
                if via_value {
                    through_native_value(fixtures::native_parameter_defaults())
                } else {
                    fixtures::native_parameter_defaults()
                },
                false,
            );
            let t = v
                .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
                .unwrap();
            let mut work = 1;
            let mut allocation_bytes = 0;
            let mut entries = 0;
            let d = loop {
                let out = v
                    .run_slice(
                        t,
                        SliceBudget {
                            instructions: 1,
                            work,
                            allocation_bytes,
                            ..SliceBudget::default()
                        },
                        || 0,
                    )
                    .unwrap();
                gc(&mut v);
                match out {
                    RunOutcome::BudgetYield(u) => {
                        work = u.required_work.max(1);
                        allocation_bytes = u.required_allocation;
                    }
                    RunOutcome::HostCall(ticket) => {
                        entries += 1;
                        assert_eq!(ticket.input, NativeInput::Entry);
                        assert_eq!(v.host_arguments(ticket).unwrap().len(), 4);
                        assert_eq!(v.host_arguments(ticket).unwrap()[0].as_int(), Some(43));
                        assert_eq!(v.host_arguments(ticket).unwrap()[1].as_int(), Some(42));
                        assert_eq!(v.host_arguments(ticket).unwrap()[2].as_int(), Some(42));
                        let rest = v.host_arguments(ticket).unwrap()[3];
                        assert_eq!(v.heap().info(rest).unwrap().kind, ObjectKind::Array);
                        assert_eq!(v.heap().info(rest).unwrap().len, 0);
                        v.respond(
                            ticket,
                            if pending {
                                HostResponse::Pending(ticket.wait_id())
                            } else {
                                HostResponse::Ready(Value::NULL)
                            },
                        )
                        .unwrap();
                    }
                    RunOutcome::Waiting(w) => {
                        collect(&mut v);
                        v.complete(w, Completion::Ready(Value::NULL)).unwrap();
                    }
                    RunOutcome::Returned(d) => break d,
                    other => panic!("{other:?}"),
                }
            };
            assert_eq!(entries, 1);
            assert_eq!(v.stats().native_entries, 1);
            assert_eq!(v.delivery_value(d).unwrap().tag(), ValueTag::Null);
            v.discard_delivery(d).unwrap();
            collect(&mut v);
            assert_eq!(v.stats().slots.active_ranges, v.pooled_root_ranges());
            assert_eq!(v.heap().stats().live_objects, u32::from(via_value));
        }
    }
}
#[test]
fn native_arguments_are_reprojected_after_default_closure_callbacks_and_continued_wait() {
    for via_value in [false, true] {
        let mut v = native_default_vm(
            if via_value {
                through_native_value(native_closure_defaults())
            } else {
                native_closure_defaults()
            },
            true,
        );
        let task = v
            .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
            .unwrap();
        let mut closure = None;
        let mut cell = None;
        let mut callbacks = 0;
        let d = loop {
            match task_slice(&mut v, task) {
                RunOutcome::BudgetYield(_) => {}
                RunOutcome::HostCall(t) => {
                    let stage = t.continuation.map_or(0, |c| c.stage);
                    match (t.input, stage) {
                        (NativeInput::Entry, 0) => {
                            let args = v.host_arguments(t).unwrap();
                            assert_eq!(args[0].as_int(), Some(43));
                            assert!(args[1].same_bits(args[2]));
                            closure = Some(args[1]);
                            cell = Some(v.heap().get_field(args[1], 3).unwrap());
                            v.respond(
                                t,
                                HostResponse::CallScript {
                                    closure: closure.unwrap(),
                                    arguments: &[],
                                    continuation: continue_at(1, FaultRetention::Preserve),
                                },
                            )
                            .unwrap();
                        }
                        (NativeInput::CallbackReturned, 1 | 3) => {
                            callbacks += 1;
                            let expected = if stage == 1 { 44 } else { 45 };
                            assert_eq!(v.host_resume_value(t).unwrap().as_int(), Some(expected));
                            assert_eq!(v.host_arguments(t).unwrap()[0].as_int(), Some(expected));
                            assert_eq!(
                                v.heap().cell_read(cell.unwrap()).unwrap().as_int(),
                                Some(expected)
                            );
                            if stage == 1 {
                                v.respond(
                                    t,
                                    HostResponse::Continue(continue_at(
                                        2,
                                        FaultRetention::Preserve,
                                    )),
                                )
                                .unwrap();
                            } else {
                                v.respond(t, HostResponse::Pending(t.wait_id())).unwrap();
                            }
                        }
                        (NativeInput::Continue { .. }, 2) => {
                            assert_eq!(v.host_arguments(t).unwrap()[0].as_int(), Some(44));
                            v.respond(
                                t,
                                HostResponse::CallScript {
                                    closure: closure.unwrap(),
                                    arguments: &[],
                                    continuation: continue_at(3, FaultRetention::Preserve),
                                },
                            )
                            .unwrap();
                        }
                        (NativeInput::WaitingReturned, 3) => {
                            assert_eq!(v.host_resume_value(t).unwrap().as_int(), Some(99));
                            assert_eq!(v.host_arguments(t).unwrap()[0].as_int(), Some(45));
                            v.respond(t, HostResponse::Ready(Value::NULL)).unwrap();
                        }
                        other => panic!("{other:?}"),
                    }
                    assert!(matches!(v.host_arguments(t), Err(VmError::InvalidTicket)));
                }
                RunOutcome::Waiting(w) => {
                    collect(&mut v);
                    v.complete(w, Completion::Ready(Value::int(99))).unwrap();
                }
                RunOutcome::Returned(d) => break d,
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(callbacks, 2);
        assert_eq!(v.stats().native_entries, 1);
        v.discard_delivery(d).unwrap();
        collect(&mut v);
        assert!(v.heap().cell_info(cell.unwrap()).is_err());
        assert_eq!(v.stats().slots.active_ranges, v.pooled_root_ranges());
    }
}

#[test]
fn reported_retry_grants_finish_helper_returns_under_a_live_catch_independent_of_unused_capacity() {
    let mut measured = Vec::new();
    for (roots, ranges) in [(16, 64), (4096, 4096)] {
        let mut p = fixtures::helper_native();
        let f = &mut p.functions[0];
        f.source_map.clear();
        let i = Instruction::new;
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
        let mut limits = VmLimits::default();
        limits.heap.max_roots = roots;
        limits.heap.max_root_ranges = ranges;
        let mut v = Vm::load(
            verify(p, Limits::default()).unwrap(),
            support(&fixtures::helper_native()),
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
        let mut work = 0;
        let mut delivery = None;
        for _ in 0..1000 {
            let out = v.run_slice(t, quota, || 0).unwrap();
            work += v.stats().usage.work;
            gc(&mut v);
            match out {
                RunOutcome::BudgetYield(u) => {
                    // Grant exactly the reported action, without paying earlier
                    // unsaved work again via u.work + u.required_work.
                    quota.work = u.required_work.max(1);
                    quota.allocation_bytes = u.required_allocation;
                }
                RunOutcome::HostCall(ticket) => {
                    assert!(v.error_info(t).unwrap().is_some());
                    v.respond(ticket, HostResponse::Pending(ticket.wait_id()))
                        .unwrap();
                }
                RunOutcome::Waiting(wait) => {
                    v.complete(wait, Completion::Ready(Value::int(42))).unwrap()
                }
                RunOutcome::Returned(d) => {
                    delivery = Some(d);
                    break;
                }
                other => panic!("{other:?}"),
            }
        }
        let d = delivery.expect("reported grants must finish the live-catch helper return");
        assert_eq!(v.delivery_value(d).unwrap().as_int(), Some(42));
        assert_eq!(v.stats().native_entries, 1);
        assert_eq!(v.stats().slots.active_ranges, v.pooled_root_ranges());
        v.discard_delivery(d).unwrap();
        measured.push(work);
    }
    assert_eq!(
        measured[0], measured[1],
        "unused descriptor capacity is not call work"
    );
}

mod retention;
