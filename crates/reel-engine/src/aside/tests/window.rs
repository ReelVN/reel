use super::*;
use aside_vm::snapshot::SnapshotBudget;
use reel_core::window::{WindowCapabilities, WindowInfo, WindowPlatform};
use std::sync::Arc;

#[test]
fn platform_snapshot_and_configure_use_current_host_from_source_and_asb() {
    let code = r#"fn opening() {
        if window.info().platform != "unknown" { throw "headless platform"; }
        narrator("attach host");
        let before = window.info();
        if before.platform != "windows" || !before.fullscreen { throw "host platform"; }
        let applied = await window.configure(fullscreen: false);
        if applied.platform != "windows" || applied.fullscreen { throw "configure state"; }
        narrator("save");
        if before.platform != "windows" || !before.fullscreen { throw "saved value"; }
        if window.info().platform != "linux" || window.info().fullscreen { throw "restored host"; }
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
        advance(&mut session);
        models::drive(&mut vm, &mut session, false);
        let request = vm.take_window_request().unwrap().unwrap();
        assert_eq!(request.configuration.fullscreen, Some(false));
        let mut applied = vm.window_info().as_ref().clone();
        applied.fullscreen = false;
        vm.complete_window_request(request.id, Ok(Arc::new(applied)))
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
