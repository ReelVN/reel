use super::*;
mod chains;
mod tween_from;

#[test]
fn initially_paused_activity_restores_before_first_activation() {
    use aside_vm::snapshot::SnapshotBudget;
    let source = r#"fn opening() {
        let node=stack(x:0); stage.replace(node);
        let moving=run(tween(node,x:100,duration:10));
        let prepared=run(tween(node,x:60,duration:1),paused:true);
        let timer=clock(interval:0.25,paused:true);
        node.add(text("clock",x:bind(timer.ticks),y:bind(timer.elapsed)));
        narrator("prepared");
        if node.x!=20 || timer.ticks!=0 || timer.elapsed!=0 {throw "initial pause";}
        stop(moving);
        node.x=30;
        resume(prepared);resume(timer);
        narrator("resumed");
        if node.x!=45 || timer.ticks!=2 {throw "activation must read current start";}
        narrator("halfway");
        if await prepared!="Completed" || node.x!=60 || timer.ticks!=4 {throw "completion";}
        stop(timer);
        narrator("done");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(source, encoded);
        models::drive(&mut vm, &mut session, false);
        let allocations = vm.vm.heap().stats().chunk_allocations;
        for i in 1..=120 {
            advance_clock(&mut vm, &session, i as f64 / 60., 64);
        }
        assert_eq!(vm.vm.heap().stats().chunk_allocations, allocations);
        let node = vm.stage().root().unwrap();
        vm.sync_stage(&session).unwrap();
        assert_eq!(vm.stage().node(node).unwrap().props.x, 20.);
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
                session_id: SessionId(42),
                epoch: Epoch(21),
            },
            crate::ReadProgress::default(),
            None,
            None,
        )
        .unwrap();
        while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let (mut session, mut vm) = restore.finish().unwrap();
        advance_dialogue(&mut vm, &mut session);
        advance_clock(&mut vm, &session, 2.5, 64);
        advance_dialogue(&mut vm, &mut session);
        advance_clock(&mut vm, &session, 3., 64);
        advance_dialogue(&mut vm, &mut session);
        assert!(matches!(session.view().interaction,
            Some(crate::InteractionView::Dialogue(d)) if d.text()=="done"));
        assert!(!vm.activities_need_time());
    }
}

#[test]
fn initially_paused_actions_defer_zero_duration_and_preserve_controls() {
    let source = r#"fn dynamic(value) { value } fn opening() {
        let node=stack(x:7);stage.replace(node);
        let zero=run(tween(node,x:99,duration:0),paused:true);
        let empty=run(sequence(),paused:true);
        if node.x!=7 {throw "paused zero wrote";}
        stop(zero);
        if await zero!="Stopped" || node.x!=7 {throw "paused stop";}
        resume(empty);
        if await empty!="Completed" {throw "empty resume";}
        let forced=run(tween(node,x:42,duration:0),paused:true);
        finish(forced);
        if await forced!="Completed" || node.x!=42 {throw "paused finish";}
        scope(body:|s| {
            let grouped=s.run(sequence(tween(node,x:50,duration:0),delay(0)),paused:true);
            if node.x!=42 {throw "scope activated early";}
            resume(grouped);
            if await grouped!="Completed" || node.x!=50 {throw "scope resume";}
        });
        let waiting=run(tween(node,x:60,duration:1),paused:true,owner:node);
        node.remove();
        let mut cancelled=false;
        try {await waiting;} catch error {cancelled=true;}
        if !cancelled {throw "paused owner retirement";}
        let mut rejected=0;
        try {clock(paused:dynamic(1));} catch error {rejected+=1;}
        try {run(delay(1),paused:dynamic(1));} catch error {rejected+=1;}
        if rejected!=2 {throw "paused requires bool";}
        narrator("done");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(source, encoded);
        models::drive(&mut vm, &mut session, false);
        assert!(matches!(session.view().interaction,
            Some(crate::InteractionView::Dialogue(d)) if d.text()=="done"));
        assert!(!vm.activities_need_time());
    }
}

