use super::*;
use aside_vm::snapshot::SnapshotBudget;

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
                    vm.dispatch(session, ticket, "en", NativeBudget::RetryMinimum(32))
                {
                    vm.reject_native(session, ticket, error).unwrap();
                }
            }
            SessionStep::Vm(RunOutcome::Returned(_)) => vm.discard_delivery(session).unwrap(),
            SessionStep::Vm(RunOutcome::Fault(id)) => panic!(
                "writing mode fault: {:?}, value: {:?}",
                vm.vm.delivery_fault(id).unwrap(),
                text::read_string(vm.vm.heap(), vm.vm.delivery_value(id).unwrap(), 4096)
            ),
            SessionStep::Idle | SessionStep::Vm(RunOutcome::Waiting(_))
                if !vm.has_runnable_tasks() && !vm.activities_pending() =>
            {
                vm.sync_stage(session).unwrap();
                return;
            }
            _ => {}
        }
    }
    panic!("writing mode task failed to settle");
}

#[test]
fn writing_mode_source_asb_validation_bindings_configuration_and_snapshot() {
    let code = r#"
    fn dynamic(value) { value }
    fn opening() {
        let state = model({mode: "vertical-rl"});
        let plain = text("default");
        if plain.writing_mode != "horizontal-tb" { throw "horizontal default"; }
        let direct = text(rich("[ruby=ほし]星[/ruby]空"), writing_mode: "vertical-rl");
        let bound = text("bound", writing_mode: bind(state.mode));
        let derived = text("derived", writing_mode: derive(|| state.mode));
        let input = text_input(value: "editable");
        let area = text_area(value: "editable\narea");
        let mut rejected = 0;
        try { let _ = text("bad", writing_mode: "sideways"); } catch error { rejected += 1; }
        try { direct.writing_mode = dynamic(0); } catch error { rejected += 1; }
        try { let _ = text_input(writing_mode: "vertical-rl"); } catch error { rejected += 1; }
        try { let _ = text_area(writing_mode: "vertical-lr"); } catch error { rejected += 1; }
        try { input.writing_mode = "vertical-rl"; } catch error { rejected += 1; }
        try { area.with(writing_mode: "vertical-lr"); } catch error { rejected += 1; }
        if rejected != 6 || input.writing_mode != "horizontal-tb" || area.writing_mode != "horizontal-tb" {
            throw "writing mode validation and rollback";
        }
        stage.replace(column {
            plain; direct; bound; derived; input; area;
            button("toggle", writing_mode: "vertical-rl", on_click: || {
                state.mode = if state.mode == "vertical-rl" { "vertical-lr" } else { "vertical-rl" };
                direct.with(writing_mode: state.mode);
            });
            toggle("switch", writing_mode: "vertical-lr");
            radio(0, label: "option", value: 0, writing_mode: "vertical-rl");
        });
        narrator("ready");
    }
    "#;
    let assert_modes = |vm: &SessionVm, mode| {
        for node in vm.stage().nodes() {
            match &node.kind {
                NodeKind::Text(text)
                    if text.as_ref() == "星空"
                        || text.as_ref() == "bound"
                        || text.as_ref() == "derived" =>
                {
                    assert_eq!(node.props.writing_mode, mode);
                }
                NodeKind::TextInput { .. } => {
                    assert_eq!(node.props.writing_mode, reel_ui::WritingMode::HorizontalTb)
                }
                NodeKind::Button { .. } | NodeKind::Radio { .. } => {
                    assert_eq!(node.props.writing_mode, reel_ui::WritingMode::VerticalRl)
                }
                NodeKind::Toggle { .. } => {
                    assert_eq!(node.props.writing_mode, reel_ui::WritingMode::VerticalLr)
                }
                _ => {}
            }
        }
    };
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        drive(&mut vm, &mut session);
        assert_modes(&vm, reel_ui::WritingMode::VerticalRl);
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
        let (mut restored_session, mut restored) = restore.finish().unwrap();
        for (session, vm) in [
            (&mut session, &mut vm),
            (&mut restored_session, &mut restored),
        ] {
            drive(vm, session);
            assert_modes(vm, reel_ui::WritingMode::VerticalRl);
            let button = vm
                .stage()
                .nodes()
                .find(|node| {
                    matches!(&node.kind,
                NodeKind::Button {label: Some(label), ..} if label == "toggle")
                })
                .unwrap()
                .id;
            for mode in [
                reel_ui::WritingMode::VerticalLr,
                reel_ui::WritingMode::VerticalRl,
            ] {
                assert!(vm.click(session, button).unwrap());
                drive(vm, session);
                assert_modes(vm, mode);
            }
        }
    }
}
