use super::*;
use crate::{InputOutcome, InteractionView};
use reel_ui::stage::NodeKind;
use std::time::Duration;

fn has_text(vm: &SessionVm, value: &str) -> bool {
    vm.stage()
        .nodes()
        .any(|node| node.kind == NodeKind::Text(value.into()))
}

fn reveal(session: &mut Session, seconds: f64) {
    let id = session.view().interaction.unwrap().id();
    session
        .advance_reveal(
            session.root_owner(),
            id,
            Duration::from_secs_f64(seconds),
            10.,
        )
        .unwrap();
}

fn advance(session: &mut Session) -> InputOutcome {
    let id = session.view().interaction.unwrap().id();
    session
        .handle_local_input(session.root_owner(), InputAction::Advance(id))
        .unwrap()
}

fn dispatch_entry(vm: &mut SessionVm, session: &mut Session, expected: &str) {
    for _ in 0..100_000 {
        match vm
            .step(
                session,
                SessionBudget::RetryMinimum(SliceBudget {
                    instructions: 1,
                    work: 16,
                    ..Default::default()
                }),
                || 0,
            )
            .unwrap()
        {
            SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                let package = vm.vm.program().package();
                let symbol =
                    &package.symbols[package.natives[ticket.native.index()].symbol.index()];
                let reached = symbol == expected
                    && matches!(ticket.input, aside_vm::executor::NativeInput::Entry);
                vm.dispatch(session, ticket, "en", NativeBudget::Exact(32))
                    .unwrap();
                if reached {
                    return;
                }
            }
            SessionStep::Vm(RunOutcome::BudgetYield(_)) | SessionStep::Progress => {}
            unexpected => panic!("did not reach {expected} entry: {unexpected:?}"),
        }
    }
    panic!("entry boundary did not settle: {expected}");
}

#[test]
fn speech_chain_keeps_receiver_playback_and_each_advancement_checkpoint_source_asb() {
    let code = r#"use reel::types::Speech;
        fn opening() {
            let marker=text("playing");stage.replace(marker);
            let line:Speech=narrator(rich("A[pause=0.5]"),wait:false,reveal_skippable:false);
            if line.revealed()!=line || line.revealed()!=line {throw "reveal changed receiver";}
            marker.value="played";
            if line.append("B",wait:false)!=line {throw "append changed receiver";}
            let continued:Speech=line.wait().append("\nC");
            if continued!=line {throw "waiting chain changed receiver";}
            marker.value="advanced";
        }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, true);
        let id = session.view().interaction.unwrap().id();
        assert!(matches!(
            vm.pending.as_ref().unwrap().kind,
            PendingInteractionKind::Reveal
        ));
        assert_eq!(advance(&mut session), InputOutcome::Pending);
        reveal(&mut session, 0.1);
        models::drive(&mut vm, &mut session, true);
        assert!(has_text(&vm, "playing"));
        assert!(session.read_progress().ids().next().is_none());
        assert!(matches!(session.view().interaction,
            Some(InteractionView::Dialogue(d)) if d.id()==id && d.lines()[0].visible_text()=="A"
                && !d.lines()[0].playback_complete() && !d.awaiting_advance()));
        reveal(&mut session, 0.6);
        models::drive(&mut vm, &mut session, true);
        assert!(has_text(&vm, "played"));
        assert!(matches!(session.view().interaction,
            Some(InteractionView::Dialogue(d)) if d.id()==id && d.text()=="AB" && d.awaiting_advance()));
        assert!(session.read_progress().ids().next().is_none());
        reveal(&mut session, 0.2);
        assert_eq!(advance(&mut session), InputOutcome::Completed);
        models::drive(&mut vm, &mut session, true);
        assert_eq!(session.read_progress().ids().count(), 2);
        assert!(matches!(session.view().interaction,
            Some(InteractionView::Dialogue(d)) if d.id()==id && d.text()=="AB\nC" && d.awaiting_advance()));
        assert!(!has_text(&vm, "advanced"));
        reveal(&mut session, 0.3);
        assert_eq!(advance(&mut session), InputOutcome::Completed);
        models::drive(&mut vm, &mut session, true);
        assert!(has_text(&vm, "advanced"));
        assert_eq!(session.read_progress().ids().count(), 3);
        assert_eq!(session.view().history.len(), 1);
        assert_eq!(
            session.view().history[0].lines()[0].resolved_text(),
            "AB\nC"
        );
        vm.finish(&mut session).unwrap();
    }
}