#[test]
fn observable_clock_coalesces_deadlines_and_keeps_activity_ownership() {
    use std::time::Duration;
    let source = r#"fn dynamic(value) {value} fn opening() {
        let owner = stack(); stage.replace(owner);
        let timer = clock(interval: 60, owner: owner);
        let output = text("clock", x: bind(timer.ticks), y: bind(timer.elapsed));
        owner.add(output);
        let mut rejected = 0;
        try { clock(interval: 0); } catch e { rejected += 1; }
        try { dynamic(timer).ticks = 5; } catch e { rejected += 1; }
        try { finish(timer); } catch e { rejected += 1; }
        if rejected != 3 { throw "clock contract"; }
        narrator("running");
        pause(timer);
        narrator("paused");
        resume(timer);
        narrator("resumed");
        stop(timer);
        if await timer != "Stopped" { throw "stop"; }
        let retired = clock(owner: owner);
        owner.remove();
        let mut cancelled = false;
        try { await retired; } catch e { cancelled = true; }
        if !cancelled { throw "owner cancellation"; }
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(source, encoded);
        models::drive(&mut vm, &mut session, false);
        let position = |vm: &SessionVm| {
            let node = vm
                .stage()
                .nodes()
                .find(|n| n.kind == reel_ui::stage::NodeKind::Text("clock".into()))
                .unwrap();
            (node.props.x, node.props.y)
        };
        assert_eq!(position(&vm), (0., 0.));
        assert_eq!(vm.activities_poll_after(), Some(Duration::from_secs(60)));
        assert!(vm.continuous_time_pending());
        let allocations = vm.vm.heap().stats().chunk_allocations;
        advance_clock(&mut vm, &session, 59., 1);
        assert_eq!(vm.vm.heap().stats().chunk_allocations, allocations);
        assert_eq!(vm.activities_poll_after(), Some(Duration::from_secs(1)));
        let work = advance_clock(&mut vm, &session, 60_000_000., 1);
        assert!(
            work.work < 40,
            "a million missed ticks must be coalesced: {work:?}"
        );
        assert_eq!(vm.vm.heap().stats().chunk_allocations, allocations);
        models::drive(&mut vm, &mut session, false);
        assert_eq!(position(&vm), (1_000_000., 60_000_000.));
        advance_dialogue(&mut vm, &mut session);
        assert_eq!(vm.activities_poll_after(), None);
        assert!(
            !vm.continuous_time_pending(),
            "paused clocks have no sampling event"
        );
        advance_clock(&mut vm, &session, 60_000_030., 1);
        advance_dialogue(&mut vm, &mut session);
        assert_eq!(vm.activities_poll_after(), Some(Duration::from_secs(60)));
        assert!(vm.continuous_time_pending());
        advance_clock(&mut vm, &session, 60_000_090., 1);
        models::drive(&mut vm, &mut session, false);
        assert_eq!(position(&vm), (1_000_001., 60_000_060.));
        let id = session.view().interaction.unwrap().id();
        for _ in 0..2 {
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(id))
                .unwrap();
        }
        models::drive(&mut vm, &mut session, false);
        assert!(!vm.activities_need_time());
        assert!(!vm.activities.has_active());
        assert!(!vm.continuous_time_pending());
    }
}

