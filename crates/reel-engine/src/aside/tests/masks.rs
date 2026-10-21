use super::*;
use aside_vm::snapshot::SnapshotBudget;
use std::sync::Arc;

fn advance(vm: &mut SessionVm, session: &mut Session) {
    let id = session.view().interaction.unwrap().id();
    for _ in 0..2 {
        session
            .handle_local_input(session.root_owner(), InputAction::Advance(id))
            .unwrap();
    }
    models::drive(vm, session, false);
}
#[test]
fn composed_mask_source_asb_snapshot_and_parameter_identity() {
    let code = r#"fn opening() {
        let a = mask.alpha("first.png", x: mask.param("x"), repeat: "x");
        let b = mask.alpha("second.png", y: 1, scale_y: -1);
        let q = mask.add(mask.pow(a, 3), mask.multiply(2, mask.pow(b, 2)));
        let coverage = mask.multiply(a, mask.subtract(1, mask.smoothstep(
            mask.clamp(mask.multiply(0.5, q)), mask.clamp(mask.multiply(1.2, q)),
            mask.subtract(1, mask.multiply(1.5, mask.param("progress"))))));
        let node = stack(mask: coverage, mask_progress: 0, width: 100, height: 100);
        stage.replace(node);
        narrator("initial");
        node.mask_x = 0.25; node.mask_progress = 1;
        narrator("changed");
        node.mask = null;
        narrator("cleared");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        let root = vm.stage().root().unwrap();
        let original = vm.stage().node(root).unwrap().props.mask.clone().unwrap();
        assert_eq!(
            original
                .sources()
                .iter()
                .map(|s| s.as_ref())
                .collect::<Vec<_>>(),
            ["first.png", "second.png"]
        );
        assert_eq!(
            original
                .operations()
                .iter()
                .filter(|op| matches!(op, reel_ui::stage::MaskOp::Sample { .. }))
                .count(),
            2
        );
        assert!(!original.is_image_threshold());
        advance(&mut vm, &mut session);
        let props = &vm.stage().node(root).unwrap().props;
        assert_eq!((props.mask_x, props.mask_progress), (0.25, 1.));
        assert!(Arc::ptr_eq(&original, props.mask.as_ref().unwrap()));
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
        let restored_root = restored.stage().root().unwrap();
        let props = &restored.stage().node(restored_root).unwrap().props;
        assert_eq!((props.mask_x, props.mask_progress), (0.25, 1.));
        assert_eq!(props.mask.as_ref().unwrap().as_ref(), original.as_ref());
        let weak = Arc::downgrade(props.mask.as_ref().unwrap());
        advance(&mut restored, &mut restored_session);
        assert!(
            restored
                .stage()
                .node(restored_root)
                .unwrap()
                .props
                .mask
                .is_none()
        );
        assert!(weak.upgrade().is_none());
        advance(&mut vm, &mut session);
        assert!(vm.stage().node(root).unwrap().props.mask.is_none());
        drop(original);
        collect(&mut vm);
    }
}
#[test]
fn mask_construction_rejects_invalid_values_at_author_calls() {
    let code = r#"fn dynamic(value) {value} fn opening() {
        let mut rejected = 0;
        try { mask.alpha(""); } catch e { rejected += 1; }
        try { mask.alpha("big.reel-image"); } catch e { rejected += 1; }
        try { mask.alpha("a.png", repeat: "invalid"); } catch e { rejected += 1; }
        try { mask.alpha("a.png", x: dynamic({})); } catch e { rejected += 1; }
        try { mask.param("unknown"); } catch e { rejected += 1; }
        try { mask.pow(1, -1); } catch e { rejected += 1; }
        try { mask.clamp(1, min: 2, max: 1); } catch e { rejected += 1; }
        try { let _ = stack(mask_x: 1e300); } catch e { rejected += 1; }
        if rejected != 8 { throw f"rejected {rejected}"; }
        narrator("checked");
    }"#;
    let (mut session, mut vm) = models::prepared(code, true);
    models::drive(&mut vm, &mut session, false);
    assert!(session.view().interaction.is_some());
}

#[test]
fn composed_mask_example_links_from_source_and_asb() {
    let code = include_str!("../../../../../examples/author_composed_mask/src/main.aside");
    let story = source::compile_story(code, "example", 1, "main").unwrap();
    let bytes = bc::encode(story.program.package(), bc::Limits::default()).unwrap();
    let program = bc::verify(
        bc::decode(&bytes, bc::Limits::default()).unwrap(),
        bc::Limits::default(),
    )
    .unwrap();
    SessionVm::load(&session(), program, VmLimits::default()).unwrap();
}

#[test]
fn mask_uniform_properties_share_tween_binding_and_owner_cancellation() {
    let code = r#"fn opening() {
        let state = model({offset: -0.25});
        let node = stack(mask: mask.param("progress"), mask_progress: 0,
            mask_y: bind(state.offset));
        stage.replace(node);
        let moving = run(tween(node, mask_x: 1, mask_progress: 1, duration: 2), owner: node);
        narrator("moving");
        state.offset = -0.75;
        narrator("bound");
        node.remove();
        let mut cancelled = false;
        try { await moving; } catch error { cancelled = true; }
        if !cancelled { throw "animation survived its owner"; }
        narrator("removed");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        let root = vm.stage().root().unwrap();
        let original = vm.stage().node(root).unwrap().props.mask.clone().unwrap();
        activities::advance_clock(&mut vm, &session, 1., 1);
        vm.sync_stage(&session).unwrap();
        let props = &vm.stage().node(root).unwrap().props;
        assert_eq!(
            (props.mask_x, props.mask_y, props.mask_progress),
            (0.5, -0.25, 0.5)
        );
        assert!(Arc::ptr_eq(&original, props.mask.as_ref().unwrap()));
        advance(&mut vm, &mut session);
        assert_eq!(vm.stage().node(root).unwrap().props.mask_y, -0.75);
        advance(&mut vm, &mut session);
        assert!(vm.stage().root().is_none());
        assert!(!vm.activities_need_time());
    }
}
