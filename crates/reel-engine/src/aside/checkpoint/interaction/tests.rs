//! Real source and ASB loads preserve old reading contents in current views.
use super::*;
use crate::{
    Epoch, InputAction, InteractionView, SessionId,
    aside::{
        RunOutcome, SessionBudget, SessionStep, SliceBudget, VmLimits,
        checkpoint::{CheckpointLoad, CheckpointRecord},
        source,
    },
};
use aside_vm::bytecode as bc;

const HEADER: &str = r#"
use std::resume::{state,root,flow,point};
class World { let mut reward:int=0; }
state(World,"world");root(main,"game");
"#;

fn program(body: &str, boot: &str) -> String {
    format!(
        r#"{HEADER}
        fn main(state:World=model(World())){{reading(||{{
            let dialogue=game.dialogue();let history=game.history();let choice=game.choice();
            stage.replace(stack(){{text(derive(||dialogue.text));}});
            {boot}
            run_flow(story,state)
        }})}}
        fn story(state:World){{flow("story");{body}}}
    "#
    )
}

fn loaded(code: &str, version: u64, encoded: bool) -> (Session, SessionVm) {
    let mut story = source::compile_story(code, "reading_resume", version, "main").unwrap();
    if encoded {
        story.program = bc::VerifiedProgram::decode(
            &story
                .program
                .encode(Default::default())
                .unwrap()
                .into_bytes(),
            Default::default(),
        )
        .unwrap();
    }
    let initializer = story.initializer;
    let session = Session::new(SessionId(632), Epoch(version)).unwrap();
    let mut vm = SessionVm::load_story(&session, story, VmLimits::default()).unwrap();
    vm.start(&session, initializer).unwrap();
    (session, vm)
}

fn next(vm: &mut SessionVm, session: &mut Session) -> RunOutcome {
    for _ in 0..100_000 {
        match vm
            .step(
                session,
                SessionBudget::RetryMinimum(SliceBudget {
                    instructions: 1,
                    ..Default::default()
                }),
                || 0,
            )
            .unwrap()
        {
            SessionStep::Vm(RunOutcome::BudgetYield(_)) | SessionStep::Progress => {}
            SessionStep::Vm(outcome) => return outcome,
            other => panic!("unexpected reading step {other:?}"),
        }
    }
    panic!("reading fixture exceeded its work budget")
}

fn settle(vm: &mut SessionVm, session: &mut Session) -> RunOutcome {
    loop {
        match next(vm, session) {
            RunOutcome::HostCall(ticket) => {
                vm.dispatch(session, ticket, "en", NativeBudget::RetryMinimum(128))
                    .unwrap();
            }
            outcome => {
                vm.sync_reading(session).unwrap();
                return outcome;
            }
        }
    }
}

fn started(code: &str, encoded: bool) -> (Session, SessionVm) {
    let (mut session, mut vm) = loaded(code, 1, encoded);
    assert!(matches!(
        settle(&mut vm, &mut session),
        RunOutcome::Returned(_)
    ));
    vm.discard_delivery(&mut session).unwrap();
    vm.start_root(&session, None).unwrap();
    assert!(matches!(
        settle(&mut vm, &mut session),
        RunOutcome::Waiting(_)
    ));
    (session, vm)
}

fn capture(vm: &mut SessionVm, session: &Session) -> CheckpointRecord {
    let mut capture = vm.capture_checkpoint_begin(session).unwrap();
    assert!(
        !capture
            .step(vm, session, NativeBudget::Exact(0), 1)
            .unwrap()
    );
    let mut steps = 0;
    while !capture
        .step(vm, session, NativeBudget::RetryMinimum(1), 1)
        .unwrap()
    {
        steps += 1;
        assert!(steps < 100_000);
    }
    let saved = capture.finish(vm).unwrap();
    let mut bytes = Vec::new();
    saved.record().write_to(&mut bytes).unwrap();
    CheckpointRecord::decode(&bytes).unwrap()
}

fn restore(
    code: &str,
    encoded: bool,
    record: CheckpointRecord,
) -> Result<(Session, SessionVm), CheckpointError> {
    let (mut session, mut vm) = loaded(code, 2, encoded);
    let mut load = CheckpointLoad::new(record, &vm)?;
    for _ in 0..100_000 {
        if load.step(
            &mut vm,
            &mut session,
            "en",
            NativeBudget::RetryMinimum(1),
            1,
            SliceBudget {
                instructions: 1,
                ..Default::default()
            },
            || 0,
        )? {
            return Ok((session, vm));
        }
    }
    panic!("reading load exceeded its work budget")
}

fn advance(session: &mut Session) {
    let id = session.view().interaction.unwrap().id();
    for _ in 0..2 {
        session
            .handle_local_input(session.root_owner(), InputAction::Advance(id))
            .unwrap();
        if session.completion_pending() {
            break;
        }
    }
}