#[test]
fn clock_ticks_index_and_slice_through_pause_restore_and_stop() {
    use aside_vm::snapshot::SnapshotBudget;
    use std::time::Duration;
    let source = r#"fn check(timer, expected: int) {
        let ticks: int = timer.ticks;
        let elapsed: float = timer.elapsed;
        let prefixes = ["", "雪", "雪a", "雪a🙂", "雪a🙂b"];
        if ticks != expected || "雪a🙂b".slice_scalars(0..ticks) != prefixes[ticks] {
            throw "integer clock indexing";
        }
        if elapsed != ticks * 0.25 { throw "clock elapsed"; }
    }
    fn opening() {
        let owner = stack(); stage.replace(owner);
        let timer = clock(interval: 0.25, owner: owner);
        owner.add(text(derive(|| "雪a🙂b".slice_scalars(0..timer.ticks))));
        check(timer, 0);
        narrator("initial");
        check(timer, 2);
        narrator("running");
        check(timer, 3);
        pause(timer);
        narrator("paused");
        check(timer, 3);
        resume(timer);
        narrator("resumed");
        check(timer, 4);
        stop(timer);
        if await timer != "Stopped" { throw "clock stop"; }
        check(timer, 4);
        narrator("stopped");
        check(timer, 4);
        narrator("done");
    }"#;
    let shown = |vm: &SessionVm, value: &str| {
        assert!(vm.stage().nodes().any(
            |node| matches!(&node.kind, reel_ui::stage::NodeKind::Text(text) if text.as_ref() == value)
        ));
    };
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(source, encoded);
        models::drive(&mut vm, &mut session, false);
        shown(&vm, "");
        advance_clock(&mut vm, &session, 0.625, 1);
        advance_dialogue(&mut vm, &mut session);
        shown(&vm, "雪a");
        advance_clock(&mut vm, &session, 0.875, 1);
        advance_dialogue(&mut vm, &mut session);
        shown(&vm, "雪a🙂");
        assert_eq!(vm.activities_poll_after(), None);

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
        let (mut session, mut vm) = restore.finish().unwrap();
        assert_eq!(vm.activities_poll_after(), None);
        advance_clock(&mut vm, &session, 10.875, 1);
        models::drive(&mut vm, &mut session, false);
        shown(&vm, "雪a🙂");
        advance_dialogue(&mut vm, &mut session);
        assert_eq!(vm.activities_poll_after(), Some(Duration::from_millis(125)));
        advance_clock(&mut vm, &session, 11., 1);
        advance_dialogue(&mut vm, &mut session);
        shown(&vm, "雪a🙂b");
        assert!(!vm.activities_need_time());
        advance_clock(&mut vm, &session, 100., 1);
        advance_dialogue(&mut vm, &mut session);
        assert!(matches!(session.view().interaction,
            Some(crate::InteractionView::Dialogue(dialogue)) if dialogue.text() == "done"));
    }
}

#[test]
fn clock_rejects_tick_count_overflow_before_publishing() {
    let (mut session, mut vm) = models::prepared(
        r#"fn opening() {
            let timer = clock(interval: 1e-20);
            stage.replace(text("clock", x: bind(timer.ticks), y: bind(timer.elapsed)));
            narrator("running");
        }"#,
        false,
    );
    models::drive(&mut vm, &mut session, false);
    assert!(matches!(
        vm.advance_time(&session, 1., 256),
        Err(AdapterError::Input("clock tick count overflow"))
    ));
    vm.sync_stage(&session).unwrap();
    let node = vm.stage().node(vm.stage().root().unwrap()).unwrap();
    assert_eq!((node.props.x, node.props.y), (0., 0.));
}

fn drive(vm: &mut SessionVm, session: &mut Session) -> RunOutcome {
    loop {
        match next(vm, session) {
            RunOutcome::HostCall(ticket) => {
                if let Err(error) = vm.dispatch(session, ticket, "en", NativeBudget::Exact(256)) {
                    vm.reject_native(session, ticket, error).unwrap();
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
                        vm.step(session, SessionBudget::Exact(SliceBudget::default()), || 0)
                            .unwrap(),
                        SessionStep::Progress
                    ));
                }
                return other;
            }
        }
    }
}
pub(super) fn advance_dialogue(vm: &mut SessionVm, session: &mut Session) {
    let id = session.view().interaction.unwrap().id();
    for _ in 0..2 {
        session
            .handle_local_input(session.root_owner(), InputAction::Advance(id))
            .unwrap();
    }
    assert!(matches!(drive(vm, session), RunOutcome::Waiting(_)));
}

