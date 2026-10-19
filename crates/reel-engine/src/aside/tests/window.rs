use super::*;
use aside_vm::snapshot::SnapshotBudget;
use reel_core::window::{WindowCapabilities, WindowInfo, WindowPlatform};
use std::sync::Arc;

#[test]
fn platform_snapshot_and_configure_use_current_host_from_source_and_asb() {
    let code = r#"fn opening() {
        if window.info().platform != "unknown" { throw "headless platform"; }
        if stage.pointer() != null { throw "initial pointer"; }
        narrator("attach host");
        let before = window.info();
        let pointer_before = stage.pointer();
        if pointer_before.x != 12 || pointer_before.y != 34 { throw "host pointer"; }
        if before.platform != "windows" || !before.fullscreen { throw "host platform"; }
        let applied = await window.configure(fullscreen: false);
        if applied.platform != "windows" || applied.fullscreen { throw "configure state"; }
        narrator("save");
        if before.platform != "windows" || !before.fullscreen { throw "saved value"; }
        if window.info().platform != "linux" || window.info().fullscreen { throw "restored host"; }
        if stage.pointer() != null || pointer_before.x != 12 { throw "pointer restoration"; }
        narrator("restored");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        vm.set_window_info(Arc::new(WindowInfo {
            platform: WindowPlatform::Windows,
            fullscreen: true,
            capabilities: WindowCapabilities {
                fullscreen: true,
                ..Default::default()
            },
            ..Default::default()
        }));
        vm.set_pointer_position(Some([12., 34.]));
        advance(&mut session);
        models::drive(&mut vm, &mut session, false);
        let request = vm.take_window_request().unwrap().unwrap();
        let crate::aside::window::WindowCommand::Configure(configuration) = request.command else {
            panic!("configure request");
        };
        assert_eq!(configuration.fullscreen, Some(false));
        let mut applied = vm.window_info().as_ref().clone();
        applied.fullscreen = false;
        vm.complete_window_request(
            request.id,
            Ok(crate::aside::window::WindowReply::Configured(Arc::new(
                applied,
            ))),
        )
        .unwrap();
        models::drive(&mut vm, &mut session, false);
        assert!(
            matches!(session.view().interaction, Some(crate::InteractionView::Dialogue(d)) if d.text() == "save")
        );
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
        assert_eq!(vm.window_info().platform, WindowPlatform::Unknown);
        vm.set_window_info(Arc::new(WindowInfo {
            platform: WindowPlatform::Linux,
            ..Default::default()
        }));
        advance(&mut session);
        models::drive(&mut vm, &mut session, false);
        assert!(
            matches!(session.view().interaction, Some(crate::InteractionView::Dialogue(d)) if d.text() == "restored")
        );
    }
}

fn advance(session: &mut Session) {
    let id = session.view().interaction.unwrap().id();
    for _ in 0..2 {
        session
            .handle_local_input(session.root_owner(), InputAction::Advance(id))
            .unwrap();
    }
}

#[test]
fn pointer_requests_have_owned_results_and_one_pending_slot_from_source_and_asb() {
    let code = r#"fn opening() {
        let panel = stack(width: 320, height: 200);
        let root = stack(children: [panel]);
        stage.replace(root);
        if await stage.move_pointer(10, 20, owner: panel) { throw "unsupported"; }
        narrator("attach");
        let request = stage.move_pointer(100, 120, owner: panel);
        let mut busy = false;
        try { stage.move_pointer(20, 40, owner: panel); } catch error { busy = true; }
        if !busy { throw "unbounded queue"; }
        if !await request { throw "host refused"; }
        let cancelled = stage.move_pointer(50, 60, owner: panel);
        panel.remove();
        let mut rejected = false;
        try { await cancelled; } catch error { rejected = true; }
        if !rejected { throw "retired owner"; }
        narrator("done");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        assert!(vm.take_window_request().unwrap().is_none());
        vm.set_window_info(Arc::new(WindowInfo {
            capabilities: WindowCapabilities {
                pointer_move: true,
                ..Default::default()
            },
            ..Default::default()
        }));
        advance(&mut session);
        models::drive(&mut vm, &mut session, false);
        let request = vm.take_window_request().unwrap().unwrap();
        assert!(matches!(
            request.command,
            crate::aside::window::WindowCommand::MovePointer {
                position: [100., 120.],
                ..
            }
        ));
        vm.complete_window_request(
            request.id,
            Ok(crate::aside::window::WindowReply::PointerMoved(true)),
        )
        .unwrap();
        assert!(
            !vm.complete_window_request(
                request.id,
                Ok(crate::aside::window::WindowReply::PointerMoved(true))
            )
            .unwrap()
        );
        models::drive(&mut vm, &mut session, false);
        assert!(vm.take_window_request().unwrap().is_none());
        assert!(
            matches!(session.view().interaction, Some(crate::InteractionView::Dialogue(d)) if d.text()=="done")
        );
    }
}
