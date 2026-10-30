use super::*;

#[test]
fn derived_method_and_inherited_fields_refresh_after_nested_dependencies_change() {
    let code = r#"
    class Base {let mut amount:int=2;}
    class Child:Base {
        let mut forwarded:int=0;
        fn sample(self)->int {self.amount+self.forwarded}
    }
    fn opening(){
        let state=model({source:2});
        let upstream=derive(||state.source*3);
        let data=model(Child());
        data.forwarded=upstream;
        let result=derive(||data.sample());
        stage.replace(column(){
            text(value:derive(||f"{result.value}"));
            button("change",on_click:||{state.source=7;data.amount=5;});
        });
        narrator("ready");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        assert!(
            vm.stage()
                .nodes()
                .any(|node| node.kind == NodeKind::Text("8".into()))
        );
        let button = vm.stage().nodes().find(|node|
            matches!(&node.kind, NodeKind::Button {label:Some(label),..} if label=="change")
        ).unwrap().id;
        vm.click(&mut session, button).unwrap();
        models::drive(&mut vm, &mut session, false);
        assert!(
            vm.stage()
                .nodes()
                .any(|node| node.kind == NodeKind::Text("26".into()))
        );
    }
}
use reel_ui::stage::NodeKind;

#[test]
fn nested_derived_guard_and_index_share_the_same_model_update() {
    let code = r#"
    fn opening(){
        let state=model({index:0,other:0});
        let items=["first","second","third"];
        let visible=derive(||state.index>0);
        stage.replace(column(){
            text(value:derive(||if state.other>=0 && visible.value {items[state.index-1]} else {"hidden"}));
            button("one",on_click:||{state.other+=1;state.index=1;});
            button("two",on_click:||{state.other+=1;state.index=2;});
            button("zero",on_click:||{state.other+=1;state.index=0;});
        });
        narrator("ready");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        for (action, expected) in [
            ("one", "first"),
            ("two", "second"),
            ("one", "first"),
            ("zero", "hidden"),
            ("two", "second"),
            ("zero", "hidden"),
        ] {
            let id = vm
                .stage()
                .nodes()
                .find(|n| matches!(&n.kind,NodeKind::Button{label:Some(s),..}if s==action))
                .unwrap()
                .id;
            vm.click(&mut session, id).unwrap();
            models::drive(&mut vm, &mut session, false);
            assert!(
                vm.stage()
                    .nodes()
                    .any(|n| n.kind == NodeKind::Text(expected.into()))
            );
        }
    }
}

#[test]
fn nested_derived_strip_uses_current_guard_after_hidden_preview_reopens() {
    let code = r#"fn opening(){
        let state=model({index:5,strip:5,busy:false,fullscreen:false,view:"preview"});
        let items=["0","1","2","3","4","5","6","7","8","9","10","11"];
        let strip=stack(){for offset in -3..4 {
            let visible=derive(||state.strip+offset>=0 && state.strip+offset<items.len());
            stack(visible:visible){text(value:derive(||if visible.value {items[state.strip+offset]} else {"hidden"}));};
        }};
        let current=derive(||items[state.index]);
        stage.replace(column(){
            stack(visible:derive(||state.view!="grid"),enabled:derive(||!state.busy && !state.fullscreen)){
                strip;text(value:current);text(value:derive(||if state.busy {"busy"} else {current.value}));
            };
            button("first",on_click:||{state.busy=true;state.index=0;state.strip=0;state.busy=false;});
            button("back",on_click:||{state.view="grid";});
            button("last",on_click:||{state.busy=true;state.index=11;state.strip=11;state.view="preview";state.busy=false;});
        });narrator("ready");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        for action in ["first", "back", "last"] {
            let id = vm
                .stage()
                .nodes()
                .find(|n| matches!(&n.kind,NodeKind::Button{label:Some(s),..}if s==action))
                .unwrap()
                .id;
            vm.click(&mut session, id).unwrap();
            models::drive(&mut vm, &mut session, false);
        }
    }
}