#[test]
fn delays_share_composition_controls_and_sleep_until_their_deadline() {
    use std::time::Duration;
    let (mut session, mut vm) = setup(
        r#"fn opening() {
        let node = text("timed"); stage.replace(node);
        let mut rejected = false;
        try { delay(-1); } catch e { rejected = true; }
        if !rejected { throw "negative delay"; }
        if await run(delay(0)) != "Completed" { throw "zero delay"; }
        let action = sequence(delay(1), parallel(delay(0.5), tween(node, x: 100, duration: 0.5)));
        let activity = run(action);
        pause(activity); resume(activity);
        if await activity != "Completed" || node.x != 100 { throw "composition"; }
        let ended = run(repeat(delay(1), times: 3));
        finish(ended);
        if await ended != "Completed" { throw "finish"; }
        let stopped = run(delay(4)); stop(stopped);
        if await stopped != "Stopped" { throw "stop"; }
        let owned = run(delay(20), owner: node);
        stage.replace(text("replacement"));
        let mut cancelled = false;
        try { await owned; } catch e { cancelled = true; }
        if !cancelled { throw "owner"; }
        true
    }"#,
        Context::Ordinary,
    );
    assert!(matches!(
        drive(&mut vm, &mut session),
        RunOutcome::Waiting(_)
    ));
    assert_eq!(vm.activities_poll_after(), Some(Duration::from_secs(1)));
    assert!(
        !vm.continuous_time_pending(),
        "the initial delay is a pure deadline"
    );
    let chunks = vm.vm.heap().stats().chunk_allocations;
    assert!(!advance_clock(&mut vm, &session, 0.5, 1).woke);
    assert_eq!(vm.vm.heap().stats().chunk_allocations, chunks);
    assert_eq!(vm.activities_poll_after(), Some(Duration::from_millis(500)));
    advance_clock(&mut vm, &session, 1.25, 1);
    assert_eq!(
        vm.stage().node(vm.stage().root().unwrap()).unwrap().props.x,
        0.,
        "projection stays explicit"
    );
    assert_eq!(vm.activities_poll_after(), Some(Duration::from_millis(16)));
    assert!(
        vm.continuous_time_pending(),
        "the following tween samples continuously"
    );
    advance_clock(&mut vm, &session, 1.5, 1);
    assert!(matches!(
        drive(&mut vm, &mut session),
        RunOutcome::Returned(_)
    ));
    assert_eq!(vm.retain_return(&session).unwrap().as_bool(), Some(true));
    assert_eq!(vm.activities_poll_after(), None);
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
fn quadratic_curves_keep_distinct_midpoints_and_exact_endpoints() {
    let code = r#"fn opening() {
        let root = stack(); stage.replace(root);
        for curve in ["quad_in", "quad_out", "quad_in_out"] {
            let node = text(curve); root.add(node);
            run(tween(node, x: 100, duration: 1, easing: curve));
        }
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        let allocations = vm.vm.heap().stats().chunk_allocations;
        for (time, expected) in [
            (0.25, [6.25, 43.75, 12.5]),
            (0.75, [56.25, 93.75, 87.5]),
            (1., [100.; 3]),
        ] {
            advance_clock(&mut vm, &session, time, 32);
            vm.sync_stage(&session).unwrap();
            for (curve, expected) in ["quad_in", "quad_out", "quad_in_out"]
                .into_iter()
                .zip(expected)
            {
                let node = vm
                    .stage()
                    .nodes()
                    .find(|node| node.kind == reel_ui::stage::NodeKind::Text(curve.into()))
                    .unwrap();
                assert_eq!(node.props.x, expected);
            }
        }
        assert_eq!(vm.vm.heap().stats().chunk_allocations, allocations);
        assert!(!vm.activities_need_time());
    }
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
    let source = r#"fn dynamic(value) { value } fn opening() {
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
        try { repeat(tween(node, x: 0), times: dynamic(1.5)); } catch error { errors += 1; }
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
                let dispatch = vm
                    .dispatch(&mut session, ticket, "en", NativeBudget::Exact(1))
                    .unwrap();
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

