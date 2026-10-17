use super::*;
use aside_vm::snapshot::SnapshotBudget;
use reel_ui::stage::NodeKind;

fn advance(vm: &mut SessionVm, session: &mut Session) {
    let id = session.view().interaction.unwrap().id();
    for _ in 0..2 {
        session
            .handle_local_input(session.root_owner(), InputAction::Advance(id))
            .unwrap();
    }
    models::drive(vm, session, true);
    collect(vm);
}

#[test]
fn clear_history_preserves_observers_display_and_saved_continuations() {
    let code = r#"
    let history = game.history();
    fn opening() {
        game.record(rich("[b]previous chapter[/b]"));
        let kept = history[0];
        stage.replace(column() {
            text(value: derive(|| f"groups {history.len()}"));
            button("clear", on_click: |event| {
                game.clear_history();
                game.clear_history();
                if history != game.history() || history.len() != 0
                    || kept.text.markup != "[b]previous chapter[/b]" {
                    throw "clear changed identity or retained row";
                }
            });
        });
        narrator("waiting");
        if history.len() != 0 { throw "old history returned"; }
        game.record("next chapter");
        if history.len() != 1 || history[0].text != "next chapter"
            || history[0].group_id == kept.group_id { throw "append after clear"; }
        narrator("end", history: false);
    }
    "#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, true);
        assert_eq!(session.view().history.len(), 2);
        let active = session.view().interaction.unwrap().id();
        let mut capture = vm.snapshot_begin(&session).unwrap();
        while !capture.step(SnapshotBudget { units: 64 }).unwrap().complete {}
        let saved_before_clear = capture.finish().unwrap().encode().unwrap();
        let button = vm.stage().nodes().find(|node| {
            matches!(&node.kind, NodeKind::Button { label: Some(label), .. } if label == "clear")
        }).unwrap().id;
        assert!(vm.click(&mut session, button).unwrap());
        models::drive(&mut vm, &mut session, true);
        assert_eq!(session.view().interaction.unwrap().id(), active);
        assert!(session.view().history.is_empty());
        assert_eq!(session.storage_usage().reserved_history_slots, 0);
        assert_eq!(session.storage_usage().retained_history_text_bytes, 0);
        assert!(
            vm.stage()
                .nodes()
                .any(|node| node.kind == NodeKind::Text("groups 0".into()))
        );
        collect(&mut vm);

        if encoded {
            let program = vm.vm.program().clone();
            // Old saves retain their own history; clearing does not edit them.
            let mut old = snapshot::SessionSnapshot::decode(&saved_before_clear).unwrap();
            let mut restore = SessionVm::restore_begin(
                &mut old,
                program.clone(),
                VmLimits::default(),
                SessionIdentity {
                    session_id: SessionId(43),
                    epoch: Epoch(11),
                },
                Default::default(),
                None,
                None,
            )
            .unwrap();
            while !restore.step(SnapshotBudget { units: 64 }).unwrap().complete {}
            let (old_session, _) = restore.finish().unwrap();
            assert_eq!(old_session.view().history.len(), 2);

            let mut capture = vm.snapshot_begin(&session).unwrap();
            while !capture.step(SnapshotBudget { units: 64 }).unwrap().complete {}
            let bytes = capture.finish().unwrap().encode().unwrap();
            let mut cleared = snapshot::SessionSnapshot::decode(&bytes).unwrap();
            let mut restore = SessionVm::restore_begin(
                &mut cleared,
                program,
                VmLimits::default(),
                SessionIdentity {
                    session_id: SessionId(42),
                    epoch: Epoch(10),
                },
                Default::default(),
                None,
                None,
            )
            .unwrap();
            while !restore.step(SnapshotBudget { units: 64 }).unwrap().complete {}
            (session, vm) = restore.finish().unwrap();
            assert!(session.view().history.is_empty());
        }
        advance(&mut vm, &mut session);
        assert_eq!(session.view().history.len(), 1);
        assert_eq!(session.view().history[0].text(), "next chapter");
        assert!(session.read_progress().ids().next().is_some());
    }
}

#[test]
fn host_history_clear_detects_replacement_of_equal_length() {
    let (mut session, mut vm) = models::prepared(
        r#"
        let history = game.history();
        fn opening() {
            game.record("old chapter");
            let first_id = history[0].group_id;
            narrator("waiting", history: false);
            if history.len() != 1 || history[0].group_id == first_id
                || history[0].text != "replacement" { throw "stale history projection"; }
            narrator("finished", history: false);
        }
    "#,
        true,
    );
    models::drive(&mut vm, &mut session, true);
    let interaction = session.view().interaction.unwrap().id();
    session.clear_history();
    session
        .record_history(
            &[DialogueLineInput {
                speaker_id: None,
                speaker_name: "",
                content_id: "new-chapter",
                text: "replacement".into(),
                history: Default::default(),
            }],
            "en",
        )
        .unwrap();
    vm.sync_narrative(&session).unwrap();
    assert_eq!(session.view().interaction.unwrap().id(), interaction);
    collect(&mut vm);
    advance(&mut vm, &mut session);
    let Some(crate::InteractionView::Dialogue(dialogue)) = session.view().interaction else {
        panic!()
    };
    assert_eq!(dialogue.text(), "finished");
}

