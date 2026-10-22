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
fn retaining_history_rows_preserves_partial_groups_views_and_restored_aliases() {
    let code = r#"
    let history = game.history();
    fn dynamic(value) {value}
    fn opening() {
        let mio = character("Mio");
        let d = game.dialogue();
        game.record("discarded group");
        say([mio.line("discarded row"), mio.line("unrecorded", history: false),
             mio.line("retained first"), mio.line("retained second")]);
        game.record("retained last");
        let group = history[1];
        let lines = group.lines;
        let first = lines[1];
        let second = lines[2];
        let last = history[2];
        stage.replace(column() {
            list(|| history, key: |group| group.group_id, build: |group| {
                list(|| group.value.lines, key: |line| line.id,
                     build: |line| text(line.value.text))
            });
            text(value: derive(|| f"first {group.text}; rows {lines.len()}"));
            button("retain", on_click: || {
                let mut errors = 0;
                try { game.reading().clear_history(-1); } catch e { errors += 1; }
                try { game.reading().clear_history(dynamic(1.5)); } catch e { errors += 1; }
                try { game.reading().clear_history(dynamic(true)); } catch e { errors += 1; }
                if errors != 3 || history.len() != 3 || lines.len() != 3 {
                    throw "invalid retention changed history";
                }
                game.reading().clear_history(keep_last: 3);
                game.reading().clear_history(999);
                game.reading().clear_history(3);
            });
        });
        narrator("waiting", history: false);
        if history != game.history() || history.len() != 2 || history[0] != group
            || history[1] != last || group.lines != lines || lines.len() != 2
            || lines[0] != first || lines[1] != second || group.text != first.text
            || group.speaker_name != first.speaker_name || d.lines.len() != 5
            || first.id != d.lines[2].id || second.id != d.lines[3].id {
            throw "retained identity, sparse index, NVL or restore";
        }
        game.record("new group");
        if history.len() != 3 || history[2].group_id <= last.group_id {
            throw "history returned or group ID reused";
        }
        narrator("finished", history: false);
    }
    "#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, true);
        advance(&mut vm, &mut session);
        let active = session.view().interaction.unwrap().id();
        let kept = ["retained first", "retained second", "retained last"].map(|text| {
            vm.stage()
                .nodes()
                .find(|n| n.kind == NodeKind::Text(text.into()))
                .unwrap()
                .id
        });
        let removed = ["discarded group", "discarded row"].map(|text| {
            vm.stage()
                .nodes()
                .find(|n| n.kind == NodeKind::Text(text.into()))
                .unwrap()
                .id
        });
        let button = vm.stage().nodes().find(|node| {
            matches!(&node.kind, NodeKind::Button { label: Some(label), .. } if label == "retain")
        }).unwrap().id;
        assert!(vm.click(&mut session, button).unwrap());
        models::drive(&mut vm, &mut session, true);
        collect(&mut vm);
        assert_eq!(session.view().interaction.unwrap().id(), active);
        assert_eq!(session.view().history.len(), 2);
        assert_eq!(session.view().history[0].lines()[0].id().index(), 2);
        assert!(
            kept.iter().all(|&id| vm.stage().node(id).is_some()),
            "kept {kept:?}, current {:?}",
            vm.stage()
                .nodes()
                .map(|n| (n.id, &n.kind))
                .collect::<Vec<_>>()
        );
        assert!(removed.iter().all(|&id| vm.stage().node(id).is_none()));
        assert!(
            vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("first retained first; rows 2".into()))
        );
        // A partial group's original row/array aliases survive graph restore.
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
        advance(&mut vm, &mut session);
        assert_eq!(
            session
                .view()
                .history
                .iter()
                .map(|g| g.lines().len())
                .sum::<usize>(),
            4
        );
        let Some(crate::InteractionView::Dialogue(dialogue)) = session.view().interaction else {
            panic!()
        };
        assert_eq!(dialogue.text(), "finished");
    }
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
                game.reading().clear_history();
                game.reading().clear_history();
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
    session.clear_history(0);
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
fn successive_retention_within_the_same_group_preserves_its_projection() {
    let code = r#"
        fn opening() {
            let mio = character("Mio");
            let history = game.history();
            say([mio.line("one"), mio.line("two"), mio.line("three")]);
            let group = history[0];
            let lines = group.lines;
            let last = lines[2];
            game.reading().clear_history(2);
            if group.text != "two" || lines.len() != 2 { throw "first trim"; }
            game.reading().clear_history(1);
            if history.len() != 1 || history[0] != group || group.lines != lines
                || lines.len() != 1 || lines[0] != last || group.text != "three" {
                throw "same group trim lost identity";
            }
            narrator("finished", history: false);
        }
    "#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, true);
        advance(&mut vm, &mut session);
        assert_eq!(session.view().history[0].lines().len(), 1);
        assert_eq!(session.view().history[0].lines()[0].id().index(), 2);
    }
}

#[test]
fn independent_history_keeps_display_identity_callbacks_and_snapshot_continuations() {
    let code = r#"
    fn dynamic(value) {value}
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
        try { mio("bad", history: dynamic(42)); } catch e { rejected += 1; }
        try { narrator("bad", history: dynamic([])); } catch e { rejected += 1; }
        try { mio.line("bad", history: dynamic(null)); } catch e { rejected += 1; }
        try { game.record("bad", speaker: dynamic({})); } catch e { rejected += 1; }
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