#[test]
fn bound_animation_sources_share_claims_and_release_old_connections() {
    let code = r#"fn opening() {
        let source = text("source", x: 10);
        let a = text("a", x: bind(source.x));
        let b = text("b", x: bind(a.x));
        let replacement = text("replacement", x: 30);
        let view = column() { source; a; b; replacement; };
        stage.replace(view);
        let first = run(tween(a, x: 100, duration: 10));
        let second = run(tween(b, x: 200, duration: 10));
        if await first != "Stopped" { throw "shared animation claim"; }
        pause(second);
        source.x = 20;
        if await second != "Stopped" { throw "write through source"; }
        let third = run(tween(b, x: 300, duration: 10));
        a.x = bind(replacement.x);
        if await third != "Stopped" { throw "rebinding stops old chain"; }
        let old = run(tween(source, x: 40, duration: 10));
        let fresh = run(tween(b, x: 50, duration: 10));
        finish(old);
        finish(fresh);
        if await old != "Completed" || await fresh != "Completed" { throw "stale claim"; }
        if source.x != 40 || replacement.x != 50 || a.x != 50 || b.x != 50 { throw "animated propagation"; }
        let mut conflict = false;
        try { run(parallel(tween(a, x: 60), tween(b, x: 70))); } catch e { conflict = true; }
        if !conflict { throw "parallel alias conflict"; }
        let fourth = run(tween(b, x: 80));
        unbind(bind(a.x));
        if await fourth != "Stopped" { throw "unbind stops dependent animation"; }
        let limit = slider(value: 10, max: 100);
        let panel = stack(width: bind(limit.value));
        view.add(limit);
        view.add(panel);
        let ongoing = run(tween(panel, width: 80, duration: 1));
        limit.max = 20;
        let mut failed = false;
        try { await ongoing; } catch e { failed = true; }
        if !failed || limit.value != 10 || panel.width != 10 { throw "failed animation transaction"; }
        true
    }"#;
    let (mut session, mut vm) = setup(code, Context::Ordinary);
    assert!(matches!(
        drive(&mut vm, &mut session),
        RunOutcome::Waiting(_)
    ));
    collect(&mut vm);
    advance_clock(&mut vm, &session, 0.5, 7);
    assert!(matches!(
        drive(&mut vm, &mut session),
        RunOutcome::Returned(_)
    ));
    assert_eq!(vm.retain_return(&session).unwrap().as_bool(), Some(true));
    assert!(!vm.activities_need_time());
}

#[test]
fn skipping_finishes_story_sequences_with_a_small_budget_without_finishing_ui_or_loops() {
    let code = r#"fn opening() {
        let moving = text("story");
        let ui = text("ui");
        let looped = text("loop");
        let paused = text("paused");
        stage.replace(column() { moving; ui; looped; paused; });
        let ui_job = run(tween(ui, x: 100, duration: 100), owner: ui);
        let loop_job = run(repeat(tween(looped, x: 100, duration: 100)));
        let paused_job = run(tween(paused, x: 100, duration: 100));
        pause(paused_job);
        let job = run(sequence(tween(moving, x: 40, duration: 100),
                              repeat(tween(moving, x: 80, duration: 100), times: 1000000)));
        narrator("enable skip");
        if await job != "Completed" || moving.x != 80 { throw "story finish"; }
        if ui.x != 0 || looped.x != 0 || paused.x != 0 { throw "other owner/control advanced"; }
        stop(ui_job); stop(loop_job); stop(paused_job);
        narrator("finished");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        session.set_skip_reading(true, true, false);
        let progress = advance_clock(&mut vm, &session, 0., 1);
        assert!(
            progress.work < 200,
            "finish must not iterate a million repetitions"
        );
        let id = session.view().interaction.unwrap().id();
        for _ in 0..2 {
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(id))
                .unwrap();
        }
        models::drive(&mut vm, &mut session, false);
        let Some(crate::InteractionView::Dialogue(d)) = session.view().interaction else {
            panic!("dialogue")
        };
        assert_eq!(d.text(), "finished");
    }
}

