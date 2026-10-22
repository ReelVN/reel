use super::*;
mod queue;
use aside_vm::snapshot::SnapshotBudget;
use reel_ui::{
    stage::{NodeId, NodeKind, NodeText},
    widgets::TextInputEvent,
};

#[test]
fn editing_policy_uses_shared_schema_bindings_and_restores_source_and_asb() {
    let code = r##"fn opening() {
        let data = model({body: "", locked: false, hint: "A hint"});
        let status = text("status");
        let editor = text_area(value: bind(data.body), placeholder: bind(data.hint),
            read_only: bind(data.locked), selection_color: "#10203040", caret_color: "#aabbcc",
            text_outline: 1, text_shadow_x: 2, on_change: |e| {
                if data.body != e.value || e.target.value != e.value { throw "binding"; }
                status.x += 1;
            }, on_submit: |e| {
                if e.value != data.body { throw "submit"; } status.y += 1;
            });
        stage.replace(column { editor; status;
            button("lock", on_click: || { data.locked = true; });
            button("author", on_click: || {
                data.body = "author\nvalue";
                data.hint = "changed";
                editor.with(selection_color: "#506070", caret_color: null,
                    text_outline: 2, text_shadow_y: 3);
            });
            button("unlock", on_click: || { data.locked = false; data.hint = ""; });
        });
        let mut rejected = 0;
        try { editor.selection_color = "bad"; } catch _ { rejected += 1; }
        try { editor.caret_color = "#nope00"; } catch _ { rejected += 1; }
        if rejected != 2 { throw "invalid colors accepted"; }
        narrator("waiting");
    }"##;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        drive(&mut vm, &mut session);
        let editor = vm
            .stage()
            .nodes()
            .find(|n| matches!(n.kind, NodeKind::TextInput { .. }))
            .unwrap()
            .id;
        let edit = |before: &str, after: &str, revision: u64| NodeText {
            node: editor,
            event: TextInputEvent::replace_all(before, after, revision),
        };
        assert!(vm.text_input(&mut session, &edit("", "user", 0)).unwrap());
        drive(&mut vm, &mut session);
        vm.click(&mut session, button(&vm, "lock")).unwrap();
        drive(&mut vm, &mut session);
        assert!(
            !vm.text_input(&mut session, &edit("user", "queued edit", 0))
                .unwrap()
        );
        assert!(
            vm.text_input(
                &mut session,
                &NodeText {
                    node: editor,
                    event: TextInputEvent::Submitted {
                        value: "user".into(),
                        replay: None
                    }
                }
            )
            .unwrap()
        );
        drive(&mut vm, &mut session);
        vm.click(&mut session, button(&vm, "author")).unwrap();
        drive(&mut vm, &mut session);
        let assert_config = |vm: &SessionVm| {
            let node = vm.stage().node(editor).unwrap();
            let NodeKind::TextInput { value, options, .. } = &node.kind else {
                panic!()
            };
            assert_eq!(value.as_ref(), "author\nvalue");
            assert!(options.read_only);
            assert_eq!(options.placeholder.as_deref(), Some("changed"));
            assert_eq!(options.selection_color, [0x50, 0x60, 0x70, 0xff]);
            assert_eq!(options.caret_color, None);
            assert_eq!(node.props.text_outline, 2.);
            assert_eq!(node.props.text_shadow_x, 2.);
            assert_eq!(node.props.text_shadow_y, 3.);
            assert_eq!(text_x(vm, "status"), 1.);
            assert_eq!(node_y(vm, "status"), 1.);
        };
        assert_config(&vm);
        let program = vm.vm.program().clone();
        let mut capture = vm.snapshot_begin(&session).unwrap();
        while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let mut image =
            snapshot::SessionSnapshot::decode(&capture.finish().unwrap().encode().unwrap())
                .unwrap();
        let mut restore = SessionVm::restore_begin(
            &mut image,
            program,
            VmLimits::default(),
            SessionIdentity {
                session_id: SessionId(41),
                epoch: Epoch(20),
            },
            crate::ReadProgress::default(),
            None,
            None,
        )
        .unwrap();
        while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let (mut session, mut vm) = restore.finish().unwrap();
        drive(&mut vm, &mut session);
        assert_config(&vm);
        vm.click(&mut session, button(&vm, "unlock")).unwrap();
        drive(&mut vm, &mut session);
        assert!(
            vm.text_input(&mut session, &edit("author\nvalue", "new", 1))
                .unwrap()
        );
        drive(&mut vm, &mut session);
        assert_eq!(text_x(&vm, "status"), 2.);
        let NodeKind::TextInput { options, .. } = &vm.stage().node(editor).unwrap().kind else {
            panic!()
        };
        assert!(options.placeholder.is_none());
    }
}

