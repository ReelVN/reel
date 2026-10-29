use super::*;
mod writing_mode;

#[test]
fn dialogue_ready_tracks_group_timing_and_interaction_boundaries_from_source_and_asb() {
    use reel_ui::stage::NodeKind;
    use std::time::Duration;

    let code = r#"
        fn opening() {
            let d = game.dialogue();
            stage.replace(text(value: derive(|| f"{d.ready}")));
            say([character("A").line("A"), character("B").line(rich("B[pause=0.5]"))]);
            if !d.lines[0].ready || !d.lines[1].ready { throw "completed rows"; }
            choose([{key: "next", label: "Next"}]);
            d.clear();
            if d.ready || d.lines.len() != 0 { throw "cleared page"; }
            narrator(rich("C[pause=10]"));
        }
    "#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        let project = |vm: &mut SessionVm, session: &mut Session, expected: bool| {
            vm.sync_narrative(session).unwrap();
            models::drive(vm, session, true);
            assert!(
                vm.stage()
                    .nodes()
                    .any(|n| { n.kind == NodeKind::Text(expected.to_string().into()) })
            );
        };
        project(&mut vm, &mut session, false);
        let owner = session.root_owner();
        let id = session.view().interaction.unwrap().id();
        session
            .advance_reveal(owner, id, Duration::from_millis(100), 10.)
            .unwrap();
        project(&mut vm, &mut session, false);
        let Some(crate::InteractionView::Dialogue(d)) = session.view().interaction else {
            panic!()
        };
        assert!(d.lines()[0].playback_complete());
        assert_eq!(d.lines()[1].visible_text(), "B");
        assert!(
            !d.lines()[1].playback_complete(),
            "tail pause outlives visible text"
        );
        session
            .advance_reveal(owner, id, Duration::from_millis(500), 10.)
            .unwrap();
        project(&mut vm, &mut session, true);
        session
            .handle_local_input(owner, InputAction::Advance(id))
            .unwrap();
        project(&mut vm, &mut session, false);
        let Some(crate::InteractionView::Choice(c)) = session.view().interaction else {
            panic!()
        };
        let choice = c.options()[0].id().clone();
        session
            .handle_local_input(owner, InputAction::Select(choice))
            .unwrap();
        project(&mut vm, &mut session, false);
        let id = session.view().interaction.unwrap().id();
        session
            .handle_local_input(owner, InputAction::Advance(id))
            .unwrap();
        project(&mut vm, &mut session, true);
        collect(&mut vm);
        session
            .handle_local_input(owner, InputAction::Advance(id))
            .unwrap();
        project(&mut vm, &mut session, false);
    }
}
use reel_ui::stage::{NodeKind, NodeLink};