#[test]
fn camera_source_asb_binding_animation_takeover_and_retirement() {
    let code = r#"fn opening() {
        let data = model({focus: 200});
        let view = camera(camera_x: bind(data.focus), camera_y: 100,
                          width: 400, height: 200) { text("world"); };
        let readout = text("focus", x: derive(|| view.camera_x + view.zoom));
        stage.replace(stack { view; readout; });
        if !view.clip || view.zoom != 1 { throw "camera defaults"; }
        let mut rejected = 0;
        try { view.zoom = 0; } catch e { rejected += 1; }
        try { let _ = camera(zoom: -1); } catch e { rejected += 1; }
        try { tween(view, zoom: 0); } catch e { rejected += 1; }
        try { tween(readout, camera_x: 10); } catch e { rejected += 1; }
        if rejected != 4 || view.zoom != 1 { throw "camera validation"; }
        let first = run(tween(view, camera_x: 400, camera_y: 200,
                              zoom: 2, camera_rotation: 90, duration: 2));
        narrator("moving");
        let second = run(tween(view, camera_x: 500, duration: 1));
        if await first != "Stopped" { throw "takeover"; }
        data.focus = 333;
        if await second != "Stopped" { throw "bound assignment takeover"; }
        if view.camera_x != 333 { throw "binding"; }
        let third = run(tween(view, camera_x: 350, duration: 1));
        finish(third);
        if await third != "Completed" || data.focus != 350 { throw "released model claim"; }
        let pending = run(tween(view, zoom: 3, duration: 10), owner: view);
        view.remove();
        let mut cancelled = false;
        try { await pending; } catch e { cancelled = true; }
        if !cancelled { throw "camera owner cancellation"; }
        narrator("done");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        collect(&mut vm);
        let started = std::time::Instant::now();
        for step in 1..=256 {
            let chunks = vm.vm.heap().stats().chunk_allocations;
            advance_clock(&mut vm, &session, step as f64 / 256., 64);
            assert_eq!(
                vm.vm.heap().stats().chunk_allocations,
                chunks,
                "camera tween must not allocate VM payload chunks per sample"
            );
            models::drive(&mut vm, &mut session, false);
        }
        let camera = vm
            .stage()
            .nodes()
            .find_map(|node| match node.kind {
                reel_ui::stage::NodeKind::Camera(view) => Some(view),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            (camera.x, camera.y, camera.zoom, camera.rotation),
            (300., 150., 1.5, 45.)
        );
        let readout = vm
            .stage()
            .nodes()
            .find(|node| node.kind == reel_ui::stage::NodeKind::Text("focus".into()))
            .unwrap();
        assert_eq!(readout.props.x, 301.5);
        eprintln!(
            "camera encoded={encoded}: 256 tween samples with derived projection {:?}; tween sample VM chunk growth=0",
            started.elapsed()
        );
        advance_dialogue(&mut vm, &mut session);
        assert!(
            !vm.stage()
                .nodes()
                .any(|node| matches!(node.kind, reel_ui::stage::NodeKind::Camera(_)))
        );
        assert!(!vm.activities_need_time());
    }
}

#[test]
fn camera_public_example_executes_source_and_asb() {
    let source = include_str!("../../../../../examples/author_camera/src/main.aside")
        .replace("fn main()", "fn opening()");
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(&source, encoded);
        assert!(matches!(
            drive(&mut vm, &mut session),
            RunOutcome::Waiting(_)
        ));
        vm.sync_stage(&session).unwrap();
        assert_eq!(vm.presentation_request(), 1);
        vm.accept_presentation(&session, 1).unwrap();
        assert!(matches!(
            drive(&mut vm, &mut session),
            RunOutcome::Waiting(_)
        ));
        advance_clock(&mut vm, &session, 5.4, 8);
        assert!(matches!(
            drive(&mut vm, &mut session),
            RunOutcome::Returned(_)
        ));
        vm.sync_stage(&session).unwrap();
        let view = vm
            .stage()
            .nodes()
            .find_map(|node| match node.kind {
                reel_ui::stage::NodeKind::Camera(view) => Some(view),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            (view.x, view.y, view.zoom, view.rotation),
            (600., 320., 0.72, 0.)
        );
        assert!(!vm.activities_need_time());
    }
}

