use super::*;
use aside_vm::snapshot::SnapshotBudget;

#[test]
fn capture_included_source_asb_writes_and_snapshot_restore() {
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(
            r#"fn opening() {
            let child = stack(capture_included: false);
            stage.replace(stack() { child });
            narrator("excluded");
            child.capture_included = true;
            narrator("included");
        }"#,
            encoded,
        );
        models::drive(&mut vm, &mut session, false);
        let root = vm.stage().root().unwrap();
        let child = vm.stage().node(root).unwrap().children[0];
        assert!(vm.stage().node(root).unwrap().props.capture_included);
        assert!(!vm.stage().node(child).unwrap().props.capture_included);
        let mut capture = vm.snapshot_begin(&session).unwrap();
        while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let bytes = capture.finish().unwrap().encode().unwrap();
        let mut image = snapshot::SessionSnapshot::decode(&bytes).unwrap();
        let mut restore = SessionVm::restore_begin(
            &mut image,
            vm.vm.program().clone(),
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
        assert!(!restored.stage().node(child).unwrap().props.capture_included);
        let id = restored_session.view().interaction.unwrap().id();
        for _ in 0..2 {
            restored_session
                .handle_local_input(restored_session.root_owner(), InputAction::Advance(id))
                .unwrap();
        }
        models::drive(&mut restored, &mut restored_session, false);
        assert!(restored.stage().node(child).unwrap().props.capture_included);
    }
}
