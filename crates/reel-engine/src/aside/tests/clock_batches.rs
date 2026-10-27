use super::*;
use aside_vm::snapshot::SnapshotBudget;

fn one_step(vm: &mut SessionVm, session: &mut Session) {
    match vm
        .step(
            session,
            SessionBudget::Exact(SliceBudget {
                instructions: 3,
                ..Default::default()
            }),
            || 0,
        )
        .unwrap()
    {
        SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
            vm.dispatch(session, ticket, "en", NativeBudget::Exact(1))
                .unwrap();
        }
        SessionStep::Vm(RunOutcome::Returned(_)) => vm.discard_delivery(session).unwrap(),
        SessionStep::Vm(RunOutcome::Fault(id)) => panic!(
            "source fault: {:?}, value: {:?}",
            vm.vm.delivery_fault(id).unwrap(),
            text::read_string(vm.vm.heap(), vm.vm.delivery_value(id).unwrap(), 4096)
        ),
        SessionStep::Vm(RunOutcome::ResourceBlocked(error)) => panic!("{error:?}"),
        _ => {}
    }
}

fn position(vm: &SessionVm) -> f64 {
    let mut nodes = vm.stage().nodes().filter(|node| {
        matches!(&node.kind, reel_ui::stage::NodeKind::Text(text) if text.as_ref() == "sample")
    });
    let first = nodes.next().unwrap();
    assert_eq!(first.props.y, first.props.x * 2.);
    for node in nodes {
        assert_eq!(node.props.x, first.props.x, "one published time batch");
        assert_eq!(node.props.y, first.props.x * 2.);
    }
    first.props.x
}

#[test]
fn continuous_clock_settles_sliced_derivations_and_restores_a_pending_batch() {
    let source = r#"fn sample(timer) {
        let before = timer.elapsed;
        let mut i = 0;
        while i < 8 { i += 1; }
        if before != timer.elapsed { throw "time changed inside a derived batch"; }
        before
    }
    fn opening() {
        let timer = clock(interval: 0.002);
        let root = stack();
        for i in 0..6 {
            root.add(text("sample",
                x: derive(|| sample(timer)), y: derive(|| sample(timer) * 2)));
        }
        stage.replace(root);
        narrator("running");
        stop(timer);
        narrator("stopped");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(source, encoded);
        models::drive(&mut vm, &mut session, false);
        assert_eq!(position(&vm), 0.);
        assert!(vm.continuous_time_pending());
        let mut restored = false;
        let mut publications = 0;
        let mut last = 0.;
        for tick in 1..=4000 {
            let time = f64::from(tick) / 1000.;
            let chunks = vm.vm.heap().stats().chunk_allocations;
            let progress = vm.advance_time(&session, time, 1).unwrap();
            assert!(progress.work <= 1);
            assert_eq!(vm.vm.heap().stats().chunk_allocations, chunks);
            one_step(&mut vm, &mut session);
            let current = position(&vm);
            if current != last {
                assert!(current > last);
                publications += 1;
                last = current;
            }
            if !restored && vm.derived_task.is_some() && tick > 50 {
                let sampled = vm.activities.time();
                assert!(sampled < time);
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
                (session, vm) = restore.finish().unwrap();
                assert_eq!(vm.activities.time(), sampled);
                assert!(
                    vm.continuous_time_pending(),
                    "restore rebuilds clock sampling state"
                );
                restored = true;
            }
        }
        assert!(restored);
        assert!(
            publications >= 2,
            "continuous time must publish completed batches"
        );
        // Stop advancing the external clock, then drain both its last requested
        // time and any already-started observer without inventing another tick.
        for _ in 0..20_000 {
            vm.advance_time(&session, 4., 1).unwrap();
            one_step(&mut vm, &mut session);
            if vm.stage_ready() && position(&vm) == 4. {
                break;
            }
        }
        assert_eq!(position(&vm), 4.);
        let id = session.view().interaction.unwrap().id();
        for _ in 0..2 {
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(id))
                .unwrap();
        }
        models::drive(&mut vm, &mut session, false);
        assert!(!vm.activities_need_time());
        assert!(!vm.continuous_time_pending());
        activities::advance_clock(&mut vm, &session, 100., 1);
        models::drive(&mut vm, &mut session, false);
        assert_eq!(
            position(&vm),
            4.,
            "stopping preserves the final published value"
        );
    }
}

#[test]
fn continuous_reveal_settles_sliced_derivations_before_the_line_finishes() {
    let source = format!(
        r#"fn sample(body) {{
        let before = body.reveal;
        let mut i = 0;
        while i < 8 {{ i += 1; }}
        if before != game.dialogue().reveal {{ throw "reveal changed inside a derived batch"; }}
        before
    }}
    fn opening() {{
        let body = game.dialogue();
        let root = stack();
        for i in 0..6 {{
            root.add(text("sample",
                x: derive(|| sample(body)), y: derive(|| sample(body) * 2)));
        }}
        stage.replace(root);
        narrator("{}");
        narrator("finished");
    }}"#,
        "a".repeat(4000)
    );
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(&source, encoded);
        models::drive(&mut vm, &mut session, false);
        let id = session.view().interaction.unwrap().id();
        let mut last = position(&vm);
        let mut publications = 0;
        for tick in 1..=2000 {
            session
                .advance_reveal(
                    session.root_owner(),
                    id,
                    std::time::Duration::from_millis(1),
                    1000.,
                )
                .unwrap();
            vm.advance_time(&session, f64::from(tick) / 1000., 1)
                .unwrap();
            one_step(&mut vm, &mut session);
            let current = position(&vm);
            if current != last {
                assert!(current > last);
                publications += 1;
                last = current;
            }
        }
        assert!(
            publications >= 2,
            "reveal must publish while its source is still changing"
        );
        session
            .handle_local_input(session.root_owner(), InputAction::Advance(id))
            .unwrap();
        models::drive(&mut vm, &mut session, false);
        assert_eq!(
            position(&vm),
            4000.,
            "the complete line must become visible"
        );
        session
            .handle_local_input(session.root_owner(), InputAction::Advance(id))
            .unwrap();
        models::drive(&mut vm, &mut session, false);
        assert!(matches!(session.view().interaction,
            Some(crate::InteractionView::Dialogue(dialogue)) if dialogue.text() == "finished"));
    }
}