#[test]
fn protected_reveal_consumes_input_and_restores_its_timing() {
    use aside_vm::snapshot::SnapshotBudget;
    use std::time::Duration;

    let code = r#"
    fn opening() {
        let a = character("A");
        a(rich("[speed=10]A[pause=1]B[/speed]"), reveal_skippable: false);
        say([a.line("C"), a.line(rich("D[pause=1]"))], reveal_skippable: false);
        narrator("E", reveal_skippable: false);
        narrator("default");
    }
    "#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, true);
        let id = session.view().interaction.unwrap().id();
        let owner = session.root_owner();
        session
            .advance_reveal(owner, id, Duration::from_millis(100), 10.)
            .unwrap();
        assert_eq!(
            session
                .handle_local_input(owner, InputAction::Advance(id))
                .unwrap(),
            crate::InputOutcome::Pending
        );
        let Some(crate::InteractionView::Dialogue(d)) = session.view().interaction else {
            panic!()
        };
        assert_eq!(d.reveal().revealed_count, 1);
        assert!(!d.reveal_skippable());

        let program = vm.vm.program().clone();
        let mut capture = vm.snapshot_begin(&session).unwrap();
        while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let mut image =
            snapshot::SessionSnapshot::decode(&capture.finish().unwrap().encode().unwrap())
                .unwrap();
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
        let (mut restored_session, mut restored) = restore.finish().unwrap();
        for (session, vm) in [
            (&mut session, &mut vm),
            (&mut restored_session, &mut restored),
        ] {
            for expected in ["A", "C", "E"] {
                models::drive(vm, session, true);
                let owner = session.root_owner();
                let Some(crate::InteractionView::Dialogue(d)) = session.view().interaction else {
                    panic!()
                };
                assert!(d.text().starts_with(expected));
                assert!(!d.reveal_skippable());
                let id = d.id();
                assert_eq!(
                    session
                        .handle_local_input(owner, InputAction::Advance(id))
                        .unwrap(),
                    crate::InputOutcome::Pending
                );
                // Completing characters alone cannot skip the rich tail pause.
                session
                    .advance_reveal(owner, id, Duration::from_millis(100), 10.)
                    .unwrap();
                if expected == "C" {
                    assert_eq!(
                        session
                            .handle_local_input(owner, InputAction::Advance(id))
                            .unwrap(),
                        crate::InputOutcome::Pending
                    );
                }
                session
                    .advance_reveal(owner, id, Duration::from_secs(2), 10.)
                    .unwrap();
                assert_eq!(
                    session
                        .handle_local_input(owner, InputAction::Advance(id))
                        .unwrap(),
                    crate::InputOutcome::Completed
                );
            }
            models::drive(vm, session, true);
            let Some(crate::InteractionView::Dialogue(d)) = session.view().interaction else {
                panic!()
            };
            assert!(d.reveal_skippable());
            let id = d.id();
            assert_eq!(
                session
                    .handle_local_input(session.root_owner(), InputAction::Advance(id))
                    .unwrap(),
                crate::InputOutcome::Revealed
            );
        }
    }
}

#[test]
fn text_wrap_validates_reacts_and_restores_from_source_and_asb() {
    use aside_vm::snapshot::SnapshotBudget;

    let code = r#"
    fn dynamic(value) {value}
    fn opening() {
        let state = model({wrap: false});
        let defaulted = text("default");
        let direct = text("direct");
        if !defaulted.text_wrap || !direct.text_wrap { throw "default must wrap"; }
        direct.text_wrap = false;
        let bound = text("bound", text_wrap: bind(state.wrap));
        let derived = text("derived", text_wrap: derive(|| state.wrap));
        let explicit = text("explicit\nline", text_wrap: false);
        let mut rejected = 0;
        try { direct.text_wrap = dynamic(0); } catch e { rejected += 1; }
        try { state.wrap = "bad"; } catch e { rejected += 1; }
        if rejected != 2 || direct.text_wrap || state.wrap { throw "boolean validation"; }
        stage.replace(column() {
            defaulted; direct; bound; derived; explicit;
            button("toggle", on_click: |event| {
                state.wrap = !state.wrap;
                direct.text_wrap = state.wrap;
            });
        });
        narrator("ready");
    }
    "#;
    let assert_wrap = |vm: &SessionVm, expected| {
        for (name, wrap) in [
            ("default", true),
            ("direct", expected),
            ("bound", expected),
            ("derived", expected),
            ("explicit\nline", false),
        ] {
            let node = vm
                .stage()
                .nodes()
                .find(|node| node.kind == NodeKind::Text(name.into()))
                .unwrap();
            assert_eq!(node.props.text_wrap, wrap, "{name}");
        }
    };
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, true);
        assert_wrap(&vm, false);
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
        let (mut restored_session, mut restored) = restore.finish().unwrap();
        for (session, vm) in [
            (&mut session, &mut vm),
            (&mut restored_session, &mut restored),
        ] {
            models::drive(vm, session, true);
            assert_wrap(vm, false);
            let button = vm
                .stage()
                .nodes()
                .find(|node| {
                    matches!(
                        &node.kind, NodeKind::Button { label: Some(label), .. } if label == "toggle"
                    )
                })
                .unwrap()
                .id;
            for expected in [true, false] {
                assert!(vm.click(session, button).unwrap());
                models::drive(vm, session, true);
                assert_wrap(vm, expected);
            }
        }
    }
}

