use super::*;
use aside_vm::snapshot::SnapshotBudget;
use reel_ui::stage::{InteractionState, NodeInteraction, NodeKind};

#[test]
fn restore_real_story_stage_callbacks_narrative_random_and_concurrent_activities() {
    let code = r##"
    let mut entries = 0;
    fn opening() {
        entries += 1;
        let data = model({score: 1});
        let rng = random(seed: 42);
        let first = rng.int(0, 1000000);
        let done = completion();
        let background = image("scene.png");
        let snow = particles("flake.png", rate: 5, lifetime: 2);
        let emission = emit(snow);
        let mover = run(tween(background, x: 100, duration: 4, easing: "linear"));
        let task = spawn(|| { await run(delay(2)); data.score += 3; done.resolve(9); });
        stage.replace(column() {
            background; snow;
            button("increase", on_click: |e| { data.score += 1; });
            text("score", x: bind(data.score));
            slider(value: bind(data.score), max: 20);
            checkbox("enabled", value: true);
            camera(width: 400, height: 300) { scroll(height: 100) { text("scroll"); } }
        });
        let dialogue = game.dialogue(); let history = game.history(); let reading = game.reading();
        narrator(rich("[color=#ff0088]保存[/color][pause=0.5]之后"));
        let picked = choose([{key: "next", label: "继续"}]);
        if picked != "next" || await done != 9 { throw "saved completion or choice"; }
        await task;
        finish(mover); stop(emission);
        if entries != 1 || data.score != 5 || background.x != 100 { throw "replayed or lost state"; }
        narrator(f"done {rng.int(0, 1000000)}");
    }
    "##;
    let (mut session, mut runtime) = models::prepared(code, true);
    models::drive(&mut runtime, &mut session, false);
    activities::advance_clock(&mut runtime, &session, 0.5, 1);
    models::drive(&mut runtime, &mut session, false);
    let button = runtime.stage().nodes().find(|n| matches!(&n.kind, NodeKind::Button { label: Some(label), .. } if label == "increase")).unwrap().id;
    runtime
        .stage
        .interaction(
            runtime.vm.heap_mut(),
            &NodeInteraction {
                node: button,
                state: InteractionState {
                    hover: true,
                    pressed: true,
                    ..Default::default()
                },
            },
        )
        .unwrap();
    models::drive(&mut runtime, &mut session, false);
    let old_interaction = session.view().interaction.unwrap().id();
    let expected = runtime
        .stage()
        .nodes()
        .map(|n| (n.id, n.props.clone(), n.children.clone()))
        .collect::<Vec<_>>();
    let bytecode = bc::encode(runtime.vm.program().package(), bc::Limits::default()).unwrap();
    let mut capture = runtime.snapshot_begin(&session).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let image = capture.finish().unwrap();
    fn send<T: Send>(_: &T) {}
    send(&image);
    let bytes = image.encode().unwrap();
    let mut image = snapshot::SessionSnapshot::decode(&bytes).unwrap();
    let program = bc::verify(
        bc::decode(&bytecode, bc::Limits::default()).unwrap(),
        bc::Limits::default(),
    )
    .unwrap();
    let mut restore = SessionVm::restore_begin(
        &mut image,
        program,
        VmLimits::default(),
        SessionIdentity {
            session_id: SessionId(41),
            epoch: Epoch(10),
        },
        crate::ReadProgress::default(),
        None,
        None,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let (mut restored_session, mut restored) = restore.finish().unwrap();
    models::drive(&mut restored, &mut restored_session, false);
    assert_eq!(
        restored
            .stage()
            .nodes()
            .map(|n| (n.id, n.props.clone(), n.children.clone()))
            .collect::<Vec<_>>(),
        expected
    );
    assert!(
        restored_session
            .handle_local_input(
                restored_session.root_owner(),
                InputAction::Advance(old_interaction)
            )
            .is_err()
    );
    let host = restored.stage.value(restored.vm.heap(), button).unwrap();
    let field = restored.vm.heap().host_fields(host).unwrap();
    assert!((0..restored.vm.heap().table_len(field).unwrap()).any(|i| {
        let (key, value) = restored.vm.heap().table_entry(field, i).unwrap();
        text::read_string(restored.vm.heap(), key, 32).unwrap() == "hover"
            && value.as_bool() == Some(false)
    }));
    for (vm, session) in [
        (&mut runtime, &mut session),
        (&mut restored, &mut restored_session),
    ] {
        vm.click(session, button).unwrap();
        models::drive(vm, session, false);
        activities::advance_clock(vm, session, 2.5, 1);
        models::drive(vm, session, false);
        let interaction = session.view().interaction.unwrap().id();
        for _ in 0..2 {
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(interaction))
                .unwrap();
        }
        models::drive(vm, session, false);
        let crate::InteractionView::Choice(choice) = session.view().interaction.unwrap() else {
            panic!("choice after restore")
        };
        let choice = choice.options()[0].id().clone();
        session
            .handle_local_input(session.root_owner(), InputAction::Select(choice))
            .unwrap();
        models::drive(vm, session, false);
    }
    let text = |session: &Session| match session.view().interaction.unwrap() {
        crate::InteractionView::Dialogue(d) => d.text().to_owned(),
        _ => unreachable!(),
    };
    assert_eq!(text(&session), text(&restored_session));
    assert_eq!(
        session.view().history.len(),
        restored_session.view().history.len()
    );
    eprintln!(
        "session snapshot {} bytes; {} nodes; continuation and RNG match",
        bytes.len(),
        expected.len()
    );
}