#[test]
fn input_history_coalesces_typed_edits_and_restores_typed_methods_and_selection() {
    use reel_ui::widgets::{TextEdit, TextEditKind, TextSelection};
    let code = r#"fn opening() {
        let data = model({body: ""});
        let status = text("status");
        let editor = text_input(value: bind(data.body), on_change: |e| {
            if data.body != e.value || editor_value(e.target) != e.value { throw "commit"; }
            status.x += 1;
        });
        stage.replace(column { editor; status;
            button("undo", on_click: || { if editor.undo() != editor { throw "identity"; } });
            button("redo", on_click: || { if editor.redo() != editor { throw "identity"; } });
            button("select", on_click: || {
                if editor.select_bytes(3, focus: 1).select_all() != editor { throw "selection identity"; }
                let mut rejected = 0;
                try { editor.select_bytes(2); } catch _ { rejected += 1; }
                try { editor.select_bytes(-1); } catch _ { rejected += 1; }
                if rejected != 2 { throw "UTF-8 validation"; }
                if editor.selection_anchor != 0 || editor.selection_focus != 6 { throw "selection"; }
            });
            button("replace", on_click: || { data.body = "author"; });
        });
        narrator("wait");
    }
    fn editor_value(input: TextInputNode) -> string { return input.value; }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        drive(&mut vm, &mut session);
        let editor = vm
            .stage()
            .nodes()
            .find(|n| matches!(n.kind, NodeKind::TextInput { .. }))
            .unwrap()
            .id;
        let mut previous = String::new();
        for text in ["A", "é", "中"] {
            let value = format!("{previous}{text}");
            let before = TextSelection {
                anchor: previous.len(),
                focus: previous.len(),
            };
            let after = TextSelection {
                anchor: value.len(),
                focus: value.len(),
            };
            assert!(
                vm.text_input(
                    &mut session,
                    &NodeText {
                        node: editor,
                        event: TextInputEvent::Changed {
                            previous: previous.clone().into(),
                            value: value.clone().into(),
                            edit: TextEdit {
                                range: previous.len()..previous.len(),
                                before,
                                after,
                                kind: TextEditKind::Insert,
                                revision: 0,
                                replay: None,
                            },
                        }
                    }
                )
                .unwrap()
            );
            drive(&mut vm, &mut session);
            previous = value;
        }
        let host = vm.stage.value(vm.vm.heap(), editor).unwrap();
        assert_eq!(vm.stage.text_history_entries(editor), Some(1));
        assert_eq!(text_x(&vm, "status"), 3.);
        vm.click(&mut session, button(&vm, "select")).unwrap();
        drive(&mut vm, &mut session);
        assert_eq!(
            vm.vm.heap().host_payload(host, 4).unwrap().as_int(),
            Some(2)
        );

        let program = vm.vm.program().clone();
        let mut capture = vm.snapshot_begin(&session).unwrap();
        while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let mut image =
            snapshot::SessionSnapshot::decode(&capture.finish().unwrap().encode().unwrap())
                .unwrap();
        let mut restore = SessionVm::restore_begin(
            &mut image,
            program,
            VmLimits::default(),
            SessionIdentity {
                session_id: SessionId(48),
                epoch: Epoch(21),
            },
            crate::ReadProgress::default(),
            None,
            None,
        )
        .unwrap();
        while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let (mut session, mut vm) = restore.finish().unwrap();
        drive(&mut vm, &mut session);
        let NodeKind::TextInput { options, .. } = &vm.stage().node(editor).unwrap().kind else {
            unreachable!()
        };
        assert_eq!(
            options.selection,
            TextSelection {
                anchor: 0,
                focus: 6
            }
        );
        assert_eq!(options.selection_revision, 2);
        vm.click(&mut session, button(&vm, "undo")).unwrap();
        drive(&mut vm, &mut session);
        let NodeKind::TextInput { value, options, .. } = &vm.stage().node(editor).unwrap().kind
        else {
            unreachable!()
        };
        assert_eq!(value.as_ref(), "");
        assert_eq!(options.selection, TextSelection::default());
        assert_eq!(text_x(&vm, "status"), 4.);
        vm.click(&mut session, button(&vm, "redo")).unwrap();
        drive(&mut vm, &mut session);
        let NodeKind::TextInput { value, .. } = &vm.stage().node(editor).unwrap().kind else {
            unreachable!()
        };
        assert_eq!(value.as_ref(), "Aé中");
        assert_eq!(text_x(&vm, "status"), 5.);
        vm.click(&mut session, button(&vm, "replace")).unwrap();
        drive(&mut vm, &mut session);
        assert!(vm.stage.text_history_entries(editor).is_none());
    }
}

fn node_y(vm: &SessionVm, caption: &str) -> f64 {
    vm.stage()
        .nodes()
        .find(|n| matches!(&n.kind, NodeKind::Text(t) if t.as_ref() == caption))
        .unwrap()
        .props
        .y
}

fn button(vm: &SessionVm, caption: &str) -> NodeId {
    vm.stage()
        .nodes()
        .find(|n| matches!(&n.kind, NodeKind::Button { label: Some(t), .. } if t == caption))
        .unwrap()
        .id
}
fn text_x(vm: &SessionVm, caption: &str) -> f64 {
    vm.stage()
        .nodes()
        .find(|n| matches!(&n.kind, NodeKind::Text(t) if t.as_ref() == caption))
        .unwrap()
        .props
        .x
}

fn drive(vm: &mut SessionVm, session: &mut Session) {
    for _ in 0..100_000 {
        match vm
            .step(
                session,
                SessionBudget::RetryMinimum(SliceBudget {
                    instructions: 7,
                    ..Default::default()
                }),
                || 0,
            )
            .unwrap()
        {
            SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                if let Err(error) =
                    vm.dispatch(session, ticket, "en", NativeBudget::RetryMinimum(1))
                {
                    vm.reject_native(session, ticket, error).unwrap();
                }
            }
            SessionStep::Vm(RunOutcome::Returned(_)) => vm.discard_delivery(session).unwrap(),
            SessionStep::Vm(RunOutcome::Fault(id)) => panic!(
                "editing fault: {:?} {:?}",
                vm.vm.delivery_fault(id).unwrap(),
                text::read_string(vm.vm.heap(), vm.vm.delivery_value(id).unwrap(), 4096)
            ),
            SessionStep::Vm(RunOutcome::ResourceBlocked(error)) => {
                panic!("editing resource: {error:?}")
            }
            SessionStep::Idle | SessionStep::Vm(RunOutcome::Waiting(_))
                if !vm.has_runnable_tasks() && !vm.activities_pending() =>
            {
                vm.sync_stage(session).unwrap();
                collect(vm);
                return;
            }
            _ => {}
        }
    }
    panic!("editing did not settle");
}