#[test]
fn speech_reveal_wait_observes_manual_reveal_without_advancing_or_marking_read_source_asb() {
    let code = r#"fn opening() {
        let line=narrator("Reveal this",wait:false);
        line.revealed();
        stage.replace(text("revealed"));await completion();
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        assert_eq!(advance(&mut session), InputOutcome::Revealed);
        models::drive(&mut vm, &mut session, true);
        assert!(has_text(&vm, "revealed"));
        assert_eq!(advance(&mut session), InputOutcome::Pending);
        assert!(!session.completion_pending());
        assert!(session.read_progress().ids().next().is_none());
        assert!(matches!(session.view().interaction,
            Some(InteractionView::Dialogue(d)) if !d.awaiting_advance() && d.advance_requested()));
        vm.revoke(&mut session).unwrap();
    }
}

#[test]
fn speech_reveal_and_advance_waits_restore_without_replaying_append_source_asb() {
    let code = r#"fn opening() {
        let history=game.history();
        let line=narrator(rich("A[pause=0.5]"),wait:false,reveal_skippable:false);
        let same=line.revealed().wait().append("B");
        if same!=line || history.len()!=1 || history[0].text.plain!="AB" {
            throw "restored chain replayed a segment";
        }
        stage.replace(text("restored"));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, true);
        let generation = session.view().interaction.unwrap().id().generation();
        (session, vm) = scopes::restored(&mut vm, &session);
        reveal(&mut session, 1.);
        models::drive(&mut vm, &mut session, true);
        assert!(matches!(session.view().interaction,
            Some(InteractionView::Dialogue(d)) if d.id().generation()==generation && d.text()=="A"
                && d.awaiting_advance()));
        (session, vm) = scopes::restored(&mut vm, &session);
        assert_eq!(advance(&mut session), InputOutcome::Completed);
        models::drive(&mut vm, &mut session, true);
        assert!(matches!(session.view().interaction,
            Some(InteractionView::Dialogue(d)) if d.id().generation()==generation && d.text()=="AB"
                && d.awaiting_advance()));
        (session, vm) = scopes::restored(&mut vm, &session);
        reveal(&mut session, 0.2);
        assert_eq!(advance(&mut session), InputOutcome::Completed);
        models::drive(&mut vm, &mut session, true);
        assert!(has_text(&vm, "restored"));
        vm.finish(&mut session).unwrap();
    }
}

#[test]
fn speech_entry_continuations_save_before_reveal_or_advance_is_armed_source_asb() {
    let code = r#"fn opening() {
        let line=narrator(rich("A[pause=0.5]"),wait:false,reveal_skippable:false);
        if line.revealed().append("B")!=line {throw "saved receiver";}
        stage.replace(text("boundary restored"));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        dispatch_entry(&mut vm, &mut session, "reel::speech::revealed");
        assert!(vm.pending.is_none());
        assert!(matches!(session.view().interaction,
            Some(InteractionView::Dialogue(d)) if !d.awaiting_advance()));
        (session, vm) = scopes::restored(&mut vm, &session);
        models::drive(&mut vm, &mut session, true);
        assert!(matches!(
            vm.pending.as_ref().unwrap().kind,
            PendingInteractionKind::Reveal
        ));
        reveal(&mut session, 1.);
        dispatch_entry(&mut vm, &mut session, "reel::speech::append");
        assert!(vm.pending.is_none());
        assert!(matches!(session.view().interaction,
            Some(InteractionView::Dialogue(d)) if d.text()=="AB" && !d.awaiting_advance()));
        assert!(session.read_progress().ids().next().is_none());
        (session, vm) = scopes::restored(&mut vm, &session);
        models::drive(&mut vm, &mut session, true);
        assert!(matches!(session.view().interaction,
            Some(InteractionView::Dialogue(d)) if d.text()=="AB" && d.awaiting_advance()));
        reveal(&mut session, 0.2);
        assert_eq!(advance(&mut session), InputOutcome::Completed);
        models::drive(&mut vm, &mut session, true);
        assert!(has_text(&vm, "boundary restored"));
        assert_eq!(session.view().history[0].lines()[0].resolved_text(), "AB");
        assert_eq!(session.read_progress().ids().count(), 2);
        vm.finish(&mut session).unwrap();
    }
}

