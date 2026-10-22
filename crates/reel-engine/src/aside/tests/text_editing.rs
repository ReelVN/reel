use super::*;
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
        let edit = |before: &str, after: &str| NodeText {
            node: editor,
            event: TextInputEvent::Changed {
                previous: before.into(),
                value: after.into(),
            },
        };
        assert!(vm.text_input(&mut session, &edit("", "user")).unwrap());
        drive(&mut vm, &mut session);
        vm.click(&mut session, button(&vm, "lock")).unwrap();
        drive(&mut vm, &mut session);
        assert!(
            !vm.text_input(&mut session, &edit("user", "queued edit"))
                .unwrap()
        );
        assert!(
            vm.text_input(
                &mut session,
                &NodeText {
                    node: editor,
                    event: TextInputEvent::Submitted("user".into())
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
            vm.text_input(&mut session, &edit("author\nvalue", "new"))
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
