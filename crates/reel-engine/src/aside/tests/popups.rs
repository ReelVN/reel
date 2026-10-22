use super::*;
use aside_vm::snapshot::SnapshotBudget;
use reel_ui::{
    stage::{NodeId, NodeKind, NodePopupDismiss},
    widgets::{PopupDismiss, PopupPlacement},
};

fn pump(vm: &mut SessionVm, session: &mut Session) {
    for _ in 0..10_000 {
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
                    vm.dispatch(session, ticket, "en", NativeBudget::RetryMinimum(32))
                {
                    vm.reject_native(session, ticket, error).unwrap();
                }
            }
            SessionStep::Vm(RunOutcome::Returned(_)) => vm.discard_delivery(session).unwrap(),
            SessionStep::Vm(RunOutcome::Fault(id)) => {
                panic!("popup fault: {:?}", vm.vm.delivery_fault(id).unwrap())
            }
            SessionStep::Vm(RunOutcome::ResourceBlocked(error)) => {
                panic!("popup resource: {error:?}")
            }
            SessionStep::Idle | SessionStep::Vm(RunOutcome::Waiting(_))
                if !vm.has_runnable_tasks() && !vm.activities_pending() =>
            {
                vm.sync_stage(session).unwrap();
                return;
            }
            _ => {}
        }
    }
    panic!("popup tasks did not suspend");
}

fn button(vm: &SessionVm, label: &str) -> NodeId {
    vm.stage()
        .nodes()
        .find(|node| {
            matches!(&node.kind,
        NodeKind::Button { label: Some(text), .. } if text == label)
        })
        .unwrap()
        .id
}

#[test]
fn unhandled_popup_dismissal_allocates_no_callback_state() {
    for encoded in [false, true] {
        let (mut session, mut vm) =
            models::prepared("fn opening() {stage.replace(stack {popup();});}", encoded);
        pump(&mut vm, &mut session);
        let node = vm
            .stage()
            .nodes()
            .find(|n| matches!(n.kind, NodeKind::Popup(_)))
            .unwrap()
            .id;
        collect(&mut vm);
        let before = vm.vm.heap().stats();
        let tasks = vm.tasks.len();
        for _ in 0..4096 {
            assert!(
                !vm.dismiss_popup(
                    &mut session,
                    &NodePopupDismiss {
                        node,
                        reason: PopupDismiss::Outside
                    }
                )
                .unwrap()
            );
        }
        let after = vm.vm.heap().stats();
        assert_eq!(vm.tasks.len(), tasks);
        assert_eq!(
            (
                after.live_objects,
                after.payload_bytes,
                after.chunk_allocations,
                after.roots
            ),
            (
                before.live_objects,
                before.payload_bytes,
                before.chunk_allocations,
                before.roots
            )
        );
    }
}

#[test]
fn popup_scope_overlays_base_modals_and_restores_its_parent_and_callback() {
    let code = r#"fn opening() {
        let result = text("result");
        let menu = popup(on_dismiss: |event| {
            if event.reason != "back" { throw "wrong close reason"; }
            result.x += 10;
            event.target.remove();
        }).with(placement: "top-end", offset: 4);
        menu.add(button("choose", on_click: || { result.x += 1; }));
        stage.replace(stack {
            result;
            button("anchor") { menu; }
            column(modal: true) { button("base", on_click: || { result.x += 100; }); }
        });
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        pump(&mut vm, &mut session);
        let popup = vm
            .stage()
            .nodes()
            .find(|n| matches!(n.kind, NodeKind::Popup(_)))
            .unwrap();
        let id = popup.id;
        let parent = popup.parent;
        assert!(popup.props.modal);
        assert!(
            matches!(popup.kind, NodeKind::Popup(view) if view.placement == PopupPlacement::TopEnd && view.offset == 4.)
        );
        assert!(!vm.click(&mut session, button(&vm, "base")).unwrap());
        assert!(vm.click(&mut session, button(&vm, "choose")).unwrap());
        pump(&mut vm, &mut session);
        let program = vm.vm.program().clone();
        let mut capture = vm.snapshot_begin(&session).unwrap();
        while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let bytes = capture.finish().unwrap().encode().unwrap();
        let mut image = snapshot::SessionSnapshot::decode(&bytes).unwrap();
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
        assert_eq!(vm.stage().node(id).unwrap().parent, parent);
        assert!(vm.stage().has_popups());
        assert!(
            vm.dismiss_popup(
                &mut session,
                &NodePopupDismiss {
                    node: id,
                    reason: PopupDismiss::Back
                }
            )
            .unwrap()
        );
        pump(&mut vm, &mut session);
        assert!(vm.stage().node(id).is_none());
        assert!(!vm.stage().has_popups());
        assert!(
            !vm.dismiss_popup(
                &mut session,
                &NodePopupDismiss {
                    node: id,
                    reason: PopupDismiss::Back
                }
            )
            .unwrap()
        );
        assert!(vm.click(&mut session, button(&vm, "base")).unwrap());
        pump(&mut vm, &mut session);
        let result = vm
            .stage()
            .nodes()
            .find(|n| matches!(&n.kind, NodeKind::Text(t) if t.as_ref() == "result"))
            .unwrap();
        assert_eq!(result.props.x, 111.);
    }
}

#[test]
fn popup_scope_respects_nested_layer_order_and_current_ancestor_flags() {
    let code = r#"fn opening() {
        let owner = column();
        owner.add(popup { button("first", on_click: || {}); });
        let second = popup(modal: false);
        second.add(column(modal: true) { button("second", on_click: || {}); });
        owner.add(second);
        stage.replace(stack { owner; button("base", on_click: || {}); });
        let mut blocked = false;
        try { second.placement = "wrong"; } catch e { blocked = true; }
        if !blocked { throw "invalid placement accepted"; }
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        pump(&mut vm, &mut session);
        assert!(!vm.click(&mut session, button(&vm, "first")).unwrap());
        assert!(vm.click(&mut session, button(&vm, "second")).unwrap());
        pump(&mut vm, &mut session);
        let first = button(&vm, "first");
        let popup = vm.stage().node(first).unwrap().parent.unwrap();
        let owner = vm.stage().node(popup).unwrap().parent.unwrap();
        let host = vm.stage.value(vm.vm.heap(), owner).unwrap();
        let offset = vm.stage.property_offset(vm.vm.heap(), host).unwrap();
        vm.vm
            .heap_mut()
            .write_host_property(
                aside_vm::PropertyId {
                    host,
                    field: offset + stage::NodeProperty::Enabled.index() as u32,
                },
                Value::bool(false),
                true,
            )
            .unwrap();
        assert!(!vm.click(&mut session, first).unwrap());
        assert!(vm.click(&mut session, button(&vm, "base")).unwrap());
    }
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(
            r#"fn opening() { stage.replace(popup { button("unanchored", on_click: || {throw "unanchored input";}); }); }"#,
            encoded,
        );
        pump(&mut vm, &mut session);
        assert!(!vm.click(&mut session, button(&vm, "unanchored")).unwrap());
    }
}
