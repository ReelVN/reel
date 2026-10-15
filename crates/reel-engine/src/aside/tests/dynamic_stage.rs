use super::*;

fn drive(vm: &mut SessionVm, session: &mut Session) -> RunOutcome {
    loop {
        match next(vm, session) {
            RunOutcome::HostCall(ticket) => {
                if let Err(error) = vm.dispatch(session, ticket, "en", 256) {
                    vm.reject_native(session, ticket, error.native_fault_kind().unwrap())
                        .unwrap();
                }
            }
            other => return other,
        }
    }
}

#[test]
fn bound_node_methods_edit_the_same_tree_and_retire_owned_activities() {
    let code = r#"fn opening() {
        let first = text("first");
        let root = column() { first; };
        stage.replace(root);
        let insert = root.add;
        let part = column() { text("nested"); };
        let moving = run(tween(part, x: 100), owner: part);
        if insert(index: 0, child: part) != part { throw "identity"; }
        let last = root.add(text("last"));
        let mut rejected = 0;
        try { root.add(first); } catch error { rejected += 1; }
        try { last.add(text("leaf child")); } catch error { rejected += 1; }
        try { root.add(text("bad index"), index: -1); } catch error { rejected += 1; }
        try { root.add(text("far index"), index: 99); } catch error { rejected += 1; }
        try { root.add = null; } catch error { rejected += 1; }
        try { insert(receiver: last, child: text("override")); } catch error { rejected += 1; }
        if rejected != 6 { throw "invalid edit accepted"; }
        let remove_part = part.remove;
        narrator("inserted");
        remove_part();
        remove_part();
        let mut cancelled = false;
        try { await moving; } catch error { cancelled = true; }
        if !cancelled { throw "removed target still animates"; }
        first.value = "retained";
        narrator("removed");
        root.clear();
        let mut retired = 0;
        try { first.value = "bad"; } catch error { retired += 1; }
        try { root.add(last); } catch error { retired += 1; }
        if retired != 2 { throw "child resurrected"; }
        insert(text("replacement"));
        narrator("cleared");
        root.remove();
        true
    }"#;
    let (mut session, mut vm) = setup(code, Context::Ordinary);
    let mut root = None;
    for (labels, count) in [
        (vec!["nested", "first", "last"], 5),
        (vec!["retained", "last"], 3),
        (vec!["replacement"], 2),
    ] {
        let outcome = drive(&mut vm, &mut session);
        assert!(matches!(outcome, RunOutcome::Waiting(_)), "{outcome:?}");
        collect(&mut vm);
        vm.sync_stage(&session).unwrap();
        let id = vm.stage().root().unwrap();
        assert_eq!(*root.get_or_insert(id), id);
        // Traverse in authored order instead of the registry's allocation order.
        let mut pending = vec![id];
        let mut texts = Vec::new();
        let mut visited = 0;
        while let Some(id) = pending.pop() {
            let node = vm.stage().node(id).unwrap();
            if let reel_ui::stage::NodeKind::Text(text) = &node.kind {
                texts.push(text.as_ref());
            }
            pending.extend(node.children.iter().rev().copied());
            visited += 1;
        }
        assert_eq!(texts, labels);
        assert_eq!(visited, count);
        let id = session.view().interaction.unwrap().id();
        for _ in 0..2 {
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(id))
                .unwrap();
        }
    }
    let outcome = drive(&mut vm, &mut session);
    assert!(matches!(outcome, RunOutcome::Returned(_)), "{outcome:?}");
    assert_eq!(vm.retain_return(&session).unwrap().as_bool(), Some(true));
    assert!(vm.stage().root().is_none());
    collect(&mut vm);
    vm.sync_stage(&session).unwrap();
    assert_eq!(vm.stage().nodes().count(), 0);
}

#[test]
fn detached_components_reject_cycles_without_changing_either_tree() {
    let code = r#"fn opening() {
        let inner = stack();
        let outer = stack() { inner; };
        let mut rejected = 0;
        try { inner.add(outer); } catch error { rejected += 1; }
        try { outer.add(outer); } catch error { rejected += 1; }
        if rejected != 2 { throw "cycle"; }
        stage.replace(outer);
        narrator("still usable");
    }"#;
    let (mut session, mut vm) = setup(code, Context::Ordinary);
    assert!(matches!(
        drive(&mut vm, &mut session),
        RunOutcome::Waiting(_)
    ));
    collect(&mut vm);
    vm.sync_stage(&session).unwrap();
    let root = vm.stage().node(vm.stage().root().unwrap()).unwrap();
    assert_eq!(root.children.len(), 1);
    assert!(
        vm.stage()
            .node(root.children[0])
            .unwrap()
            .children
            .is_empty()
    );
}