#[test]
fn published_text_wrap_example_projects_from_source_and_asb() {
    let code = include_str!("../../../../../examples/author_text_wrap/src/main.aside")
        .replace("fn main()", "fn opening()");
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(&code, encoded);
        models::drive(&mut vm, &mut session, true);
        let unwrapped: Vec<_> = vm
            .stage()
            .nodes()
            .filter(|node| matches!(node.kind, NodeKind::Text(_)) && !node.props.text_wrap)
            .collect();
        assert_eq!(unwrapped.len(), 2);
        assert!(unwrapped.iter().any(|node| matches!(
            &node.kind, NodeKind::Text(text) if text.contains('\n')
        )));
        assert!(unwrapped.iter().any(|node| matches!(
            node.props.text_fit,
            reel_ui::core::TextFit::Shrink { min_font_size: 12. }
        )));
    }
}

#[test]
fn published_rich_nvl_example_retains_styled_lines_and_activates_the_glossary() {
    let code = include_str!("../../../../../examples/author_rich_text/src/main.aside")
        .replace("fn main()", "fn opening()");
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(&code, encoded);
        for count in 1..=4 {
            models::drive(&mut vm, &mut session, true);
            assert_eq!(session.view().history.len(), count);
            let id = session.view().interaction.unwrap().id();
            session
                .reveal_more(session.root_owner(), id, usize::MAX)
                .unwrap();
            vm.sync_narrative(&session).unwrap();
            models::drive(&mut vm, &mut session, true);
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(id))
                .unwrap();
        }
        models::drive(&mut vm, &mut session, true);
        assert!(vm.has_live_work());
        let node = vm
            .stage()
            .nodes()
            .find(|n| matches!(&n.kind, NodeKind::Text(t) if t.has_links()))
            .unwrap();
        let NodeKind::Text(text) = &node.kind else {
            unreachable!()
        };
        let link = NodeLink {
            node: node.id,
            id: "star-map".into(),
            text: text.clone(),
        };
        assert!(vm.text_link(&mut session, &link).unwrap());
        models::drive(&mut vm, &mut session, true);
        assert!(
            vm.stage()
                .nodes()
                .any(|n| matches!(&n.kind, NodeKind::Text(t) if t.contains("记录恒星的位置")))
        );
        assert!(
            session.view().history[2].lines()[0]
                .content()
                .styled()
                .ruby()
                .len()
                == 1
        );
    }
}

#[test]
fn projected_rich_text_keeps_the_same_immutable_graph_through_gc() {
    use aside_vm::{RootKind, RootOwner};
    use reel_core::text::Text;

    let markup = "[b]Hello[/b] [link=star][ruby=ほし]星[/ruby][/link][pause=0.5]";
    let parsed = Text::parse(markup).unwrap();
    for encoded in [false, true] {
        let (_session, mut vm) = models::prepared("fn opening() {}", encoded);
        for chunk in [1, 3, 64] {
            let heap = vm.vm.heap_mut();
            let source = text::alloc_string(heap, markup, chunk).unwrap();
            let created = super::super::rich_text::create(heap, source, chunk).unwrap();
            let projected = super::super::rich_text::alloc(heap, &parsed, chunk).unwrap();
            let values = heap.alloc_array(&[created, projected]).unwrap();
            let root = heap.add_root(RootOwner(1), RootKind::Host, values).unwrap();
            let handles = [created, projected].map(|value| heap.handle(value).unwrap());

            collect(&mut vm);
            let heap = vm.vm.heap();
            for handle in handles {
                let value = heap.import_handle(handle).unwrap();
                let fields = heap.host_fields(value).unwrap();
                let plain = heap.table_entry(fields, 0).unwrap().1;
                let markup_value = heap.table_entry(fields, 1).unwrap().1;
                assert!(plain.same_bits(heap.host_payload(value, 2).unwrap()));
                assert!(markup_value.same_bits(heap.host_payload(value, 1).unwrap()));
                assert_eq!(
                    text::read_string(heap, plain, chunk).unwrap(),
                    parsed.as_ref()
                );
                assert_eq!(
                    text::read_string(heap, markup_value, chunk).unwrap(),
                    markup
                );
                assert_eq!(
                    super::super::rich_text::len(heap, value).unwrap(),
                    parsed.len()
                );
                assert_eq!(
                    super::super::rich_text::read(heap, value, chunk).unwrap(),
                    parsed
                );
            }
            vm.vm.heap_mut().release_root(root).unwrap();
            collect(&mut vm);
            assert!(
                handles
                    .iter()
                    .all(|&handle| vm.vm.heap().import_handle(handle).is_err())
            );
        }
    }
}

