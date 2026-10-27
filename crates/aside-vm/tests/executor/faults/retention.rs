//! Complete fault identity and bounded retention.
use super::*;

#[test]
fn full_native_fault_identity_survives_continue_wait_and_repeated_callbacks_then_host_transfer() {
    for mode in 0..3 {
        let mut v = vm(with_callback(fixtures::helper_native(), true));
        let f = v
            .start(FunctionId(2), &[], OWNER, Context::Ordinary)
            .unwrap();
        let d = task_finish(&mut v, f);
        let closure = v.delivery_value(d).unwrap();
        let cr = v
            .heap_mut()
            .add_root(OWNER, RootKind::Host, closure)
            .unwrap();
        v.discard_delivery(d).unwrap();
        let task = v
            .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
            .unwrap();
        let mut original = None;
        let mut pending_ticket = None;
        let mut callback_count = 0;
        let delivery = loop {
            match task_slice(&mut v, task) {
                RunOutcome::BudgetYield(_) => {}
                RunOutcome::HostCall(t) => {
                    let stage = t.continuation.map_or(0, |c| {
                        assert_eq!(c.version, 1);
                        c.stage
                    });
                    match (t.input, stage) {
                        (NativeInput::Entry, 0) => v
                            .respond(
                                t,
                                HostResponse::CallScript {
                                    closure,
                                    arguments: &[],
                                    continuation: continue_at(1, FaultRetention::Preserve),
                                },
                            )
                            .unwrap(),
                        (NativeInput::CallbackFault, 1) => {
                            let info = v.host_callback_fault(t).unwrap().unwrap();
                            original = Some(info);
                            callback_count += 1;
                            assert_eq!(v.heap().fault_value(info.id).unwrap().as_int(), Some(41));
                            v.respond(
                                t,
                                HostResponse::Continue(continue_at(2, FaultRetention::Preserve)),
                            )
                            .unwrap();
                        }
                        (NativeInput::Continue { .. }, 2) => {
                            assert_eq!(
                                v.host_callback_fault(t).unwrap().unwrap().id,
                                original.unwrap().id
                            );
                            pending_ticket = Some(t);
                            v.respond(t, HostResponse::Pending(t.wait_id())).unwrap();
                            assert!(matches!(
                                v.host_resume_value(t),
                                Err(VmError::InvalidTicket)
                            ));
                        }
                        (NativeInput::WaitingReturned, 2) => {
                            assert_eq!(v.host_resume_value(t).unwrap().as_int(), Some(42));
                            assert_eq!(v.host_callback_value(t).unwrap().as_int(), Some(42));
                            assert!(matches!(
                                v.host_resume_value(pending_ticket.unwrap()),
                                Err(VmError::InvalidTicket)
                            ));
                            v.respond(
                                t,
                                HostResponse::CallScript {
                                    closure,
                                    arguments: &[],
                                    continuation: continue_at(3, FaultRetention::Preserve),
                                },
                            )
                            .unwrap();
                        }
                        (NativeInput::CallbackFault, 3) => {
                            callback_count += 1;
                            assert_ne!(
                                v.host_callback_fault(t).unwrap().unwrap().id,
                                original.unwrap().id
                            );
                            let old = original.unwrap().id;
                            v.respond(
                                t,
                                HostResponse::Continue(continue_at(
                                    4,
                                    FaultRetention::Replace(&[old]),
                                )),
                            )
                            .unwrap();
                        }
                        (NativeInput::Continue { .. }, 4) => {
                            let old = original.unwrap().id;
                            assert_eq!(
                                v.heap().fault_id(v.host_faults(t).unwrap()[0]).unwrap(),
                                old
                            );
                            let value = v.heap().fault_value(old).unwrap();
                            v.respond(
                                t,
                                match mode {
                                    0 => HostResponse::ForwardFault(old),
                                    1 => HostResponse::Error(NewFault::thrown(value)),
                                    _ => HostResponse::Error(NewFault {
                                        value,
                                        code: FaultCode::core(FaultKind::Thrown),
                                        cause: Some(old),
                                    }),
                                },
                            )
                            .unwrap();
                        }
                        other => panic!("{other:?}"),
                    }
                }
                RunOutcome::Waiting(wait) => {
                    collect(&mut v);
                    v.complete(wait, Completion::Ready(Value::int(42))).unwrap();
                }
                RunOutcome::Fault(d) => break d,
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(callback_count, 2);
        let info = v.delivery_fault(delivery).unwrap().unwrap();
        let old = original.unwrap();
        assert_eq!(info.id == old.id, mode == 0);
        assert_eq!(
            info.location.code.function,
            if mode == 0 {
                FunctionId(3)
            } else {
                FunctionId(1)
            }
        );
        let value_root = v
            .heap_mut()
            .add_root(RootOwner(44), RootKind::Host, Value::NULL)
            .unwrap();
        v.copy_delivery_value_to_root(delivery, value_root).unwrap();
        assert_eq!(v.delivery_fault(delivery).unwrap().unwrap().id, info.id);
        assert!(matches!(
            v.take_delivery_root(delivery, value_root),
            Err(VmError::Contract(_))
        ));
        let root = v.take_fault_delivery(delivery, RootOwner(45)).unwrap();
        assert!(v.is_idle());
        collect(&mut v);
        assert_eq!(root.info(v.heap()).unwrap().id, info.id);
        assert_eq!(v.heap().fault_value(root.id()).unwrap().as_int(), Some(41));
        assert_eq!(
            v.heap()
                .fault_program(root.id())
                .unwrap()
                .package()
                .content_version,
            info.location.code.version
        );
        let mut stack = v.heap().fault_stack(root.id()).unwrap();
        let mut entries = Vec::new();
        while let Some(e) = stack.next(v.heap()).unwrap() {
            entries.push(e);
        }
        assert_eq!(entries.len(), if mode == 0 { 4 } else { 3 });
        assert!(
            entries
                .iter()
                .any(|e| matches!(e.kind, StackFrameKind::Native { abi_version: 1, .. }))
        );
        let first = v
            .heap()
            .fault_causes(root.id())
            .unwrap()
            .next(v.heap())
            .unwrap();
        assert_eq!(first.is_some(), mode == 2);
        if let Some(cause) = first {
            assert_eq!(cause.kind, CauseKind::ExplicitCause);
            assert_eq!(cause.fault, Some(old.id));
        }
        assert!(matches!(
            v.heap_mut().alloc_record(ObjectKind::FaultEnvelope, &[]),
            Err(HeapError::WrongKind)
        ));
        assert!(matches!(
            v.heap_mut().set_field(root.id().as_value(), 0, Value::NULL),
            Err(HeapError::WrongKind)
        ));
        let id = root.id();
        v.heap_mut().release_fault_root(root).unwrap();
        v.heap_mut().release_root(value_root).unwrap();
        v.heap_mut().release_root(cr).unwrap();
        collect(&mut v);
        assert!(v.heap().fault_info(id).is_err());
        assert!(v.heap().fault_info(old.id).is_err());
    }
}
#[test]
fn fault_capture_is_sliced_and_hard_capacity_resumes_without_reexecuting_throw() {
    let mut p = fixtures::exception_regions();
    let f = &mut p.functions[0];
    f.regions.clear();
    f.source_map.clear();
    f.code = vec![
        Instruction::new(Opcode::LoadConst, 0, 0, 0),
        Instruction::new(Opcode::Throw, 0, 0, 0),
    ];
    let registry = support(&p);
    let mut limits = VmLimits::default();
    limits.heap.max_objects = 2;
    let mut v = Vm::load(
        verify(p, Limits::default()).unwrap(),
        registry,
        limits,
        SessionEpoch(4),
    )
    .unwrap();
    let a = v.heap_mut().alloc_array(&[]).unwrap();
    let b = v.heap_mut().alloc_array(&[]).unwrap();
    let ar = v
        .heap_mut()
        .add_root(RootOwner(33), RootKind::Host, a)
        .unwrap();
    let br = v
        .heap_mut()
        .add_root(RootOwner(33), RootKind::Host, b)
        .unwrap();
    let task = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let mut quota = SliceBudget {
        instructions: 1,
        work: 1,
        allocation_bytes: 0,
        deadline_ns: None,
    };
    let mut throws = 0;
    let mut blocks = 0;
    let mut yields = 0;
    let d = loop {
        let out = v.run_slice(task, quota, || 0).unwrap();
        throws += v.stats().usage.instructions;
        gc(&mut v);
        match out {
            RunOutcome::BudgetYield(u) => {
                yields += 1;
                quota.work = (u.work + u.required_work).max(1);
                quota.allocation_bytes = (u.allocation_bytes + u.required_allocation).max(1);
            }
            RunOutcome::ResourceBlocked(blocked) => {
                blocks += 1;
                assert_eq!(blocked.resource, BlockedResource::HeapObjects);
                assert_eq!(blocked.current, 2);
                assert_eq!(blocked.required, 3);
                assert_eq!(v.stats().frames, 1);
                if blocks == 1 {
                    v.heap_mut().release_root(ar).unwrap();
                } else {
                    assert_eq!(blocks, 2);
                    v.heap_mut().release_root(br).unwrap();
                }
                collect(&mut v);
            }
            RunOutcome::Fault(d) => break d,
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(throws, 2);
    assert_eq!(blocks, 2);
    assert!(yields >= 5);
    let info = v.delivery_fault(d).unwrap().unwrap();
    assert_eq!(v.delivery_value(d).unwrap().tag(), ValueTag::Null);
    assert_instruction(
        &v,
        v.heap()
            .fault_stack(info.id)
            .unwrap()
            .next(v.heap())
            .unwrap()
            .unwrap()
            .location,
        1,
    );
}
#[test]
fn every_finally_replacement_keeps_the_complete_prior_fault_and_distinct_event() {
    let mut p = fixtures::exception_regions();
    let f = &mut p.functions[0];
    f.source_map.clear();
    let i = Instruction::new;
    f.code = vec![
        i(Opcode::LoadConst, 0, 1, 0),
        i(Opcode::Throw, 0, 0, 0),
        i(Opcode::LoadConst, 0, 2, 0),
        i(Opcode::Throw, 0, 0, 0),
        i(Opcode::EndFinally, 0, 0, 0),
        i(Opcode::LoadConst, 0, 0, 0),
        i(Opcode::Throw, 0, 0, 0),
        i(Opcode::EndFinally, 1, 0, 0),
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
                end: Pc(5),
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
                end: Pc(8),
            }),
        },
    ];
    let mut v = vm(p);
    let t = v
        .start(fixtures::ENTRY, &[], OWNER, Context::Ordinary)
        .unwrap();
    let d = task_finish(&mut v, t);
    collect(&mut v);
    let third = v.delivery_fault(d).unwrap().unwrap();
    let c3 = v
        .heap()
        .fault_causes(third.id)
        .unwrap()
        .next(v.heap())
        .unwrap()
        .unwrap();
    let second = c3.fault.unwrap();
    let c2 = v
        .heap()
        .fault_causes(second)
        .unwrap()
        .next(v.heap())
        .unwrap()
        .unwrap();
    let first = c2.fault.unwrap();
    assert_eq!(v.heap().fault_value(first).unwrap().as_int(), Some(41));
    assert_eq!(v.heap().fault_value(second).unwrap().as_int(), Some(1));
    assert_eq!(
        v.heap().fault_value(third.id).unwrap().tag(),
        ValueTag::Null
    );
    assert_eq!(
        c3.kind,
        CauseKind::ReplacedExit(ReplacedExit::Throw(second))
    );
    assert_eq!(c2.kind, CauseKind::ReplacedExit(ReplacedExit::Throw(first)));
    assert_ne!(c2.event, c3.event);
    assert!(
        v.heap()
            .fault_causes(first)
            .unwrap()
            .next(v.heap())
            .unwrap()
            .is_none()
    );
}

#[test]
fn one_fault_preserves_both_its_caught_fault_and_the_return_it_replaces() {
    let mut p = fixtures::exception_regions();
    let i = Instruction::new;
    let f = &mut p.functions[0];
    f.source_map.clear();
    f.register_count = 2;
    f.code = vec![
        i(Opcode::LoadConst, 0, 1, 0),
        i(Opcode::Throw, 0, 0, 0),
        i(Opcode::LoadConst, 1, 2, 0),
        i(Opcode::Jump, 4, 0, 0),
        i(Opcode::LoadConst, 1, 2, 0),
        i(Opcode::Return, 1, 0, 0),
        i(Opcode::LoadConst, 0, 0, 0),
        i(Opcode::Throw, 0, 0, 0),
        i(Opcode::EndFinally, 0, 0, 0),
        i(Opcode::Return, 1, 0, 0),
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
                end: Pc(9),
            }),
        },
        ExceptionRegion {
            protected: PcRange {
                start: Pc(0),
                end: Pc(2),
            },
            catch: Some(CatchRegion {
                range: PcRange {
                    start: Pc(2),
                    end: Pc(9),
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
    collect(&mut v);
    let info = v.delivery_fault(d).unwrap().unwrap();
    let mut causes = v.heap().fault_causes(info.id).unwrap();
    let replaced = causes.next(v.heap()).unwrap().unwrap();
    let caught = causes.next(v.heap()).unwrap().unwrap();
    assert_eq!(replaced.kind, CauseKind::ReplacedExit(ReplacedExit::Return));
    assert_eq!(replaced.value.as_int(), Some(1));
    assert_eq!(caught.kind, CauseKind::CaughtFault);
    assert_eq!(caught.value.as_int(), Some(41));
    assert_instruction(
        &v,
        v.heap().fault_info(caught.fault.unwrap()).unwrap().location,
        1,
    );
    assert!(causes.next(v.heap()).unwrap().is_none());
}