#[test]
fn nested_derived_new_branch_refreshes_clean_ancestors_and_bound_fields() {
    let code = r#"class Flags {let mut enabled:bool=false;}
    fn opening(){
        let state=model({open:false,index:1});
        let items=["first"];
        let base=derive(||state.index);
        let allowed=derive(||base.value>0);
        let position=derive(||base.value-1);
        FORWARDING
        let consumer=derive(||if state.open && forwarding.enabled {items[position.value]} else {"hidden"});
        stage.replace(column(){text(value:consumer);
            button("open",on_click:||{
                state.open=true;state.index=0;
                if allowed.value!=true {throw "ordinary segment eagerly recomputed";}
            });
            button("show",on_click:||{state.open=false;state.index=1;state.open=true;});
        });narrator("ready");
    }"#;
    for forwarding in [
        "let forwarding=stack(enabled:allowed);",
        "let forwarding=model({enabled:false});forwarding.enabled=allowed;",
        "let forwarding=model(Flags());forwarding.enabled=allowed;",
    ] {
        let code = code.replace("FORWARDING", forwarding);
        for encoded in [false, true] {
            let (mut session, mut vm) = models::prepared(&code, encoded);
            models::drive(&mut vm, &mut session, false);
            let id = vm
                .stage()
                .nodes()
                .find(|n| matches!(&n.kind,NodeKind::Button{label:Some(s),..}if s=="open"))
                .unwrap()
                .id;
            vm.click(&mut session, id).unwrap();
            models::drive(&mut vm, &mut session, false);
            assert!(
                vm.stage()
                    .nodes()
                    .any(|n| n.kind == NodeKind::Text("hidden".into()))
            );
            let id = vm
                .stage()
                .nodes()
                .find(|n| matches!(&n.kind,NodeKind::Button{label:Some(s),..}if s=="show"))
                .unwrap()
                .id;
            vm.click(&mut session, id).unwrap();
            models::drive(&mut vm, &mut session, false);
            assert!(
                vm.stage()
                    .nodes()
                    .any(|n| n.kind == NodeKind::Text("first".into()))
            );
        }
    }
}

#[test]
fn nested_derived_fault_keeps_the_producer_source_and_releases_capture() {
    let code = r#"fn opening(){
        let state=model({index:1,other:0});
        let source=derive(||if state.index>0 {"first"} else {throw "upstream failed";});
        let consumer=derive(||if state.other>=0 {source.value} else {"hidden"});
        stage.replace(column(){text(value:consumer);
            button("fail",on_click:||{state.other+=1;state.index=0;});
        });narrator("ready");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        let id = vm
            .stage()
            .nodes()
            .find(|n| matches!(&n.kind,NodeKind::Button{label:Some(s),..}if s=="fail"))
            .unwrap()
            .id;
        vm.click(&mut session, id).unwrap();
        for attempt in 0..100_000 {
            assert!(attempt < 99_999, "upstream fault did not finish");
            match vm
                .step(
                    &mut session,
                    SessionBudget::Exact(SliceBudget {
                        instructions: 1,
                        ..Default::default()
                    }),
                    || 0,
                )
                .unwrap()
            {
                SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                    vm.dispatch(&mut session, ticket, "en", NativeBudget::Exact(1))
                        .unwrap();
                }
                SessionStep::Vm(RunOutcome::Returned(_)) => {
                    vm.discard_delivery(&mut session).unwrap()
                }
                SessionStep::Vm(RunOutcome::Fault(id)) => {
                    let fault = vm.vm.delivery_fault(id).unwrap().unwrap();
                    assert_eq!(fault.kind, FaultKind::Thrown);
                    let span = fault.location.source.unwrap();
                    assert_eq!(
                        &code[span.start as usize..span.end as usize],
                        "throw \"upstream failed\";"
                    );
                    assert!(!vm.vm.heap().reactive_evaluating());
                    break;
                }
                _ => {}
            }
        }
    }
}

#[test]
fn nested_derived_cycle_reports_the_author_read() {
    let code = r#"fn opening(){
        let state=model({cycle:false});
        let mut first=null;let mut second=null;
        first=derive(||if state.cycle {second.value} else {1});
        second=derive(||first.value);
        stage.replace(column(){text(value:derive(||f"{second.value}"));
            button("cycle",on_click:||{state.cycle=true;});
        });narrator("ready");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        let id = vm
            .stage()
            .nodes()
            .find(|n| matches!(&n.kind,NodeKind::Button{label:Some(s),..}if s=="cycle"))
            .unwrap()
            .id;
        vm.click(&mut session, id).unwrap();
        for attempt in 0..100_000 {
            assert!(attempt < 99_999, "cycle was not diagnosed");
            match vm
                .step(
                    &mut session,
                    SessionBudget::Exact(SliceBudget {
                        instructions: 1,
                        ..Default::default()
                    }),
                    || 0,
                )
                .unwrap()
            {
                SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                    vm.dispatch(&mut session, ticket, "en", NativeBudget::Exact(1))
                        .unwrap();
                }
                SessionStep::Vm(RunOutcome::Returned(_)) => {
                    vm.discard_delivery(&mut session).unwrap()
                }
                SessionStep::Vm(RunOutcome::Fault(id)) => {
                    let fault = vm.vm.delivery_fault(id).unwrap().unwrap();
                    assert_eq!(fault.kind, FaultKind::InvalidState);
                    let span = fault.location.source.unwrap();
                    assert_eq!(&code[span.start as usize..span.end as usize], "first.value");
                    assert!(!vm.vm.heap().reactive_evaluating());
                    break;
                }
                _ => {}
            }
        }
    }
}