#[test]
fn styled_dialogue_history_reveal_and_owned_links_from_source_and_asb() {
    let code = r##"
    fn dynamic(value) {value}
    let d = game.dialogue(); let h = game.history();
    let state = model({clicked: "", value: rich("[b]initial[/b]")});
    fn opening() {
        let original = rich("[b]Hello[/b] [link=star][ruby=ほし]星[/ruby][/link] [em]空[/em]");
        if state.value.plain != "initial" { throw "rich model default"; }
        if original.plain != "Hello 星 空" || rich(rich_escape("[b]literal")).plain != "[b]literal" {
            throw "plain text/escaping";
        }
        let mut error = false;
        try { rich("星[b]x[/i]"); } catch e { error = true; }
        if !error { throw "invalid markup accepted"; }
        let mut readonly = false;
        try { dynamic(original).plain = "changed"; } catch e { readonly = true; }
        if !readonly { throw "mutable rich text"; }
        readonly = false;
        try { dynamic(original).markup = "changed"; } catch e { readonly = true; }
        if !readonly || rich(original.markup).plain != original.plain {
            throw "rich markup roundtrip or mutability";
        }
        let named = rich("[b]" + rich_escape("A[B]") + ":[/b] " + original.markup);
        if named.plain != "A[B]: Hello 星 空" { throw "styled composition"; }
        state.value = original;
        if state.value.markup != original.markup { throw "model lost markup"; }
        let label = text(value: bind(state.value), reveal: bind(d.reveal), line_height: 1.6, text_align: "center",
            on_link: |e| { state.clicked = e.id; state.value = rich("[link=star]Replacement[/link]"); });
        stage.replace(column() { label; text(value: bind(state.clicked)); });
        character("A", id: "a")(original);
        if h[0].text.plain != original.plain || h[0].lines[0].text.plain != original.plain
            || d.text.plain != original.plain || h[0].text.markup != original.markup {
            throw "history lost styles";
        }
        readonly = false;
        try { dynamic(d.text).plain = "changed"; } catch e { readonly = true; }
        if !readonly { throw "mutable projected plain text"; }
        readonly = false;
        try { dynamic(h[0].text).markup = "changed"; } catch e { readonly = true; }
        if !readonly { throw "mutable projected markup"; }
        narrator(rich("[size=36][color=#aabbcc]Next[/color][/size]"));
        stage.replace(text(rich("[link=end]Done[/link]"), on_link: |e| {
            e.target.on_link = null; e.target.value = "done";
        }));
    }"##;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, true);
        let node = vm
            .stage()
            .nodes()
            .find(|n| matches!(&n.kind, NodeKind::Text(t) if t.has_links()))
            .unwrap();
        let NodeKind::Text(text) = &node.kind else {
            unreachable!()
        };
        assert_eq!(node.props.line_height, 1.6);
        let link = NodeLink {
            node: node.id,
            id: "star".into(),
            text: text.clone(),
        };
        let interaction = session.view().interaction.unwrap().id();
        assert!(
            !vm.text_link(&mut session, &link).unwrap(),
            "unrevealed links are inactive"
        );
        session
            .reveal_more(session.root_owner(), interaction, usize::MAX)
            .unwrap();
        vm.sync_narrative(&session).unwrap();
        models::drive(&mut vm, &mut session, true);
        assert!(vm.text_link(&mut session, &link).unwrap());
        models::drive(&mut vm, &mut session, true);
        assert_eq!(
            session.view().interaction.unwrap().id(),
            interaction,
            "link must not advance narrative"
        );
        assert!(
            !vm.text_link(&mut session, &link).unwrap(),
            "old text snapshot must not activate new links"
        );
        assert!(
            vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("star".into()))
        );
        session
            .handle_local_input(session.root_owner(), InputAction::Advance(interaction))
            .unwrap();
        models::drive(&mut vm, &mut session, true);
        let Some(crate::InteractionView::Dialogue(d)) = session.view().interaction else {
            panic!("next line");
        };
        assert_eq!(d.text(), "Next");
        assert_eq!(
            session.view().history[0].lines()[0].content().styled(),
            &link.text
        );
        collect(&mut vm);
        let id = session.view().interaction.unwrap().id();
        for _ in 0..2 {
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(id))
                .unwrap();
        }
        models::drive(&mut vm, &mut session, true);
        assert!(
            vm.has_live_work(),
            "mounted text handler survives main returning"
        );
        let node = vm
            .stage()
            .nodes()
            .find(|n| matches!(&n.kind, NodeKind::Text(t) if t.has_links()))
            .unwrap();
        let NodeKind::Text(text) = &node.kind else {
            unreachable!()
        };
        let link = NodeLink {
            node: node.id,
            id: "end".into(),
            text: text.clone(),
        };
        assert!(vm.text_link(&mut session, &link).unwrap());
        models::drive(&mut vm, &mut session, true);
        assert!(
            !vm.has_live_work(),
            "removing the last link handler lets the session finish"
        );
    }
}

