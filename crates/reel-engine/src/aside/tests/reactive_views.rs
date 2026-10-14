use super::*;
use reel_ui::stage::NodeKind;

#[test]
fn derived_and_conditional_views_share_model_dependencies() {
    let code = r##"
    fn opening() {
        let data = model({left: true, a: 3, b: 9, items: [1, 2]});
        let value = derive(|| { let mut total = 0; for x in data.items { total += x; } total + if data.left { data.a } else { data.b } });
        let caption = text(value: derive(|| f"value {value.value}"));
        let page = view(|| { if data.left { text("left") } else { text("right") } });
        let control = button("update", on_click: |e| { data.left = false; data.b = 20; data.items.append(4); });
        stage.replace(column() { caption; page; control; });
        narrator("ready");
    }"##;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, true);
        assert!(
            vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("value 6".into()))
        );
        let button = vm
            .stage()
            .nodes()
            .find(|n| matches!(n.kind, NodeKind::Button { .. }))
            .unwrap()
            .id;
        assert!(vm.click(&mut session, button).unwrap());
        models::drive(&mut vm, &mut session, true);
        assert!(
            vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("value 27".into()))
        );
        assert!(
            vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("right".into()))
        );
    }
}

#[test]
fn narrative_models_compose_nvl_clear_pages_and_select_original_keys() {
    let code = r##"
    let dialogue = game.dialogue();
    let choices = game.choice();
    fn opening() {
        game.configure(default_dialogue: false, default_choices: false);
        let page = view(|| column() { for line in dialogue.lines { text(value: bind(line.text), reveal: bind(line.reveal)); } });
        let options = view(|| column() { for option in choices.options { button(option.label, on_click: |e| {
            if !choices.select(option.id) { throw "valid selection rejected"; }
            if choices.select(option.id) { throw "duplicate selection accepted"; }
        }); } });
        stage.replace(column() { page; options; });
        narrator("first");
        narrator("second");
        if dialogue.lines.len() != 2 { throw "NVL rows lost"; }
        let answer = choose([{key: "keep", label: "Keep"}, {key: "clear", label: "Clear"}]);
        if answer != "clear" { throw "wrong key"; }
        if choices.options.len() != 0 { throw "completed options retained"; }
        dialogue.clear();
        if dialogue.lines.len() != 0 { throw "page not cleared"; }
        narrator("third");
    }"##;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, true);
        for expected in ["first", "second"] {
            let Some(crate::InteractionView::Dialogue(dialogue)) = session.view().interaction
            else {
                panic!("dialogue")
            };
            assert_eq!(dialogue.text(), expected);
            let id = dialogue.id();
            let owner = session.root_owner();
            session
                .handle_local_input(owner, crate::InputAction::Advance(id))
                .unwrap();
            models::drive(&mut vm, &mut session, true);
            assert!(
                vm.stage()
                    .nodes()
                    .any(|node| node.kind == NodeKind::Text(expected.into())
                        && node.props.reveal == Some(expected.len()))
            );
            session
                .handle_local_input(owner, crate::InputAction::Advance(id))
                .unwrap();
            models::drive(&mut vm, &mut session, true);
        }
        assert!(
            vm.stage()
                .nodes()
                .any(|node| node.kind == NodeKind::Text("first".into()))
        );
        let button = vm.stage().nodes().find(|node| matches!(&node.kind, NodeKind::Button { label: Some(label), .. } if label == "Clear")).unwrap().id;
        vm.click(&mut session, button).unwrap();
        models::drive(&mut vm, &mut session, true);
        assert!(
            vm.stage()
                .nodes()
                .any(|node| node.kind == NodeKind::Text("third".into()))
        );
        assert!(
            !vm.stage()
                .nodes()
                .any(|node| node.kind == NodeKind::Text("first".into()))
        );
        assert_eq!(session.view().history.len(), 3);
    }
}

