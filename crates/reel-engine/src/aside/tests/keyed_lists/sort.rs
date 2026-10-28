use super::*;
use aside_vm::{ValueTag, snapshot::SnapshotBudget};
fn prepared(virtualized: bool, encoded: bool) -> (Session, SessionVm) {
    let prefix = "共同前缀".repeat(128);
    models::prepared(
        &format!(
            r#"
        let mut keys=0;
        fn opening() {{
            stage.replace(text("kept"));
            let items=[{{id:"{prefix}z"}},{{id:"{prefix}a"}},{{id:"{prefix}m"}}];
            let rows=list(||items,key:|x|{{keys+=1;x.id}},virtual:{virtualized},build:|row|text(value:f"row {{row.index}}"));
            stage.replace(rows);
            if keys!=3 {{throw "completed key callback replayed";}}
            narrator("ready");
        }}"#
        ),
        encoded,
    )
}
#[derive(Clone, Copy)]
enum Checkpoint {
    Setup,
    Prefix,
    Merge,
}
fn at_sort(
    vm: &mut SessionVm,
    session: &mut Session,
    virtualized: bool,
    checkpoint: Checkpoint,
) -> HostTicket {
    for _ in 0..100000 {
        match vm
            .step(
                session,
                SessionBudget::Exact(SliceBudget {
                    instructions: 32,
                    ..Default::default()
                }),
                || 0,
            )
            .unwrap()
        {
            SessionStep::Vm(RunOutcome::HostCall(t)) => {
                let p = vm.vm.program().package();
                let list = p.symbols[p.natives[t.native.index()].symbol.index()] == "reel::list";
                // Existing v1 stage IDs stay unchanged; only these new sort IDs
                // stop at private setup, a string prefix or a partially merged buffer.
                if list
                    && t.continuation
                        .is_some_and(|c| c.stage == if virtualized { 7 } else { 5 })
                {
                    let values = vm.vm.host_continuation(t).unwrap();
                    let state = values[4];
                    if state.tag() != ValueTag::Null {
                        let len = vm.vm.heap().sequence_len(state).unwrap();
                        let at_checkpoint = match checkpoint {
                            Checkpoint::Setup => (1..13).contains(&len),
                            Checkpoint::Prefix => {
                                len == 13
                                    && vm
                                        .vm
                                        .heap()
                                        .sequence_get(state, 8)
                                        .unwrap()
                                        .as_int()
                                        .unwrap()
                                        > 0
                            }
                            Checkpoint::Merge => {
                                len == 13
                                    && vm
                                        .vm
                                        .heap()
                                        .sequence_get(state, 3)
                                        .unwrap()
                                        .as_int()
                                        .unwrap()
                                        >= 2
                                    && vm
                                        .vm
                                        .heap()
                                        .sequence_get(state, 7)
                                        .unwrap()
                                        .as_int()
                                        .unwrap()
                                        > 0
                            }
                        };
                        if at_checkpoint {
                            return t;
                        }
                    }
                }
                vm.dispatch(session, t, "en", NativeBudget::Exact(7))
                    .unwrap();
            }
            SessionStep::Vm(RunOutcome::Returned(_)) => vm.discard_delivery(session).unwrap(),
            SessionStep::Vm(RunOutcome::Fault(id)) => {
                panic!("{:?}", vm.vm.delivery_fault(id).unwrap())
            }
            _ => {}
        }
    }
    panic!("did not reach partial list key comparison");
}
#[test]
fn list_sort_prefix_gc_and_snapshot_resume_without_key_callback_replay() {
    for virtualized in [false, true] {
        for (encoded, checkpoint) in [
            (false, Checkpoint::Setup),
            (true, Checkpoint::Prefix),
            (true, Checkpoint::Merge),
        ] {
            let (mut session, mut vm) = prepared(virtualized, encoded);
            let t = at_sort(&mut vm, &mut session, virtualized, checkpoint);
            let control = vm.vm.host_continuation(t).unwrap()[4];
            let version = vm.vm.heap().info(control).unwrap().content_version;
            assert_eq!(
                vm.dispatch(&mut session, t, "en", NativeBudget::Exact(0))
                    .unwrap(),
                NativeDispatch::BudgetYield {
                    work: 0,
                    required_work: 1
                }
            );
            assert_eq!(vm.vm.heap().info(control).unwrap().content_version, version);
            vm.dispatch(
                &mut session,
                t,
                "en",
                NativeBudget::Exact(if encoded { 7 } else { 1 }),
            )
            .unwrap();
            collect(&mut vm);
            assert!(
                has_text(&vm, "kept"),
                "private index must not replace mounted stage"
            );
            let mut capture = vm.snapshot_begin(&session).unwrap();
            while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
            let bytes = capture.finish().unwrap().encode().unwrap();
            let mut saved = snapshot::SessionSnapshot::decode(&bytes).unwrap();
            let mut restore = SessionVm::restore_begin(
                &mut saved,
                vm.vm.program().clone(),
                VmLimits::default(),
                SessionIdentity {
                    session_id: SessionId(123),
                    epoch: Epoch(2),
                },
                Default::default(),
                None,
                None,
            )
            .unwrap();
            while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
            let (mut session, mut vm) = restore.finish().unwrap();
            drive(&mut vm, &mut session, true);
            assert!(!has_text(&vm, "kept"));
            assert!(has_text(&vm, "row 0"));
            if virtualized {
                assert!(vm.stage().nodes().any(
                    |node| matches!(&node.kind, NodeKind::VirtualList(view) if view.total_rows == 3)
                ));
            } else {
                assert!(has_text(&vm, "row 1"));
                assert!(has_text(&vm, "row 2"));
            }
        }
    }
}
#[test]
fn list_sort_cancellation_and_source_version_reject_before_index_publication() {
    for virtualized in [false, true] {
        for changed in [false, true] {
            let (mut session, mut vm) = prepared(virtualized, true);
            let t = at_sort(
                &mut vm,
                &mut session,
                virtualized,
                if changed {
                    Checkpoint::Merge
                } else {
                    Checkpoint::Prefix
                },
            );
            if changed {
                let source = vm.vm.host_continuation(t).unwrap()[2];
                vm.vm.heap_mut().array_push(source, Value::NULL).unwrap();
                let error = vm
                    .dispatch(&mut session, t, "en", NativeBudget::Exact(7))
                    .unwrap_err();
                assert!(matches!(
                    error,
                    AdapterError::Heap(HeapError::ConcurrentModification)
                ));
                vm.reject_native(&session, t, error).unwrap();
                assert!(has_text(&vm, "kept"));
            }
            vm.revoke(&mut session).unwrap();
            while vm.cleanup_pending() {
                vm.cleanup_step(
                    &mut session,
                    8,
                    SessionBudget::Exact(SliceBudget {
                        instructions: 1,
                        ..Default::default()
                    }),
                    || 0,
                )
                .unwrap();
            }
            collect(&mut vm);
            assert!(!vm.vm.heap().reactive_evaluating());
            assert!(vm.stage().nodes().next().is_none());
        }
    }
}
