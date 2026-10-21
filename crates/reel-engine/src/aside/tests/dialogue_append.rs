use super::*;
use crate::{InputOutcome, InteractionView};
use aside_vm::snapshot::SnapshotBudget;

fn restored(session: &Session, vm: &mut SessionVm, epoch: u64) -> (Session, SessionVm) {
    let program = vm.vm.program().clone();
    let progress = session.read_progress().clone();
    let mut capture = vm.snapshot_begin(session).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let mut image =
        snapshot::SessionSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
    let mut restore = SessionVm::restore_begin(
        &mut image,
        program,
        VmLimits::default(),
        SessionIdentity {
            session_id: SessionId(41),
            epoch: Epoch(epoch),
        },
        progress,
        None,
        None,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    restore.finish().unwrap()
}

fn advance(session: &mut Session) {
    let id = session.view().interaction.unwrap().id();
    let owner = session.root_owner();
    if session
        .handle_local_input(owner, InputAction::Advance(id))
        .unwrap()
        == InputOutcome::Revealed
    {
        assert_eq!(
            session
                .handle_local_input(owner, InputAction::Advance(id))
                .unwrap(),
            InputOutcome::Completed
        );
    }
}

#[test]
fn dialogue_append_source_asb_checkpoints_history_prefix_and_restore() {
    let code = r#"
    fn opening() {
        let d = game.dialogue(); let history = game.history();
        let mio = character("Mio", id:"mio");
        let speech = mio(rich("[b]A[/b][pause=0.5]"), wait:false);
        let row = d.lines[0]; let group = history[0]; let recorded = group.lines[0];
        stage.replace(text(value:derive(||group.text)));
        await run(delay(1));
        speech.append("e");
        if row != d.lines[0] || recorded != group.lines[0] || d.lines.len()!=1 || history.len()!=1 {
            throw "append changed identity";
        }
        speech.wait();
        speech.append("́");
        speech.append("\nC",history:false);
        if d.text.plain!="Aé\nC" || group.text.plain!="Aé" || recorded.text!=group.text {
            throw "styled display or history projection";
        }
        speech.wait();
        narrator("replacement");
        let mut rejected=false;
        try { speech.append("stale"); } catch e { rejected=true; }
        if !rejected { throw "old handle survived replacement"; }
        let mixed=say([mio.line("one"),character("Other").line("two")],wait:false);
        rejected=false;
        try { mixed.append("ambiguous"); } catch e { rejected=true; }
        if !rejected { throw "ambiguous append"; }
        mixed.wait();
        stage.replace(stack());
    }
    "#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, true);
        let id = session.view().interaction.unwrap().id();
        assert_eq!(
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(id))
                .unwrap(),
            InputOutcome::Revealed
        );
        assert_eq!(
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(id))
                .unwrap(),
            InputOutcome::Pending
        );
        assert!(session.take_completion().is_none());
        (session, vm) = restored(&session, &mut vm, 20);
        vm.advance_time(&session, 1.0, 10000).unwrap();
        models::drive(&mut vm, &mut session, true);
        let Some(InteractionView::Dialogue(d)) = session.view().interaction else {
            panic!()
        };
        assert_eq!(d.text(), "Ae");
        assert_eq!(d.lines()[0].visible_text(), "A");
        assert!(d.awaiting_advance());
        assert_eq!(session.view().history[0].lines()[0].resolved_text(), "Ae");
        (session, vm) = restored(&session, &mut vm, 21);
        advance(&mut session);
        models::drive(&mut vm, &mut session, true);
        let Some(InteractionView::Dialogue(d)) = session.view().interaction else {
            panic!()
        };
        assert_eq!(d.lines()[0].visible_text(), "Aé");
        assert_eq!(session.read_progress().ids().count(), 2);
        advance(&mut session);
        models::drive(&mut vm, &mut session, true);
        advance(&mut session);
        models::drive(&mut vm, &mut session, true);
        advance(&mut session);
        models::drive(&mut vm, &mut session, true);
        collect(&mut vm);
        vm.finish(&mut session).unwrap();
    }
}

const CLEAR_ASSERTIONS: &str = r#"
fn assert_clear(d, active) {
    if d.lines.len()!=0 || d.current_line_ids.len()!=0 || d.text!="" || d.speaker_name!="" {
        throw "clear retained visible dialogue";
    }
    if d.speaker_id!=null || d.content_id!=null || d.reveal!=0 || d.read || d.ready {
        throw "clear retained display identity or progress";
    }
    if d.active!=active {throw "clear changed interaction ownership";}
}
"#;

#[test]
fn dialogue_clear_completed_adv_nvl_history_and_snapshot() {
    let code = format!(
        r#"{CLEAR_ASSERTIONS}
        fn opening() {{
            let d=game.dialogue();let history=game.history();
            stage.replace(column() {{
                text(value:bind(d.text),reveal:bind(d.reveal));
                view(|| column() {{for row in d.lines {{text(value:bind(row.text));}}}});
            }});
            character("Mio",id:"mio")("Old body");
            let row=d.lines[0];let group=history[0];
            d.clear();assert_clear(d,false);
            if d.advance_requested || history.len()!=1 || history[0]!=group || row.text!="Old body" {{
                throw "clear damaged retained data";
            }}
            stage.replace(column() {{
                text("10月12日");text(value:bind(d.text),reveal:bind(d.reveal));
                view(||column(){{for line in d.lines{{text(value:bind(line.text));}}}});
            }});
            await completion();
        }}"#
    );
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(&code, encoded);
        models::drive(&mut vm, &mut session, true);
        advance(&mut session);
        models::drive(&mut vm, &mut session, true);
        assert!(session.view().interaction.is_none());
        assert_eq!(session.view().history.len(), 1);
        assert_no_old_body(&vm, "Old body");
        (session, vm) = restored(&session, &mut vm, 30);
        models::drive(&mut vm, &mut session, true);
        assert_no_old_body(&vm, "Old body");
        assert!(
            vm.stage()
                .nodes()
                .any(|node| { node.kind == reel_ui::stage::NodeKind::Text("10月12日".into()) })
        );
    }
}