#[test]
fn independent_history_keeps_display_identity_callbacks_and_snapshot_continuations() {
    let code = r#"
    fn opening() {
        let mio = character("Mio", id: "mio");
        let later = mio.line("later display", history: rich("[b]later history[/b]"));
        let d = game.dialogue();
        game.record("before dialogue", speaker: mio);
        narrator("unrecorded", history: false);
        let h = game.history();
        if h.len() != 1 || h[0].text != "before dialogue" || h[0].speaker_name != "Mio" {
            throw "standalone history or omitted group";
        }
        d.clear();
        say([mio.line("hidden", history: false), mio.line("same"),
             mio.line("glitch", history: rich("[b]clean[/b]"))]);
        if h.len() != 2 || h[1].lines.len() != 2 || h[1].text != "same"
           || h[1].lines[0].id != d.lines[1].id || h[1].lines[1].id != d.lines[2].id
           || h[1].lines[1].text.plain != "clean" || d.lines[2].text != "glitch" {
            throw "group history alias or wrong line index";
        }
        let mut rejected = 0;
        try { mio("bad", history: 42); } catch e { rejected += 1; }
        try { narrator("bad", history: []); } catch e { rejected += 1; }
        try { mio.line("bad", history: null); } catch e { rejected += 1; }
        try { game.record("bad", speaker: {}); } catch e { rejected += 1; }
        if rejected != 4 || h.len() != 2 { throw "invalid input published history"; }
        stage.replace(button("voice", on_click: |event| {
            game.record(rich("[i]background voice[/i]"), speaker: mio);
        }));
        narrator("flicker", history: "normal");
        if h.len() != 4 || h[2].text != "normal" || h[3].text.plain != "background voice"
           || h[3].speaker_name != "Mio" { throw "background record missing"; }
        say([later]);
        if h.len() != 5 || h[4].text.plain != "later history" { throw "descriptor restore"; }
        narrator("finished", history: false);
    }
    "#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, true);
        assert_eq!(session.view().history.len(), 1);
        advance(&mut vm, &mut session);
        let Some(crate::InteractionView::Dialogue(d)) = session.view().interaction else {
            panic!()
        };
        assert_eq!(d.lines().len(), 3);
        assert_eq!(d.lines()[2].resolved_text(), "glitch");
        let group = &session.view().history[1];
        assert_eq!(group.lines().len(), 2);
        assert_eq!(group.lines()[0].id().index(), 1);
        assert_eq!(group.lines()[1].resolved_text(), "clean");
        assert!(!group.lines()[1].content().styled().spans().is_empty());

        // Capture a waiting group and an unused character.line descriptor, then
        // resume the same control flow without reconstructing or replaying it.
        if encoded {
            let program = vm.vm.program().clone();
            let mut capture = vm.snapshot_begin(&session).unwrap();
            while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
            let bytes = capture.finish().unwrap().encode().unwrap();
            let mut saved = snapshot::SessionSnapshot::decode(&bytes).unwrap();
            let mut restore = SessionVm::restore_begin(
                &mut saved,
                program,
                VmLimits::default(),
                SessionIdentity {
                    session_id: SessionId(42),
                    epoch: Epoch(10),
                },
                Default::default(),
                None,
                None,
            )
            .unwrap();
            while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
            (session, vm) = restore.finish().unwrap();
        }
        advance(&mut vm, &mut session);
        let id = session.view().interaction.unwrap().id();
        let button = vm
            .stage()
            .nodes()
            .find(|node| {
                matches!(&node.kind,
            NodeKind::Button { label: Some(label), .. } if label == "voice")
            })
            .unwrap()
            .id;
        assert!(vm.click(&mut session, button).unwrap());
        models::drive(&mut vm, &mut session, true);
        assert_eq!(session.view().interaction.unwrap().id(), id);
        assert_eq!(session.view().history.len(), 4);
        advance(&mut vm, &mut session);
        advance(&mut vm, &mut session);
        assert_eq!(session.view().history.len(), 5);
        let Some(crate::InteractionView::Dialogue(d)) = session.view().interaction else {
            panic!()
        };
        assert_eq!(d.text(), "finished");
    }
}
