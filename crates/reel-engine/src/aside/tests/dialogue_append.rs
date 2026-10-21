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