#[test]
fn dialogue_identity_and_read_styling_survive_completion_choice_and_nvl_retention() {
    let code = r##"
    let d = game.dialogue();
    let history = game.history();
    fn opening() {
        stage.replace(list(|| d.lines, key: |line| line.id, build: |row|
            text(value: bind(row.value.text), reveal: bind(row.value.reveal),
                color: derive(|| if row.value.read { "#aaaaaa" }
                    else if row.value.speaker_id == "a" { "#ff0000" } else { "#0000ff" }))));
        say([character("Same", id: "a").line("first"), character("Same", id: "b").line("second")]);
        let a = d.lines[0]; let b = d.lines[1];
        let all_read = a.read && b.read;
        if d.active || d.speaker_id != "a" || d.content_id != a.content_id || d.read != all_read
            || a.speaker_id != "a" || b.speaker_id != "b" || a.speaker_name != b.speaker_name
            || a.content_id == b.content_id { throw "displayed identity/read lost"; }
        if history[0].lines[0].speaker_id != a.speaker_id
            || history[0].lines[1].content_id != b.content_id { throw "history identity"; }
        let mut errors = 0;
        try { a.read = false; } catch e { errors += 1; }
        try { a.speaker_id = "other"; } catch e { errors += 1; }
        if errors != 2 { throw "mutable dialogue metadata"; }
        await run(delay(1));
        choose([{key: "next", label: "Next"}]);
        if d.read != all_read || d.speaker_id != "a" { throw "choice changed old display"; }
        narrator("third");
        if d.speaker_id != null || d.lines[2].speaker_id != null || d.read
            || d.lines[0] != a || d.lines[1] != b || a.read != true
            || a.reveal != a.text.byte_len() { throw "NVL state lost"; }
    }"##;
    // Discover content IDs from the actual compiled call site, then restore read
    // progress into a fresh session for mixed and fully-read groups.
    let (mut probe_session, mut probe_vm) = models::prepared(code, false);
    models::drive(&mut probe_vm, &mut probe_session, false);
    let crate::InteractionView::Dialogue(d) = probe_session.view().interaction.unwrap() else {
        panic!()
    };
    let ids: Vec<_> = d
        .lines()
        .iter()
        .map(|line| line.content_id().to_owned())
        .collect();
    for encoded in [false, true] {
        for read_count in [1, 2] {
            let progress =
                crate::ReadProgress::from_ids(ids[..read_count].iter().cloned()).unwrap();
            let mut session =
                Session::new_with_read_progress(SessionId(41), Epoch(9), progress).unwrap();
            let mut story = source::compile_story(code, "model", 1, "opening").unwrap();
            if encoded {
                let bytes = bc::encode(story.program.package(), bc::Limits::default()).unwrap();
                story.program = bc::VerifiedProgram::decode(&bytes, bc::Limits::default()).unwrap();
            }
            let mut vm = SessionVm::load(&session, story.program, VmLimits::default()).unwrap();
            vm.start(&session, story.initializer).unwrap();
            models::drive(&mut vm, &mut session, false);
            vm.start(&session, story.entry).unwrap();
            models::drive(&mut vm, &mut session, false);
            let crate::InteractionView::Dialogue(d) = session.view().interaction.unwrap() else {
                panic!()
            };
            assert!(d.lines()[0].previously_read());
            assert_eq!(d.lines()[1].previously_read(), read_count == 2);
            let colors: Vec<_> = vm
                .stage()
                .nodes()
                .filter(|n| matches!(n.kind, NodeKind::Text(_)))
                .map(|n| (n.id, n.props.color))
                .collect();
            assert_eq!(colors[0].1, [170, 170, 170, 255]);
            assert_eq!(
                colors[1].1,
                if read_count == 2 {
                    [170, 170, 170, 255]
                } else {
                    [0, 0, 255, 255]
                }
            );
            let id = d.id();
            for _ in 0..2 {
                session
                    .handle_local_input(session.root_owner(), InputAction::Advance(id))
                    .unwrap();
            }
            models::drive(&mut vm, &mut session, false);
            assert!(session.view().interaction.is_none());
            for &(node, color) in &colors {
                assert_eq!(vm.stage().node(node).unwrap().props.color, color);
            }
            activities::advance_clock(&mut vm, &session, 1., 32);
            models::drive(&mut vm, &mut session, false);
            let crate::InteractionView::Choice(choice) = session.view().interaction.unwrap() else {
                panic!()
            };
            let option = choice.options()[0].id().clone();
            session
                .handle_local_input(session.root_owner(), InputAction::Select(option))
                .unwrap();
            models::drive(&mut vm, &mut session, false);
            let id = session.view().interaction.unwrap().id();
            for _ in 0..2 {
                session
                    .handle_local_input(session.root_owner(), InputAction::Advance(id))
                    .unwrap();
            }
            models::drive(&mut vm, &mut session, false);
        }
    }
}

