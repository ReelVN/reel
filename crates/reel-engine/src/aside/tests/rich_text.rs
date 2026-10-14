use super::*;
use reel_ui::stage::{NodeKind, NodeLink};

#[test]
fn published_rich_nvl_example_retains_styled_lines_and_activates_the_glossary() {
    let code = include_str!("../../../../../examples/author_rich_text.aside")
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
fn styled_dialogue_history_reveal_and_owned_links_from_source_and_asb() {
    let code = r##"
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
        try { original.plain = "changed"; } catch e { readonly = true; }
        if !readonly { throw "mutable rich text"; }
        state.value = original;
        let label = text(value: bind(state.value), reveal: bind(d.reveal), line_height: 1.6, text_align: "center",
            on_link: |e| { state.clicked = e.id; state.value = rich("[link=star]Replacement[/link]"); });
        stage.replace(column() { label; text(value: bind(state.clicked)); });
        character("A", id: "a")(original);
        if h[0].text.plain != original.plain || h[0].lines[0].text.plain != original.plain
            || d.text.plain != original.plain { throw "history lost styles"; }
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