fn assert_no_old_body(vm: &SessionVm, body: &str) {
    assert!(
        !vm.stage()
            .nodes()
            .any(|node| { node.kind == reel_ui::stage::NodeKind::Text(body.into()) })
    );
}

#[test]
fn dialogue_clear_active_wait_preserves_advance_and_cleared_snapshot() {
    let code = format!(
        r#"{CLEAR_ASSERTIONS}
        fn opening() {{
            let d=game.dialogue();let history=game.history();
            stage.replace(column(){{
                text(value:bind(d.text),reveal:bind(d.reveal));
                view(||column(){{for row in d.lines{{text(value:bind(row.text));}}}});
                button("clear",on_click:||{{
                    let group=history[0];d.clear();assert_clear(d,true);
                    if d.advance_requested || history.len()!=1 || history[0]!=group {{
                        throw "clear altered history or retained request";
                    }}
                }});
            }});
            narrator("Waiting body");
            assert_clear(d,false);
            stage.replace(text("resumed"));await completion();
        }}"#
    );
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(&code, encoded);
        models::drive(&mut vm, &mut session, false);
        let id = session.view().interaction.unwrap().id();
        let pending = vm.pending.as_ref().unwrap().wait;
        session
            .handle_local_input(session.root_owner(), InputAction::Advance(id))
            .unwrap();
        models::drive(&mut vm, &mut session, false);
        let clear = vm
            .stage()
            .nodes()
            .find_map(|node| match &node.kind {
                reel_ui::stage::NodeKind::Button {
                    label: Some(label), ..
                } if label == "clear" => Some(node.id),
                _ => None,
            })
            .unwrap();
        assert!(vm.click(&mut session, clear).unwrap());
        models::drive(&mut vm, &mut session, true);
        assert_eq!(vm.pending.as_ref().unwrap().wait, pending);
        assert_eq!(session.view().interaction.unwrap().id(), id);
        assert!(
            matches!(session.view().interaction, Some(InteractionView::Dialogue(d))
            if !d.advance_requested() && d.awaiting_advance())
        );
        session
            .advance_reveal(
                session.root_owner(),
                id,
                std::time::Duration::from_secs(1),
                30.,
            )
            .unwrap();
        models::drive(&mut vm, &mut session, true);
        assert_no_old_body(&vm, "Waiting body");
        (session, vm) = restored(&session, &mut vm, 31);
        models::drive(&mut vm, &mut session, true);
        assert_no_old_body(&vm, "Waiting body");
        advance(&mut session);
        models::drive(&mut vm, &mut session, true);
        assert!(
            vm.stage()
                .nodes()
                .any(|node| { node.kind == reel_ui::stage::NodeKind::Text("resumed".into()) })
        );
        assert_eq!(session.view().history.len(), 1);
    }
}

#[test]
fn dialogue_clear_open_speech_append_restores_same_rows_and_new_dialogue_retires_handle() {
    let code = format!(
        r#"{CLEAR_ASSERTIONS}
        fn opening() {{
            let d=game.dialogue();let history=game.history();
            let speech=character("Mio",id:"mio")("First",wait:false);
            let row=d.lines[0];let id=row.id;let group=history[0];
            d.clear();assert_clear(d,true);
            let done=completion();
            stage.replace(column(){{
                text(value:bind(d.text),reveal:bind(d.reveal));
                button("append",on_click:||done.resolve());
            }});
            await done;
            speech.append(" appended");
            if d.lines.len()!=1 || d.lines[0]!=row || d.current_line_ids[0]!=id || d.text!="First appended" {{
                throw "append did not restore the current row";
            }}
            if d.speaker_name!="Mio" || d.speaker_id!="mio" || d.content_id!=row.content_id || d.advance_requested {{
                throw "append did not restore display identity";
            }}
            if history.len()!=1 || history[0]!=group || group.text!="First appended" {{
                throw "append changed history identity";
            }}
            speech.wait();d.clear();assert_clear(d,true);
            narrator("Replacement",wait:false);
            if d.lines.len()!=1 || d.text!="Replacement" {{throw "new dialogue did not start a new page";}}
            let mut rejected=0;
            try {{speech.append("stale");}}catch error{{rejected+=1;}}
            try {{speech.wait();}}catch error{{rejected+=1;}}
            if rejected!=2 {{throw "replaced speech handle remained valid";}}
            await completion();
        }}"#
    );
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(&code, encoded);
        models::drive(&mut vm, &mut session, true);
        assert_no_old_body(&vm, "First");
        (session, vm) = restored(&session, &mut vm, 32);
        models::drive(&mut vm, &mut session, true);
        let append = vm
            .stage()
            .nodes()
            .find_map(|node| match &node.kind {
                reel_ui::stage::NodeKind::Button {
                    label: Some(label), ..
                } if label == "append" => Some(node.id),
                _ => None,
            })
            .unwrap();
        vm.click(&mut session, append).unwrap();
        models::drive(&mut vm, &mut session, true);
        assert_eq!(
            session.view().history[0].lines()[0].resolved_text(),
            "First appended"
        );
        advance(&mut session);
        models::drive(&mut vm, &mut session, true);
        assert!(
            matches!(session.view().interaction, Some(InteractionView::Dialogue(d)) if d.text()=="Replacement")
        );
    }
}