#[test]
fn authored_scroll_out_example_keeps_nodes_and_reveals_the_next_line_after_exit() {
    let code = include_str!("../../../../../examples/author_dialogue_transition/src/main.aside")
        .replace("fn main()", "fn opening()");
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(&code, encoded);
        models::drive(&mut vm, &mut session, false);
        let text_ids: Vec<_> = vm
            .stage()
            .nodes()
            .filter(|n| matches!(n.kind, NodeKind::Text(_)))
            .map(|n| n.id)
            .collect();
        let content = vm
            .stage()
            .nodes()
            .find(|n| n.kind == NodeKind::Column(Default::default()))
            .unwrap()
            .id;
        for step in 0..2 {
            let id = session.view().interaction.unwrap().id();
            for _ in 0..2 {
                session
                    .handle_local_input(session.root_owner(), InputAction::Advance(id))
                    .unwrap();
            }
            models::drive(&mut vm, &mut session, false);
            assert!(session.view().interaction.is_none());
            let chunks = vm.vm.heap().stats().chunk_allocations;
            activities::advance_clock(&mut vm, &session, step as f64 * 0.25 + 0.1, 32);
            vm.sync_stage(&session).unwrap();
            assert_eq!(vm.vm.heap().stats().chunk_allocations, chunks);
            assert!(vm.stage().node(content).unwrap().props.y < 0.);
            activities::advance_clock(&mut vm, &session, (step + 1) as f64 * 0.25, 32);
            models::drive(&mut vm, &mut session, false);
            for &id in &text_ids {
                assert!(vm.stage().node(id).is_some());
            }
            if step == 0 {
                assert_eq!(vm.stage().node(content).unwrap().props.y, 0.);
                assert_eq!(
                    vm.stage().node(text_ids[0]).unwrap().props.color,
                    [255, 184, 212, 255]
                );
                assert_eq!(vm.stage().node(text_ids[1]).unwrap().props.reveal, Some(0));
            }
        }
        assert!(session.view().interaction.is_none());
        assert_eq!(vm.stage().node(content).unwrap().props.opacity, 0.);
    }
}

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
fn derived_table_get_tracks_missing_inserted_replaced_and_cleared_values() {
    let code = r#"
    fn opening() {
        let data = model({values: {}});
        let mut step = 0;
        stage.replace(column() {
            text(value: derive(|| f"{data.values.get("key", default: "missing")}"));
            button("update", on_click: |e| {
                step += 1;
                match step {
                    1 => { data.values.key = 42; },
                    2 => { data.values.key = null; },
                    _ => { data.values.clear(); },
                };
            });
        });
        narrator("ready");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, true);
        let button = vm
            .stage()
            .nodes()
            .find(|n| matches!(n.kind, NodeKind::Button { .. }))
            .unwrap()
            .id;
        for (step, expected) in ["missing", "42", "null", "missing"].iter().enumerate() {
            if step != 0 {
                assert!(vm.click(&mut session, button).unwrap());
                models::drive(&mut vm, &mut session, true);
            }
            assert!(
                vm.stage()
                    .nodes()
                    .any(|n| n.kind == NodeKind::Text((*expected).into())),
                "table.get must refresh after step {step}, encoded={encoded}"
            );
        }
    }
}