#[test]
fn sine_easing_source_asb_endpoints_midpoints_and_sampling_cost() {
    let source = r##"// Supply fonts/body.ttf relative to the asset root.
fn main() {
    game.configure(title: "Sine easing", width: 840, height: 300, fonts: ["fonts/body.ttf"]);
    let actions = [];
    let scene = column(x: 20, y: 30, gap: 24);
    for curve in ["sine_in", "sine_out", "sine_in_out"] {
        let marker = stack(x: 160, width: 36, height: 36, background: "#7bc6ae");
        actions.append(tween(marker, x: 720, duration: 2, easing: curve));
        scene.add(
            stack(width: 800, height: 60) {
                text(curve, font_size: 20);
                marker;
            }
        );
    }
    stage.replace(scene);
    await stage.presented(owner: scene);
    await run(parallel(...actions), owner: scene);
}
"##
    .replace("fn main()", "fn opening()");
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(&source, encoded);
        models::drive(&mut vm, &mut session, false);
        vm.accept_presentation(&session, vm.presentation_request())
            .unwrap();
        models::drive(&mut vm, &mut session, false);
        let allocations = vm.vm.heap().stats().chunk_allocations;
        let root = vm.stage().node(vm.stage().root().unwrap()).unwrap();
        let markers = root
            .children
            .iter()
            .map(|row| vm.stage().node(*row).unwrap().children[1])
            .collect::<Vec<_>>();
        for (time, expected) in [
            (0., [0.; 3]),
            (
                0.5,
                [0.0761204674887133, 0.3826834323650898, 0.1464466094067262],
            ),
            (1., [0.2928932188134524, 0.7071067811865475, 0.5]),
            (
                1.5,
                [0.6173165676349102, 0.9238795325112867, 0.8535533905932737],
            ),
            (2., [1.; 3]),
        ] {
            advance_clock(&mut vm, &session, time, 4);
            vm.sync_stage(&session).unwrap();
            for (&marker, progress) in markers.iter().zip(expected) {
                let actual = vm.stage().node(marker).unwrap().props.x;
                let expected = 160. + 560. * progress;
                if time == 0. || time == 2. {
                    assert_eq!(actual, expected, "sine tween endpoint is exact");
                } else {
                    assert!(
                        (actual - expected).abs() < 1e-9,
                        "time={time} actual={actual} expected={expected}"
                    );
                }
            }
        }
        assert_eq!(
            vm.vm.heap().stats().chunk_allocations,
            allocations,
            "sine sampling must not allocate VM payload chunks"
        );
        assert!(!vm.activities_need_time());
        models::drive(&mut vm, &mut session, false);
    }
}

#[test]
fn quart_elastic_easing_source_asb_overshoot_pause_restore_and_terminal_values() {
    use aside_vm::snapshot::SnapshotBudget;
    let source = r#"fn opening() {
        let owner = stack(); stage.replace(owner);
        let active = [];
        for curve in ["quart_in", "quart_out", "quart_in_out",
                      "elastic_in", "elastic_out", "elastic_in_out"] {
            let node = text(curve); owner.add(node);
            active.append(run(tween(node, x: 100, duration: 1, easing: curve), owner: owner));
        }
        narrator("running");
        for item in active { pause(item); }
        narrator("paused");
        for item in active { resume(item); }
        narrator("resumed");
        for item in active {
            if await item != "Completed" { throw "curve terminal"; }
        }
        narrator("done");
    }"#;
    let positions = |vm: &SessionVm| {
        vm.stage()
            .node(vm.stage().root().unwrap())
            .unwrap()
            .children
            .iter()
            .map(|id| vm.stage().node(*id).unwrap().props.x)
            .collect::<Vec<_>>()
    };
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(source, encoded);
        models::drive(&mut vm, &mut session, false);
        assert_eq!(positions(&vm), vec![0.; 6]);
        let allocations = vm.vm.heap().stats().chunk_allocations;
        advance_clock(&mut vm, &session, 0.25, 1);
        vm.sync_stage(&session).unwrap();
        let sampled = positions(&vm);
        for (actual, expected) in sampled.iter().zip([
            0.390625,
            68.359375,
            3.125,
            -0.5524271728019903,
            91.16116523516816,
            -0.78125,
        ]) {
            assert!((actual - expected).abs() < 1e-9, "{sampled:?}");
        }
        advance_clock(&mut vm, &session, 0.4, 1);
        vm.sync_stage(&session).unwrap();
        assert!(positions(&vm)[4] > 100., "elastic destination overshoot");
        assert_eq!(vm.vm.heap().stats().chunk_allocations, allocations);
        advance_dialogue(&mut vm, &mut session);
        let paused = positions(&vm);
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
        let (mut session, mut vm) = restore.finish().unwrap();
        advance_clock(&mut vm, &session, 10.4, 1);
        models::drive(&mut vm, &mut session, false);
        assert_eq!(positions(&vm), paused);
        advance_dialogue(&mut vm, &mut session);
        advance_clock(&mut vm, &session, 11., 1);
        vm.sync_stage(&session).unwrap();
        assert_eq!(positions(&vm), vec![100.; 6]);
        advance_dialogue(&mut vm, &mut session);
        assert!(matches!(session.view().interaction,
            Some(crate::InteractionView::Dialogue(dialogue)) if dialogue.text() == "done"));
        assert!(!vm.activities_need_time());
    }
}