#[test]
fn font_weight_validates_reacts_and_restores_from_source_and_asb() {
    use aside_vm::snapshot::SnapshotBudget;

    let code = r#"
    fn opening() {
        let state = model({weight: 600});
        let defaulted = text("default");
        let direct = text("direct");
        if defaulted.font_weight != 400 || direct.font_weight != 400 { throw "default weight"; }
        direct.font_weight = 600;
        let bound = text("bound", font_weight: bind(state.weight));
        let derived = text("derived", font_weight: derive(|| state.weight));
        let mut rejected = 0;
        for invalid in [0, 1001, -1, null, true, "bold"] {
            try { direct.font_weight = invalid; } catch e { rejected += 1; }
            try { let _ = text("invalid", font_weight: invalid); } catch e { rejected += 1; }
        }
        try { state.weight = 1001; } catch e { rejected += 1; }
        if rejected != 13 || direct.font_weight != 600 || state.weight != 600 { throw f"weight validation {rejected} {direct.font_weight} {state.weight}"; }
        stage.replace(column() {
            defaulted; direct; bound; derived;
            text("minimum", font_weight: 1);
            text("maximum", font_weight: 1000.0);
            text_input(value: "edit", font_weight: bind(state.weight));
            button("toggle", font_weight: derive(|| state.weight), on_click: |event| {
                state.weight = if state.weight == 600 { 450.5 } else { 600 };
                direct.font_weight = state.weight;
            });
        });
        narrator("ready");
    }
    "#;
    let assert_weight = |vm: &SessionVm, expected| {
        for node in vm.stage().nodes() {
            let weight = match &node.kind {
                NodeKind::Text(name) if *name == "default".into() => 400.,
                NodeKind::Text(name) if *name == "minimum".into() => 1.,
                NodeKind::Text(name) if *name == "maximum".into() => 1000.,
                NodeKind::Text(_) | NodeKind::TextInput { .. } | NodeKind::Button { .. } => {
                    expected
                }
                _ => continue,
            };
            assert_eq!(node.props.font_weight, weight, "{:?}", node.kind);
        }
    };
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, true);
        assert_weight(&vm, 600.);
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
        let (mut restored_session, mut restored) = restore.finish().unwrap();
        for (session, vm) in [
            (&mut session, &mut vm),
            (&mut restored_session, &mut restored),
        ] {
            models::drive(vm, session, true);
            assert_weight(vm, 600.);
            let button = vm
                .stage()
                .nodes()
                .find(|node| {
                    matches!(
                        &node.kind, NodeKind::Button { label: Some(label), .. } if label == "toggle"
                    )
                })
                .unwrap()
                .id;
            for expected in [450.5, 600.] {
                assert!(vm.click(session, button).unwrap());
                models::drive(vm, session, true);
                assert_weight(vm, expected);
            }
        }
    }
}