#[test]
fn narrative_models_compose_nvl_clear_pages_and_select_original_keys() {
    let code = r##"
    let dialogue = game.dialogue();
    let choices = game.choice();
    fn opening() {
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
        try { let _ = slider(value: temporary); } catch e { rejected += 1; }
        let node = text("cycle", x: 3);
        let cyclic = derive(|| node.x + 1);
        try { node.x = cyclic; } catch e { rejected += 1; }
        try { let _ = view(|| { let n = data.n; data.n = n + 1; let _ = text("cycle"); }); } catch e { rejected += 1; }
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
    let code = "fn opening() { let result = derive(|| { let mut n = 0; while n < 10000 { n += 1; } n }); let _ = text(value: result); }";
    let (mut session, mut vm) = models::prepared(code, false);
    loop {
        if let SessionStep::Vm(RunOutcome::HostCall(ticket)) = vm
            .step(
                &mut session,
                SessionBudget::Exact(SliceBudget {
                    instructions: 1,
                    ..Default::default()
                }),
                || 0,
            )
            .unwrap()
        {
            vm.dispatch(&mut session, ticket, "en", NativeBudget::Exact(1))
                .unwrap();
            if vm.vm.heap().reactive_evaluating() {
                break;
            }
        }
    }
    collect(&mut vm);
    vm.revoke(&mut session).unwrap();
    while vm.cleanup_pending() {
        vm.cleanup_step(
            &mut session,
            32,
            SessionBudget::Exact(SliceBudget::default()),
            || 0,
        )
        .unwrap();
    }
    assert!(!vm.vm.heap().reactive_evaluating());
}

#[test]
fn history_is_stable_readonly_incremental_and_survives_page_and_stage_replacement() {
    let code = r#"
    let d = game.dialogue();
    fn opening() {
        let mio = character("Mio", id: "mio");
        say([mio.line("first"), character("Eri", id: "eri").line("second")]);
        // First history access is deliberately late, after the interaction ended.
        let history = game.history();
        if history != game.history() || history.len() != 1 { throw "history identity"; }
        let first = history[0];
        if first.locale != "en" || first.speaker_name != "Mio" || first.text != "first"
           || first.lines.len() != 2 || first.lines[0].id != d.lines[0].id
           || first.lines[1].id != d.lines[1].id || first.lines[0].speaker_id != "mio"
           || first.lines[0].content_id == "" { throw "history group content"; }
        let mut errors = 0;
        try { history.append(first); } catch e { errors += 1; }
        try { first.text = "lost"; } catch e { errors += 1; }
        try { first.lines.clear(); } catch e { errors += 1; }
        try { first.lines[0].text = "lost"; } catch e { errors += 1; }
        if errors != 4 { throw "history was mutable"; }
        d.clear();
        stage.replace(text(value: derive(|| f"groups {history.len()}")));
        narrator("third");
        if history.len() != 2 || history[0] != first || first.lines[0].text != "first"
           || history[1].lines[0].speaker_id != null || history[1].lines[0].id != d.lines[0].id
           || history[1].group_id == first.group_id { throw "history changed or duplicated"; }
        narrator("end");
        narrator("");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, true);
        for expected in ["first", "third", "end"] {
            let Some(crate::InteractionView::Dialogue(d)) = session.view().interaction else {
                panic!("dialogue")
            };
            assert_eq!(d.text(), expected);
            let id = d.id();
            for _ in 0..2 {
                session
                    .handle_local_input(session.root_owner(), InputAction::Advance(id))
                    .unwrap();
                models::drive(&mut vm, &mut session, true);
            }
            collect(&mut vm);
        }
        assert!(
            vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("groups 4".into()))
        );
        let id = session.view().interaction.unwrap().id();
        session
            .handle_local_input(session.root_owner(), InputAction::Advance(id))
            .unwrap();
        models::drive(&mut vm, &mut session, true);
        // Rust hosts can submit an empty group; its history has empty shortcuts.
        session
            .begin_say(session.root_owner(), &[], "en", true)
            .unwrap();
        vm.sync_narrative(&session).unwrap();
        let before = vm.vm.heap().stats();
        for _ in 0..1000 {
            vm.sync_narrative(&session).unwrap();
        }
        let after = vm.vm.heap().stats();
        assert_eq!(
            (
                before.live_objects,
                before.charged_bytes,
                before.chunk_allocations
            ),
            (
                after.live_objects,
                after.charged_bytes,
                after.chunk_allocations
            )
        );
    }
}

