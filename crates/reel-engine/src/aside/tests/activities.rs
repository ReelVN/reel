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
            RunOutcome::Fault(id) => {
                let fault = vm.vm.delivery_fault(id).unwrap().unwrap();
                panic!(
                    "activity script: {} at {:?}",
                    vm.format_fault_value(fault).unwrap(),
                    fault.location
                );
            }
            other => {
                while vm.activities_pending() {
                    assert!(matches!(
                        vm.step(session, SliceBudget::default(), || 0).unwrap(),
                        SessionStep::Progress
                    ));
                }
                return other;
            }
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

pub(super) fn advance_clock(
    vm: &mut SessionVm,
    session: &Session,
    seconds: f64,
    budget: u32,
) -> ActivityProgress {
    let mut total = ActivityProgress::default();
    for _ in 0..100_000 {
        let progress = vm.advance_time(session, seconds, budget).unwrap();
        assert!(progress.work <= budget);
        total.work += progress.work;
        total.woke |= progress.woke;
        if !progress.pending {
            return total;
        }
    }
    panic!("activity round did not finish");
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
    assert!(
        (320..=328).contains(&work),
        "one reconciliation and one sample per graph node: {work}"
    );
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

#[test]
fn composition_large_and_small_clock_steps_agree_on_delayed_takeover() {
    let source = r#"fn opening() {
        let node = text("moving");
        stage.replace(node);
        let old = run(tween(node, x: 100, duration: 2));
        let motion = sequence(tween(node, y: 10, duration: 1), tween(node, x: 200, duration: 1));
        let current = run(motion);
        if await current != "Completed" || node.x != 200 || node.y != 10 { throw "composed end"; }
        if await old != "Stopped" { throw "delayed takeover"; }
        node.x = 0;
        node.y = 0;
        finish(run(motion));
        if node.x != 200 || node.y != 10 { throw "description reuse"; }
        true
    }"#;
    for times in [&[1.5][..], &[0.25, 0.5, 1., 1.25, 1.5][..]] {
        let (mut session, mut vm) = setup(source, Context::Ordinary);
        assert!(matches!(
            drive(&mut vm, &mut session),
            RunOutcome::Waiting(_)
        ));
        for &time in times {
            advance_clock(&mut vm, &session, time, 1);
            collect(&mut vm);
        }
        vm.sync_stage(&session).unwrap();
        let props = &vm.stage().node(vm.stage().root().unwrap()).unwrap().props;
        assert_eq!((props.x, props.y), (125., 10.));
        assert!(advance_clock(&mut vm, &session, 2., 1).woke);
        let outcome = drive(&mut vm, &mut session);
        assert!(matches!(outcome, RunOutcome::Returned(_)), "{outcome:?}");
        assert_eq!(vm.retain_return(&session).unwrap().as_bool(), Some(true));
    }
}

#[test]
fn composed_controls_conflicts_empty_actions_and_failure_terminate_whole_roots() {
    let source = r#"fn opening() {
        let node = text("moving", width: 10);
        let future = text("future", width: 10);
        let holder = column(children: [node, future]);
        stage.replace(holder);
        let old = run(tween(node, x: 100, duration: 2));
        let mut errors = 0;
        try { run(parallel(tween(node, x: 7, duration: 0), sequence(tween(node, y: 5), tween(node, x: 9)))); }
        catch error { errors += 1; }
        if node.x != 0 { throw "conflict partially executed"; }
        finish(old);
        if await old != "Completed" || node.x != 100 { throw "conflict stopped old root"; }
        if await run(sequence()) != "Completed" || await run(parallel()) != "Completed" { throw "empty"; }
        if await run(repeat(tween(node, x: 7, duration: 0), times: 0)) != "Completed" || node.x != 100 { throw "zero repeat"; }
        try { repeat(sequence()); } catch error { errors += 1; }
        try { repeat(tween(node, x: 0), times: -1); } catch error { errors += 1; }
        try { repeat(tween(node, x: 0), times: 1.5); } catch error { errors += 1; }
        let cycle = sequence(tween(node, x: 0, duration: 1), tween(node, y: 20, duration: 1));
        let forever = run(repeat(cycle));
        pause(forever);
        try { finish(forever); } catch error { errors += 1; }
        if node.x != 100 || errors != 5 { throw "invalid finish changed state"; }
        stop(forever);
        if await forever != "Stopped" { throw "infinite stop"; }
        let huge = run(repeat(cycle, times: 9000000000000000000));
        finish(huge);
        if await huge != "Completed" || node.x != 0 || node.y != 20 { throw "finite finish"; }
        let root = run(parallel(tween(node, x: 70), tween(node, y: 80)));
        node.x = node.x;
        if await root != "Stopped" || node.y != 20 { throw "direct write stopped only branch"; }
        let cancelled = run(sequence(tween(node, x: 50), tween(future, x: 10)));
        future.remove();
        let mut cancelled_ok = false;
        try { await cancelled; } catch error { cancelled_ok = true; }
        if !cancelled_ok { throw "future target retirement"; }
        let failing = run(sequence(tween(node, y: 30, duration: 1), tween(node, width: 100), tween(node, x: 999)));
        node.width = null;
        let mut failed = false;
        try { await failing; } catch error { failed = true; }
        if !failed || node.x == 999 || node.y != 30 { throw "segment failure"; }
        true
    }"#;
    let (mut session, mut vm) = setup(source, Context::Ordinary);
    let outcome = drive(&mut vm, &mut session);
    assert!(matches!(outcome, RunOutcome::Waiting(_)), "{outcome:?}");
    assert!(advance_clock(&mut vm, &session, 1., 1).woke);
    let outcome = drive(&mut vm, &mut session);
    assert!(matches!(outcome, RunOutcome::Returned(_)), "{outcome:?}");
    assert!(!vm.activities.has_active());
    assert_eq!(vm.retain_return(&session).unwrap().as_bool(), Some(true));
}