#[test]
fn speech_reveal_scope_cancellation_releases_the_wait_after_restore_source_asb() {
    let code = r#"fn opening() {
        let owner=stack();
        stage.replace(column {owner;button("cancel",on_click:||owner.remove());});
        let cancelled={count:0};
        try {scope(owner:owner,body:|| {
            narrator(rich("A[pause=100]"),wait:false).revealed();
            throw "cancelled speech resumed";
        });} catch error {
            if error!="ScopeCancelled" {throw error;}
            cancelled.count+=1;
        }
        if cancelled.count!=1 {throw "cancellation did not propagate once";}
        stage.replace(text("cancelled"));
    }"#;
    for encoded in [false, true] {
        for restore in [false, true] {
            let (mut session, mut vm) = models::prepared(code, encoded);
            models::drive(&mut vm, &mut session, true);
            if restore {
                (session, vm) = scopes::restored(&mut vm, &session);
            }
            let wait = vm.pending.as_ref().unwrap().wait;
            let cancel = vm
                .stage()
                .nodes()
                .find_map(|node| match &node.kind {
                    NodeKind::Button {
                        label: Some(label), ..
                    } if label == "cancel" => Some(node.id),
                    _ => None,
                })
                .unwrap();
            assert!(vm.click(&mut session, cancel).unwrap());
            models::drive(&mut vm, &mut session, true);
            assert!(!vm.vm.wait_is_pending(wait));
            assert!(vm.pending.is_none());
            assert!(has_text(&vm, "cancelled"));
            assert!(session.read_progress().ids().next().is_none());
            vm.finish(&mut session).unwrap();
        }
    }
}

#[test]
fn speech_component_composes_playback_and_visual_activity_waits_source_asb() {
    let code = r#"use reel::types::{Speech,Node,Activity};
        class AnimatedSpeech {
            let speech:Speech;
            let appearance:Activity;
            fn init(self,speech:Speech,node:Node) {
                self.speech=speech;
                self.appearance=tween(node,reveal_fade:1,duration:2).run(owner:node);
            }
            fn revealed(self)->AnimatedSpeech {
                self.speech.revealed();await self.appearance;self
            }
            fn wait(self)->AnimatedSpeech {self.revealed();self.speech.wait();self}
        }
        fn opening() {
            let dialogue=game.dialogue();let marker=text("playing");
            let body=text(value:bind(dialogue.text),reveal:bind(dialogue.reveal),reveal_fade:0);
            stage.replace(column {body;marker;});
            let line=AnimatedSpeech(narrator("A",wait:false),body);
            if line.revealed()!=line {throw "component identity";}
            marker.value="visual complete";
            if line.wait()!=line {throw "component wait identity";}
            marker.value="advanced";
        }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, true);
        reveal(&mut session, 1.);
        models::drive(&mut vm, &mut session, true);
        assert!(has_text(&vm, "playing"));
        assert!(
            vm.pending.is_none(),
            "text playback ended before the author's visual activity"
        );
        assert!(matches!(session.view().interaction,
            Some(InteractionView::Dialogue(d)) if d.lines()[0].playback_complete() && !d.awaiting_advance()));
        activities::advance_clock(&mut vm, &session, 1., 32);
        models::drive(&mut vm, &mut session, true);
        assert!(has_text(&vm, "playing"));
        (session, vm) = scopes::restored(&mut vm, &session);
        activities::advance_clock(&mut vm, &session, 2., 32);
        models::drive(&mut vm, &mut session, true);
        assert!(has_text(&vm, "visual complete"));
        assert!(matches!(session.view().interaction,
            Some(InteractionView::Dialogue(d)) if d.awaiting_advance()));
        assert!(session.read_progress().ids().next().is_none());
        assert_eq!(advance(&mut session), InputOutcome::Completed);
        models::drive(&mut vm, &mut session, true);
        assert!(has_text(&vm, "advanced"));
        vm.finish(&mut session).unwrap();
    }
}