#[test]
fn reading_controls_are_observable_transactional_and_gate_narrative_input() {
    let code = r#"
    let r = game.reading();
    fn dynamic(value) {value}
    fn opening() {
        if r != game.reading() || r.auto || r.paused || r.delay != 2.0 { throw "reading defaults"; }
        game.reading().set_auto(true, delay: 0.5);
        let mut errors = 0;
        try { game.reading().set_auto(false, delay: -1); } catch e { errors += 1; }
        try { game.reading().set_auto(dynamic(2)); } catch e { errors += 1; }
        try { r.auto = false; } catch e { errors += 1; }
        if errors != 3 || !r.auto || r.delay != 0.5 { throw "invalid settings changed state"; }
        stage.replace(column() {
            text(value: derive(|| if r.paused { "paused" } else { "reading" }));
            button("resume", on_click: |e| { game.reading().pause(false); game.reading().set_auto(false); });
        });
        game.reading().pause(true);
        narrator("waiting");
        if r.paused || r.auto || r.delay != 0.5 { throw "resume settings"; }
        let choices = game.choice();
        game.reading().pause(true);
        choose([{key: "ok", label: "OK"}]);
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, true);
        let id = session.view().interaction.unwrap().id();
        assert_eq!(
            session.handle_local_input(session.root_owner(), InputAction::Advance(id)),
            Err(crate::InputRejection::ReadingPaused)
        );
        assert!(
            !session
                .reveal_more(session.root_owner(), id, usize::MAX)
                .unwrap()
        );
        assert!(
            vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("paused".into()))
        );
        let button = vm
            .stage()
            .nodes()
            .find(|n| matches!(n.kind, NodeKind::Button { .. }))
            .unwrap()
            .id;
        vm.click(&mut session, button).unwrap();
        models::drive(&mut vm, &mut session, true);
        assert!(!session.reading().paused());
        for _ in 0..2 {
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(id))
                .unwrap();
            models::drive(&mut vm, &mut session, true);
        }
        let Some(crate::InteractionView::Choice(choice)) = session.view().interaction else {
            panic!("choice")
        };
        let option = choice.options()[0].id().clone();
        assert_eq!(
            session.handle_local_input(session.root_owner(), InputAction::Select(option)),
            Err(crate::InputRejection::ReadingPaused)
        );
    }
}

#[test]
fn retiring_the_input_scope_clears_skipping_even_when_its_cancel_callback_cannot_run() {
    use reel_ui::{
        input::ActionPhase,
        stage::{NodeAction, NodeKind},
    };
    let code = r#"fn opening() {
        stage.replace(input(actions: {skip: {bindings: ["ControlLeft"],
            on_action: |e| game.reading().set_skip(e.phase == "pressed", unread: true)}}));
        narrator("before");
        stage.replace(text("new scene"));
        narrator("after");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, true);
        let node = vm
            .stage()
            .nodes()
            .find(|n| matches!(n.kind, NodeKind::Input { .. }))
            .unwrap();
        let NodeKind::Input { bindings, .. } = &node.kind else {
            unreachable!()
        };
        let action = NodeAction {
            node: node.id,
            action: "skip".into(),
            value: 1.,
            phase: ActionPhase::Pressed,
            bindings: bindings.clone(),
        };
        vm.input_action(&mut session, &action).unwrap();
        models::drive(&mut vm, &mut session, true);
        assert!(session.reading().skipping());
        let id = session.view().interaction.unwrap().id();
        for _ in 0..2 {
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(id))
                .unwrap();
        }
        models::drive(&mut vm, &mut session, true);
        assert!(!session.reading().skipping());
        assert!(
            vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("new scene".into()))
        );
    }
}

#[test]
fn stable_reading_controls_skip_heap_publication_and_publish_changed_fields() {
    let (mut session, mut vm) = models::prepared(
        "fn opening(){ game.reading(); narrator(\"ready\"); }",
        false,
    );
    models::drive(&mut vm, &mut session, false);
    let before = vm.narrative.as_ref().unwrap().reading_publishes;
    for _ in 0..4096 {
        vm.sync_narrative(&session).unwrap();
    }
    assert_eq!(vm.narrative.as_ref().unwrap().reading_publishes, before);
    println!(
        "reading_controls syncs=4096 publications={}",
        vm.narrative.as_ref().unwrap().reading_publishes - before
    );
    session.set_skip_reading(true, true, true);
    vm.sync_narrative(&session).unwrap();
    assert_eq!(vm.narrative.as_ref().unwrap().reading_publishes - before, 3);
}