#[test]
fn nested_loops_reuse_the_graph_and_pause_the_whole_timeline() {
    let source = r#"fn opening() {
        let node = text("moving");
        stage.replace(node);
        let cycle = sequence(parallel(tween(node, x: 10, duration: 0.5), tween(node, y: 20, duration: 0.25)), tween(node, x: 0, y: 0, duration: 0.5));
        let activity = run(repeat(cycle));
        narrator("looping");
        pause(activity);
        narrator("paused");
        resume(activity);
        narrator("resumed");
        stop(activity);
        if await activity != "Stopped" { throw "stop loop"; }
        true
    }"#;
    let (mut session, mut vm) = setup(source, Context::Ordinary);
    assert!(matches!(
        drive(&mut vm, &mut session),
        RunOutcome::Waiting(_)
    ));
    collect(&mut vm);
    let before = vm.vm.heap().stats();
    for step in 1..=128 {
        advance_clock(&mut vm, &session, step as f64 / 8., 3);
        if step % 8 == 0 {
            collect(&mut vm);
        }
    }
    let after = vm.vm.heap().stats();
    assert_eq!(
        after.chunk_allocations, before.chunk_allocations,
        "loops allocate no VM payload chunks"
    );
    assert_eq!(after.live_objects, before.live_objects);
    advance_clock(&mut vm, &session, 16.25, 1);
    advance_dialogue(&mut vm, &mut session);
    assert!(!vm.activities_need_time());
    advance_clock(&mut vm, &session, 30., 1);
    vm.sync_stage(&session).unwrap();
    assert_eq!(
        vm.stage().node(vm.stage().root().unwrap()).unwrap().props.x,
        5.
    );
    advance_dialogue(&mut vm, &mut session);
    advance_clock(&mut vm, &session, 30.25, 1);
    vm.sync_stage(&session).unwrap();
    assert_eq!(
        vm.stage().node(vm.stage().root().unwrap()).unwrap().props.x,
        10.
    );
    let id = session.view().interaction.unwrap().id();
    for _ in 0..2 {
        session
            .handle_local_input(session.root_owner(), InputAction::Advance(id))
            .unwrap();
    }
    assert!(matches!(
        drive(&mut vm, &mut session),
        RunOutcome::Returned(_)
    ));
    assert_eq!(vm.retain_return(&session).unwrap().as_bool(), Some(true));
}

#[test]
fn deep_graph_admission_and_execution_survive_gc_between_every_work_slice() {
    let source = r#"fn opening() {
        let node = text("deep");
        stage.replace(node);
        let mut action = tween(node, x: 10, duration: 1);
        for i in 0..256 { action = sequence(action); }
        if await run(action) != "Completed" || node.x != 10 { throw "deep graph"; }
        true
    }"#;
    let (mut session, mut vm) = setup(source, Context::Ordinary);
    let mut slices = 0;
    loop {
        match next(&mut vm, &mut session) {
            RunOutcome::HostCall(ticket) => {
                let dispatch = vm.dispatch(&mut session, ticket, "en", 1).unwrap();
                if matches!(dispatch, NativeDispatch::Decoding { .. }) {
                    slices += 1;
                    collect(&mut vm);
                }
            }
            RunOutcome::Waiting(_) => break,
            other => panic!("deep admission: {other:?}"),
        }
    }
    assert!(slices >= 256);
    while vm.activities_pending() {
        vm.activities.drain(&mut vm.vm, 1).unwrap();
        collect(&mut vm);
    }
    let mut woke = false;
    for _ in 0..10_000 {
        let progress = vm.advance_time(&session, 1., 1).unwrap();
        collect(&mut vm);
        woke |= progress.woke;
        if !progress.pending {
            break;
        }
    }
    assert!(woke);
    assert!(matches!(
        drive(&mut vm, &mut session),
        RunOutcome::Returned(_)
    ));
    assert_eq!(vm.retain_return(&session).unwrap().as_bool(), Some(true));
}