fn dialogue(session: &Session) -> &crate::DialogueInteraction {
    match session.view().interaction.unwrap() {
        InteractionView::Dialogue(dialogue) => dialogue,
        _ => panic!("expected restored dialogue"),
    }
}

#[test]
fn current_root_keeps_old_page_pruned_history_and_partial_reveal_source_asb() {
    let a = program(
        r#"
        state.reward+=5;
        point("previous");narrator("Old visible row");
        game.reading().clear_history();
        point("saved");narrator("A旧正文ABCDE");
        state.reward+=1;state.reward
    "#,
        "",
    );
    let b = program(
        r#"
        state.reward+=500;
        point("previous");narrator("Changed earlier row");
        point("saved");narrator("B changed wording");
        state.reward+=2;state.reward
    "#,
        "game.reading().set_auto(condition:false).pause(true);",
    );
    for encoded in [false, true] {
        let (mut session, mut vm) = started(&a, encoded);
        advance(&mut session);
        assert!(matches!(
            settle(&mut vm, &mut session),
            RunOutcome::Waiting(_)
        ));
        let old_id = session.view().interaction.unwrap().id();
        session
            .reveal_more(session.root_owner(), old_id, 3)
            .unwrap();
        vm.sync_reading(&session).unwrap();
        let record = capture(&mut vm, &session);
        let (mut session, mut vm) = restore(&b, encoded, record).unwrap();
        assert_eq!(dialogue(&session).text(), "A旧正文ABCDE");
        assert_eq!(dialogue(&session).lines()[0].visible_text(), "A旧正");
        assert_eq!(session.view().history.len(), 1);
        assert_eq!(
            session.view().history[0].lines()[0].content().fallback(),
            "A旧正文ABCDE"
        );
        assert!(session.reading().paused());
        assert!(!session.reading().auto_ready());
        let display = vm.checkpoint_display().unwrap();
        assert_eq!(display.lines.len(), 2);
        assert_eq!(display.lines[0].fallback_text(), "Old visible row");
        assert_eq!(display.lines[1].visible_text(), "A旧正");
        assert_ne!(dialogue(&session).id().epoch(), old_id.epoch());
        assert_eq!(dialogue(&session).id().generation(), old_id.generation());
        assert_eq!(
            vm.pending.as_ref().unwrap().interaction,
            dialogue(&session).id()
        );
        session.pause_reading(false);
        advance(&mut session);
        let result = settle(&mut vm, &mut session);
        let RunOutcome::Returned(delivery) = result else {
            panic!("restored story failed: {result:?}");
        };
        assert_eq!(vm.vm.delivery_value(delivery).unwrap().as_int(), Some(7));
    }
}

#[test]
fn choice_reuses_current_key_values_and_preserves_saved_options_source_asb() {
    let a = program(
        r#"
        state.reward+=3;point("choice");
        let result=choose([{key:"left",label:"A left"},{key:"right",label:"A right"}]);
        if result!="right"{throw "wrong saved option";}state.reward
    "#,
        "",
    );
    let b = program(
        r#"
        state.reward+=100;point("choice");
        let result=choose([{key:"right",label:"B right"},{key:"left",label:"B left"}]);
        if result!="right"{throw "wrong resumed key";}state.reward+7
    "#,
        "",
    );
    for encoded in [false, true] {
        let (session, mut vm) = started(&a, encoded);
        let record = capture(&mut vm, &session);
        let (mut session, mut vm) = restore(&b, encoded, record).unwrap();
        let InteractionView::Choice(choice) = session.view().interaction.unwrap() else {
            panic!("choice missing");
        };
        assert_eq!(choice.options()[0].label(), "A left");
        let selected = choice.options()[1].id().clone();
        session
            .handle_local_input(session.root_owner(), InputAction::Select(selected))
            .unwrap();
        let RunOutcome::Returned(delivery) = settle(&mut vm, &mut session) else {
            panic!("choice did not finish");
        };
        assert_eq!(vm.vm.delivery_value(delivery).unwrap().as_int(), Some(10));
    }
}