#[test]
fn reactive_computation_allows_private_work_and_rejects_effects_and_cycles() {
    let code = r##"
    fn mutate(value) { value.n = 40; }
    fn wait() { narrator("forbidden"); }
    fn opening() {
        let data = model({n: 2});
        let temporary = derive(|| {
            let values = [1]; values.append(2); values.reverse();
            let local = {n: 3}; local.n += values[0];
            local.n + data.n
        });
        if temporary.value != 7 { throw "private computation"; }
        let mut rejected = 0;
        try { derive(|| mutate(data)); } catch e { rejected += 1; }
        try { derive(wait); } catch e { rejected += 1; }
        try { slider(value: temporary); } catch e { rejected += 1; }
        let node = text("cycle", x: 3);
        let cyclic = derive(|| node.x + 1);
        try { node.x = cyclic; } catch e { rejected += 1; }
        try { view(|| { let n = data.n; data.n = n + 1; text("cycle"); }); } catch e { rejected += 1; }
        if rejected != 5 || data.n != 2 || node.x != 3 { throw "effect boundary"; }
        stage.replace(column() { text("pure"); text(value: derive(|| game.dialogue().text)); });
        narrator("ready");
    }"##;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, true);
        assert!(
            vm.stage()
                .nodes()
                .any(|node| node.kind == NodeKind::Text("pure".into()))
        );
        assert!(
            vm.stage()
                .nodes()
                .any(|node| node.kind == NodeKind::Text("ready".into()))
        );
    }
}

#[test]
fn conditional_dependencies_and_rebinding_drop_old_sources() {
    let code = r##"
    fn opening() {
        let data = model({left: true, a: 2, b: 8});
        let plain = {offset: 0};
        let shown = text("result", x: derive(|| if data.left { data.a + plain.offset } else { data.b + plain.offset }));
        let target = text("target", x: bind(data.a));
        let follower = text("follower", x: derive(|| target.x + 1));
        stage.replace(column() {
            shown; target; follower;
            button("switch", on_click: |e| { data.left = false; target.x = bind(data.b); });
            button("unrelated", on_click: |e| { plain.offset = 100; data.a = 4; });
            button("related", on_click: |e| { data.b = 9; });
        });
        narrator("ready");
    }"##;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, true);
        for (label, expected, following) in [
            ("switch", 8., 9.),
            ("unrelated", 8., 9.),
            ("related", 109., 10.),
        ] {
            let button = vm.stage().nodes().find(|n| matches!(&n.kind, NodeKind::Button { label: Some(text), .. } if text == label)).unwrap().id;
            vm.click(&mut session, button).unwrap();
            models::drive(&mut vm, &mut session, true);
            for (caption, value) in [("result", expected), ("follower", following)] {
                assert_eq!(
                    vm.stage()
                        .nodes()
                        .find(|n| n.kind == NodeKind::Text(caption.into()))
                        .unwrap()
                        .props
                        .x,
                    value
                );
            }
        }
    }
}

#[test]
fn cancellation_during_initial_computation_releases_capture_and_frames() {
    let code = "fn opening() { let result = derive(|| { let mut n = 0; while n < 10000 { n += 1; } n }); text(value: result); }";
    let (mut session, mut vm) = models::prepared(code, false);
    loop {
        if let SessionStep::Vm(RunOutcome::HostCall(ticket)) = vm
            .step(
                &mut session,
                SliceBudget {
                    instructions: 1,
                    ..Default::default()
                },
                || 0,
            )
            .unwrap()
        {
            vm.dispatch(&mut session, ticket, "en", 1).unwrap();
            if vm.vm.heap().reactive_evaluating() {
                break;
            }
        }
    }
    collect(&mut vm);
    vm.revoke(&mut session).unwrap();
    while vm.cleanup_pending() {
        vm.cleanup_step(&mut session, 32, SliceBudget::default(), || 0)
            .unwrap();
    }
    assert!(!vm.vm.heap().reactive_evaluating());
}