#[test]
fn same_parent_move_preserves_identity_bindings_activities_and_ownership() {
    let source = r#"fn opening() {
        let first = text("first");
        let second = text("second");
        let third = text("third");
        let root = stack { first; second; third; };
        stage.replace(root);
        let job = run(tween(first, x: 80, duration: 2), owner: first);
        let move = root.move;
        if move(first, index: 2) != first { throw "move identity"; }
        root.move(first, index: 2);
        first.value = "moving";
        let mut rejected = 0;
        try { root.move(first, index: -1); } catch e { rejected += 1; }
        try { root.move(first, index: 3); } catch e { rejected += 1; }
        try { root.move(text("detached"), index: 0); } catch e { rejected += 1; }
        let other = stack { text("other"); };
        try { other.move(first, index: 0); } catch e { rejected += 1; }
        try { root.move(root, index: 0); } catch e { rejected += 1; }
        if rejected != 5 { throw "invalid reorder"; }
        narrator("reordered");
        if await job != "Completed" || first.x != 80 { throw "animation survived"; }
        root.move(first, index: 0);
        let pending = run(tween(first, y: 100, duration: 10), owner: first);
        root.move(first, index: 1);
        first.remove();
        let mut cancelled = false;
        try { await pending; } catch e { cancelled = true; }
        if !cancelled { throw "owner still retires"; }
        narrator("removed");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(source, encoded);
        models::drive(&mut vm, &mut session, false);
        let root = vm.stage().root().unwrap();
        let ids = vm.stage().node(root).unwrap().children.clone();
        let labels = |vm: &SessionVm| {
            vm.stage()
                .node(root)
                .unwrap()
                .children
                .iter()
                .map(|id| match &vm.stage().node(*id).unwrap().kind {
                    reel_ui::stage::NodeKind::Text(text) => text.to_string(),
                    _ => panic!("text"),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(labels(&vm), ["second", "third", "moving"]);
        collect(&mut vm);
        activities::advance_clock(&mut vm, &session, 2., 4);
        assert_eq!(vm.stage().node(root).unwrap().children, ids);
        let interaction = session.view().interaction.unwrap().id();
        for _ in 0..2 {
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(interaction))
                .unwrap();
        }
        models::drive(&mut vm, &mut session, false);
        assert_eq!(labels(&vm), ["second", "third"]);
        assert_eq!(vm.stage().node(root).unwrap().children, ids[..2]);
        assert!(!vm.activities_need_time());
    }
}

#[test]
fn same_parent_reorder_public_example_callbacks_keep_the_animation() {
    use reel_ui::stage::NodeKind;
    let source = include_str!("../../../../../examples/author_reorder.aside")
        .replace("fn main()", "fn opening()");
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(&source, encoded);
        assert!(matches!(
            drive(&mut vm, &mut session),
            RunOutcome::Waiting(_)
        ));
        vm.accept_presentation(&session, vm.presentation_request())
            .unwrap();
        assert!(matches!(
            drive(&mut vm, &mut session),
            RunOutcome::Returned(_)
        ));
        vm.discard_delivery(&mut session).unwrap();
        vm.sync_stage(&session).unwrap();
        let blue = vm.stage().nodes().find(|node| matches!(&node.kind, NodeKind::Button {label: Some(label), ..} if label == "Blue card")).unwrap().id;
        let red = vm.stage().nodes().find(|node| matches!(&node.kind, NodeKind::Button {label: Some(label), ..} if label == "Red card")).unwrap().id;
        let cards = vm.stage().node(blue).unwrap().parent.unwrap();
        assert_eq!(vm.stage().node(cards).unwrap().children, [blue, red]);
        for (target, order) in [(blue, [red, blue]), (red, [blue, red]), (blue, [red, blue])] {
            assert!(vm.click(&mut session, target).unwrap());
            models::drive(&mut vm, &mut session, false);
            assert_eq!(vm.stage().node(cards).unwrap().children, order);
        }
        activities::advance_clock(&mut vm, &session, 0.75, 4);
        vm.sync_stage(&session).unwrap();
        assert_eq!(vm.stage().node(blue).unwrap().props.y, 60.);
        assert!(
            vm.activities_need_time(),
            "the original repeating animation survives every reorder"
        );
    }
}