#[test]
fn saved_choice_key_missing_from_current_options_rejects_load() {
    let a = program(r#"point("choice");choose([{key:"old",label:"Old"}]);0"#, "");
    let b = program(r#"point("choice");choose([{key:"new",label:"New"}]);0"#, "");
    let (session, mut vm) = started(&a, false);
    let record = capture(&mut vm, &session);
    let error = restore(&b, false, record).err().unwrap();
    assert!(
        error.to_string().contains("saved choice key is absent"),
        "{error}"
    );
}

#[test]
fn speech_wait_restores_its_receiver_and_segment_without_replaying_narrator_source_asb() {
    let a = program(
        r#"
        point("speech");let speech=narrator("A staged segment",wait:false);
        state.reward+=9;point("segment");let same=speech.wait();
        if same!=speech{throw "receiver identity";}state.reward
    "#,
        "",
    );
    let b = program(
        r#"
        point("speech");let speech=narrator("B replacement segment",wait:false);
        state.reward+=900;point("segment");let same=speech.wait();
        if same!=speech{throw "restored receiver identity";}state.reward+1
    "#,
        "",
    );
    for encoded in [false, true] {
        let (session, mut vm) = started(&a, encoded);
        let record = capture(&mut vm, &session);
        let (mut session, mut vm) = restore(&b, encoded, record).unwrap();
        assert_eq!(dialogue(&session).text(), "A staged segment");
        assert_eq!(session.view().history.len(), 1);
        advance(&mut session);
        let RunOutcome::Returned(delivery) = settle(&mut vm, &mut session) else {
            panic!("speech did not finish");
        };
        assert_eq!(vm.vm.delivery_value(delivery).unwrap().as_int(), Some(10));
    }
}

#[test]
fn inferred_speech_wait_restores_after_script_edits() {
    let before = program(
        r#"
        state.reward += 9;
        let prepared = stage_speech(state.reward == 9);
        let prior = prepared.speech;
        let speech = if prior != null { prior } else { stage_speech(true).speech };
        let resumed = speech.wait();
        if resumed != speech { throw "receiver identity"; }
        state.reward + 1
    "#,
        "",
    ) + r#"
        fn stage_speech(visible: bool) -> table {
            { speech: if visible { narrator("Original segment.", wait:false) } else { null } }
        }
    "#;
    let after = before
        .replace("Original segment.", "Edited segment.")
        .replace("state.reward + 1", "state.reward + 2");
    for encoded in [false, true] {
        let (session, mut vm) = started(&before, encoded);
        let record = capture(&mut vm, &session);
        let (mut session, mut vm) = restore(&after, encoded, record).unwrap();
        assert_eq!(dialogue(&session).text(), "Original segment.");
        assert_eq!(session.view().history.len(), 1);
        advance(&mut session);
        let RunOutcome::Returned(delivery) = settle(&mut vm, &mut session) else {
            panic!("inferred speech did not finish");
        };
        assert_eq!(vm.vm.delivery_value(delivery).unwrap().as_int(), Some(11));
    }
}

#[test]
fn completed_dialogue_resumes_once_without_requesting_another_advance() {
    let a = program(
        r#"state.reward+=4;point("line");narrator("Completed A");state.reward"#,
        "",
    );
    let b = program(
        r#"state.reward+=400;point("line");narrator("Changed B");state.reward+2"#,
        "",
    );
    let (mut session, mut vm) = started(&a, true);
    advance(&mut session);
    assert!(session.completion_pending());
    let record = capture(&mut vm, &session);
    let (mut session, mut vm) = restore(&b, true, record).unwrap();
    assert!(session.view().interaction.is_none());
    assert!(vm.checkpoint_display().unwrap().lines[0].playback_complete());
    let RunOutcome::Returned(delivery) = settle(&mut vm, &mut session) else {
        panic!("completed wait did not resume");
    };
    assert_eq!(vm.vm.delivery_value(delivery).unwrap().as_int(), Some(6));
}

#[test]
fn cleared_speech_page_stays_cleared_while_restored_wait_reveals_and_advances() {
    let a = program(
        r#"
        point("speech");let speech=narrator("Hidden A",wait:false);game.dialogue().clear();
        point("segment");speech.wait();42
    "#,
        "",
    );
    let b = program(
        r#"
        point("speech");let speech=narrator("Changed B",wait:false);game.dialogue().clear();
        point("segment");speech.wait();43
    "#,
        "",
    );
    let (session, mut vm) = started(&a, false);
    let record = capture(&mut vm, &session);
    let (mut session, mut vm) = restore(&b, false, record).unwrap();
    assert!(vm.checkpoint_display().unwrap().cleared);
    assert!(vm.checkpoint_display().unwrap().lines.is_empty());
    let id = session.view().interaction.unwrap().id();
    session.reveal_more(session.root_owner(), id, 2).unwrap();
    vm.sync_reading(&session).unwrap();
    assert!(vm.checkpoint_display().unwrap().cleared);
    assert!(vm.checkpoint_display().unwrap().lines.is_empty());
    advance(&mut session);
    let RunOutcome::Returned(delivery) = settle(&mut vm, &mut session) else {
        panic!("cleared wait did not resume");
    };
    assert_eq!(vm.vm.delivery_value(delivery).unwrap().as_int(), Some(43));
}
