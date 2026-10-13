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
fn advance_dialogue(vm: &mut SessionVm, session: &mut Session) {
    let id = session.view().interaction.unwrap().id();
    for _ in 0..2 {
        session
            .handle_local_input(session.root_owner(), InputAction::Advance(id))
            .unwrap();
    }
    assert!(matches!(drive(vm, session), RunOutcome::Waiting(_)));
}

#[test]
fn author_tween_await_clock_projection_and_steady_heap_cost() {
    let code = r#"fn opening() {
        let node = text("moving", x: 20, opacity: 0);
        stage.replace(node);
        let action = tween(node, x: 220, opacity: 1, rotation: 360, duration: 2);
        if await run(action) != "Completed" { throw "result"; }
        if node.x != 220 || node.opacity != 1 || node.rotation != 360 { throw "end"; }
        if await run(action) != "Completed" { throw "reuse"; }
        true
    }"#;
    let (mut session, mut vm) = setup(code, Context::Ordinary);
    assert!(matches!(
        drive(&mut vm, &mut session),
        RunOutcome::Waiting(_)
    ));
    collect(&mut vm);
    let chunks = vm.vm.heap().stats().chunk_allocations;
    let started = std::time::Instant::now();
    for step in 1..=256 {
        assert!(
            !vm.advance_time(&session, step as f64 / 256., 64)
                .unwrap()
                .woke
        );
    }
    assert_eq!(
        vm.vm.heap().stats().chunk_allocations,
        chunks,
        "steady animation must not allocate script storage"
    );
    println!(
        "256 tween updates: {:?}, 0 VM payload chunk allocations",
        started.elapsed()
    );
    vm.sync_stage(&session).unwrap();
    let node = vm.stage().node(vm.stage().root().unwrap()).unwrap();
    assert_eq!(
        (node.props.x, node.props.opacity, node.props.rotation),
        (120., 0.5, 180.)
    );
    assert!(vm.advance_time(&session, 2., 64).unwrap().woke);
    assert!(matches!(
        drive(&mut vm, &mut session),
        RunOutcome::Waiting(_)
    ));
    assert!(vm.advance_time(&session, 4., 64).unwrap().woke);
    assert!(matches!(
        drive(&mut vm, &mut session),
        RunOutcome::Returned(_)
    ));
    assert_eq!(vm.retain_return(&session).unwrap().as_bool(), Some(true));
    assert!(!vm.activities_need_time());
}

#[test]
fn activity_controls_takeover_direct_writes_and_retirement() {
    let code = r#"fn opening() {
        let node = text("moving");
        stage.replace(node);
        let first = run(tween(node, x: 100, y: 100, duration: 1));
        pause(first);
        narrator("paused");
        if node.x != 0 { throw "paused clock"; }
        resume(first);
        narrator("resumed");
        if node.x != 50 { throw "resume clock"; }
        node.opacity = 0.8;
        pause(first);
        resume(first);
        let second = run(tween(node, x: 200, duration: 1));
        if await first != "Stopped" { throw "takeover"; }
        node.x = node.x;
        if await second != "Stopped" { throw "same-value write"; }
        let third = run(tween(node, x: 300, duration: 1));
        finish(third);
        finish(third);
        if await third != "Completed" || node.x != 300 { throw "finish"; }
        let stopped = run(tween(node, x: 400));
        stop(stopped);
        finish(stopped);
        if await stopped != "Stopped" || node.x != 300 { throw "terminal changed"; }
        if await run(tween(node, x: 20, duration: 0)) != "Completed" || node.x != 20 { throw "zero"; }
        let independent = run(tween(node, opacity: 1));
        node.y = 7;
        try { node.opacity = 2; } catch error {}
        finish(independent);
        if await independent != "Completed" { throw "unrelated or failed write"; }
        let mut invalid = 0;
        try { tween(node, x: 1, duration: -1); } catch error { invalid += 1; }
        try { tween(node, x: 1, easing: "unknown"); } catch error { invalid += 1; }
        try { run(tween(node, width: 100)); } catch error { invalid += 1; }
        if invalid != 3 { throw "bad action admitted"; }
        let retired = run(tween(node, x: 200), owner: node);
        stage.replace(text("new"));
        let mut cancelled = false;
        try { await retired; } catch error { cancelled = true; }
        if !cancelled { throw "retirement"; }
        true
    }"#;
    let (mut session, mut vm) = setup(code, Context::Ordinary);
    assert!(matches!(
        drive(&mut vm, &mut session),
        RunOutcome::Waiting(_)
    ));
    assert!(!vm.activities_need_time());
    assert!(!vm.advance_time(&session, 100., 64).unwrap().woke);
    advance_dialogue(&mut vm, &mut session);
    assert!(vm.activities_need_time());
    vm.advance_time(&session, 100.5, 64).unwrap();
    let id = session.view().interaction.unwrap().id();
    for _ in 0..2 {
        session
            .handle_local_input(session.root_owner(), InputAction::Advance(id))
            .unwrap();
    }
    let outcome = drive(&mut vm, &mut session);
    if let RunOutcome::Fault(id) = outcome {
        panic!("{:?}", vm.vm.delivery_fault(id));
    }
    assert!(matches!(outcome, RunOutcome::Returned(_)), "{outcome:?}");
    assert_eq!(vm.retain_return(&session).unwrap().as_bool(), Some(true));
    assert!(!vm.activities_need_time());
}

#[test]
fn many_activities_use_bounded_time_slices_and_release_terminal_graphs() {
    let code = r#"fn opening() {
        let mut nodes = [];
        let mut last = null;
        for i in 0..160 {
            let node = text("particle", x: i);
            nodes.append(node);
            last = run(tween(node, y: 100, duration: 2));
        }
        stage.replace(stack(children: nodes));
        if await last != "Completed" { throw "completion"; }
        for node in nodes { if node.y != 100 { throw "partial frame"; } }
        true
    }"#;
    let (mut session, mut vm) = setup(code, Context::Ordinary);
    assert!(matches!(
        drive(&mut vm, &mut session),
        RunOutcome::Waiting(_)
    ));
    collect(&mut vm);
    let roots = vm.vm.heap().stats().roots;
    let chunks = vm.vm.heap().stats().chunk_allocations;
    let first = vm.advance_time(&session, 1., 1).unwrap();
    assert_eq!(first.work, 1);
    assert!(first.pending);
    // A time pass is sampled once, even when the host clock advances between slices.
    let mut work = first.work;
    loop {
        let step = vm.advance_time(&session, 1.5, 7).unwrap();
        assert!(step.work <= 7);
        work += step.work;
        if !step.pending {
            break;
        }
    }
    assert_eq!(work, 160);
    vm.sync_stage(&session).unwrap();
    for node in vm
        .stage()
        .nodes()
        .filter(|n| matches!(n.kind, reel_ui::stage::NodeKind::Text(_)))
    {
        assert_eq!(node.props.y, 50.);
    }
    assert_eq!(vm.vm.heap().stats().chunk_allocations, chunks);
    assert!(vm.advance_time(&session, 1.4, 1).is_err());
    let mut woke = false;
    loop {
        let step = vm.advance_time(&session, 2., 7).unwrap();
        woke |= step.woke;
        if !step.pending {
            break;
        }
    }
    assert!(woke);
    assert!(!vm.activities_need_time());
    assert!(vm.vm.heap().stats().roots < roots);
    assert!(matches!(
        drive(&mut vm, &mut session),
        RunOutcome::Returned(_)
    ));
    assert_eq!(vm.retain_return(&session).unwrap().as_bool(), Some(true));
}
