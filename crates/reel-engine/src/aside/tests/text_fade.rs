use super::*;
use aside_vm::snapshot::SnapshotBudget;
fn restore(session: &Session, vm: &mut SessionVm) -> (Session, SessionVm) {
    let program = vm.vm.program().clone();
    let mut capture = vm.snapshot_begin(session).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let mut image =
        snapshot::SessionSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
    let mut restore = SessionVm::restore_begin(
        &mut image,
        program,
        VmLimits::default(),
        SessionIdentity {
            session_id: SessionId(42),
            epoch: Epoch(21),
        },
        crate::ReadProgress::default(),
        None,
        None,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    restore.finish().unwrap()
}
fn props(vm: &mut SessionVm, session: &Session) -> (Option<f64>, usize, f64) {
    vm.sync_stage(session).unwrap();
    let p = &vm.stage().node(vm.stage().root().unwrap()).unwrap().props;
    (p.reveal_fade, p.reveal_fade_from, p.reveal_fade_step)
}
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
fn text_fade_scalar_source_asb_binding_tween_cancel_and_snapshot() {
    let code = r##"fn dynamic(value) {value} fn opening() {
        let state=model({step:0.25});
        let node=text("雪AB",reveal_fade:0,reveal_fade_from:3,reveal_fade_step:bind(state.step),
            text_shadow_x:-1,text_shadow_y:2,text_shadow_color:"#123456");
        stage.replace(node);
        let mut rejected=0;
        try {node.reveal_fade=dynamic("bad");}catch error{rejected+=1;}
        try {node.reveal_fade_step=-1;}catch error{rejected+=1;}
        try {node.reveal_fade_from=-1;}catch error{rejected+=1;}
        try {let _ = text("雪",reveal_fade_from:1);}catch error{rejected+=1;}
        if rejected!=4 {throw "fade input contract";}
        let moving=tween(node,reveal_fade:1,reveal_fade_step:0.5,duration:2).run();
        narrator("moving");
        moving.stop();
        if await moving!="Stopped" || node.reveal_fade!=0.5 {throw "fade stop must preserve progress";}
        narrator("stopped");
        node.reveal_fade=null;
        narrator("disabled");
    }"##;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        activities::advance_clock(&mut vm, &session, 1., 64);
        assert_eq!(props(&mut vm, &session), (Some(0.5), 3, 0.375));
        (session, vm) = restore(&session, &mut vm);
        assert_eq!(props(&mut vm, &session), (Some(0.5), 3, 0.375));
        let p = &vm.stage().node(vm.stage().root().unwrap()).unwrap().props;
        assert_eq!(
            (p.text_shadow_x, p.text_shadow_y, p.text_shadow_color),
            (-1., 2., [0x12, 0x34, 0x56, 255])
        );
        advance(&mut vm, &mut session);
        activities::advance_clock(&mut vm, &session, 5., 64);
        assert_eq!(props(&mut vm, &session), (Some(0.5), 3, 0.375));
        advance(&mut vm, &mut session);
        assert_eq!(props(&mut vm, &session), (None, 3, 0.375));
    }
}

#[test]
fn text_fade_origin_is_not_an_animation_target() {
    assert!(
        source::compile_story(
            "fn opening(){let node=text(\"x\");let _ = tween(node,reveal_fade_from:0,duration:1);}",
            "invalid",
            1,
            "opening"
        )
        .is_err()
    );
}

#[test]
fn manual_advance_request_source_asb_append_playback_and_snapshot() {
    let code = r#"fn opening() {
        let d=game.dialogue();
        let speech=narrator("Initial text",wait:false);
        await when(||d.advance_requested);
        if !d.lines[0].advance_requested {throw "line request missing";}
        speech.append(" continuation",wait:false);
        if d.advance_requested || d.lines[0].advance_requested {throw "append retained request";}
        await when(||d.advance_requested);
        narrator("New group",wait:false);
        if d.advance_requested {throw "new group root retained request";}
        if d.lines[d.lines.len()-1].advance_requested {throw "new group line retained request";}
        stage.replace(stack(x:123));
        await completion();
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        let id = session.view().interaction.unwrap().id();
        assert_eq!(
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(id))
                .unwrap(),
            crate::InputOutcome::Revealed
        );
        assert!(
            matches!(session.view().interaction,Some(crate::InteractionView::Dialogue(d)) if d.advance_requested())
        );
        (session, vm) = restore(&session, &mut vm);
        models::drive(&mut vm, &mut session, false);
        let id = session.view().interaction.unwrap().id();
        assert!(
            matches!(session.view().interaction,Some(crate::InteractionView::Dialogue(d)) if !d.advance_requested())
        );
        assert_eq!(
            session
                .handle_playback_input(session.root_owner(), InputAction::Advance(id))
                .unwrap(),
            crate::InputOutcome::Revealed
        );
        models::drive(&mut vm, &mut session, false);
        assert!(
            matches!(session.view().interaction,Some(crate::InteractionView::Dialogue(d)) if !d.advance_requested())
        );
        assert_eq!(
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(id))
                .unwrap(),
            crate::InputOutcome::Pending
        );
        models::drive(&mut vm, &mut session, false);
        vm.sync_stage(&session).unwrap();
        assert_eq!(
            vm.stage().node(vm.stage().root().unwrap()).unwrap().props.x,
            123.
        );
    }
}

#[test]
fn manual_advance_request_protected_reveal_waits_for_ready() {
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(
            r#"fn opening(){narrator("Protected",wait:false,reveal_skippable:false);await completion();}"#,
            encoded,
        );
        models::drive(&mut vm, &mut session, false);
        let id = session.view().interaction.unwrap().id();
        assert_eq!(
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(id))
                .unwrap(),
            crate::InputOutcome::Pending
        );
        assert!(
            matches!(session.view().interaction,Some(crate::InteractionView::Dialogue(d)) if !d.advance_requested())
        );
        session
            .advance_reveal(
                session.root_owner(),
                id,
                std::time::Duration::from_secs(10),
                30.,
            )
            .unwrap();
        assert!(
            matches!(session.view().interaction,Some(crate::InteractionView::Dialogue(d)) if !d.advance_requested())
        );
        assert_eq!(
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(id))
                .unwrap(),
            crate::InputOutcome::Pending
        );
        assert!(
            matches!(session.view().interaction,Some(crate::InteractionView::Dialogue(d)) if d.advance_requested())
        );
    }
}
