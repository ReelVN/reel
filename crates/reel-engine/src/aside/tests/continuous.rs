use super::*;

fn marker(vm: &SessionVm, label: &str) -> f64 {
    vm.stage()
        .nodes()
        .find_map(|node| match &node.kind {
            reel_ui::stage::NodeKind::Text(value) if value.as_ref() == label => Some(node.props.x),
            _ => None,
        })
        .unwrap()
}

#[test]
fn clock_next_shares_ticks_without_per_tick_heap_objects() {
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(
            r#"
            fn opening() {
                let root=stack(); stage.replace(root);
                let a=root.add(text("a")); let b=root.add(text("b"));
                let timer=clock(interval:0.01,owner:root);
                let next=timer.next;
                spawn(|| {loop {a.x=next();}},owner:root);
                spawn(|| {loop {b.x=next();}},owner:root);
                narrator("running");
            }
        "#,
            encoded,
        );
        models::drive(&mut vm, &mut session, false);
        activities::advance_clock(&mut vm, &session, 0.02, 64);
        models::drive(&mut vm, &mut session, false);
        let before = vm.vm.heap().stats();
        for i in 3..=100 {
            activities::advance_clock(&mut vm, &session, i as f64 * 0.01 + 0.00001, 64);
            models::drive(&mut vm, &mut session, false);
        }
        assert_eq!(marker(&vm, "a"), 100.);
        assert_eq!(marker(&vm, "b"), 100.);
        let after = vm.vm.heap().stats();
        assert_eq!(after.live_objects, before.live_objects);
        assert_eq!(after.chunk_allocations, before.chunk_allocations);
        activities::advance_clock(&mut vm, &session, 2.5, 1);
        models::drive(&mut vm, &mut session, false);
        assert_eq!(marker(&vm, "a"), 250.);
        assert_eq!(marker(&vm, "b"), 250.);
    }
}

#[test]
fn clock_next_pause_resume_and_stop_settle_waits() {
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(
            r#"
            fn opening() {
                let root=stack();stage.replace(root);
                let position=root.add(text("position"));
                let cancelled=root.add(text("cancelled"));
                let timer=clock(interval:0.1,owner:root);
                spawn(||{try {loop {position.x=timer.next();}} catch error {cancelled.x=1;}},owner:root);
                root.add(button("pause",on_click:||timer.pause()));
                root.add(button("resume",on_click:||timer.resume()));
                root.add(button("stop",on_click:||timer.stop()));
                narrator("running");
            }
        "#,
            encoded,
        );
        models::drive(&mut vm, &mut session, false);
        let click = |label: &str, vm: &mut SessionVm, session: &mut Session| {
            let id = vm
                .stage()
                .nodes()
                .find_map(|node| match &node.kind {
                    reel_ui::stage::NodeKind::Button {
                        label: Some(caption),
                        ..
                    } if caption == label => Some(node.id),
                    _ => None,
                })
                .unwrap();
            vm.click(session, id).unwrap();
            models::drive(vm, session, false);
        };
        activities::advance_clock(&mut vm, &session, 0.2, 64);
        models::drive(&mut vm, &mut session, false);
        assert_eq!(marker(&vm, "position"), 2.);
        click("pause", &mut vm, &mut session);
        activities::advance_clock(&mut vm, &session, 3., 64);
        models::drive(&mut vm, &mut session, false);
        assert_eq!(marker(&vm, "position"), 2.);
        click("resume", &mut vm, &mut session);
        activities::advance_clock(&mut vm, &session, 3.1, 64);
        models::drive(&mut vm, &mut session, false);
        assert_eq!(marker(&vm, "position"), 3.);
        click("stop", &mut vm, &mut session);
        assert_eq!(marker(&vm, "cancelled"), 1.);
        assert!(!vm.activities_need_time());
    }
}

#[test]
fn clock_next_restores_pending_wait_and_owner_retirement_cancels_it() {
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(
            r#"
            fn opening() {
                let root=stack();stage.replace(root);
                let position=root.add(text("position"));
                let cancelled=root.add(text("cancelled"));
                let owner=root.add(stack());
                let timer=clock(interval:0.1,owner:owner);
                let next=timer.next;
                spawn(||{try {loop {position.x=next();}} catch error {cancelled.x=1;}},owner:root);
                root.add(button("retire",on_click:||owner.remove()));
                narrator("running");
            }
        "#,
            encoded,
        );
        models::drive(&mut vm, &mut session, false);
        activities::advance_clock(&mut vm, &session, 0.2, 1);
        models::drive(&mut vm, &mut session, false);
        assert_eq!(marker(&vm, "position"), 2.);
        let program = vm.vm.program().clone();
        let mut capture = vm.snapshot_begin(&session).unwrap();
        while !capture
            .step(aside_vm::snapshot::SnapshotBudget { units: 1 })
            .unwrap()
            .complete
        {}
        let bytes = capture.finish().unwrap().encode().unwrap();
        let mut image = snapshot::SessionSnapshot::decode(&bytes).unwrap();
        let mut restore = SessionVm::restore_begin(
            &mut image,
            program,
            VmLimits::default(),
            crate::SessionIdentity {
                session_id: SessionId(42),
                epoch: Epoch(2),
            },
            crate::ReadProgress::default(),
            None,
            None,
        )
        .unwrap();
        while !restore
            .step(aside_vm::snapshot::SnapshotBudget { units: 1 })
            .unwrap()
            .complete
        {}
        let (mut session, mut vm) = restore.finish().unwrap();
        models::drive(&mut vm, &mut session, false);
        assert_eq!(marker(&vm, "position"), 2.);
        activities::advance_clock(&mut vm, &session, 0.5, 1);
        models::drive(&mut vm, &mut session, false);
        assert_eq!(marker(&vm, "position"), 5.);
        let id = vm
            .stage()
            .nodes()
            .find_map(|node| match &node.kind {
                reel_ui::stage::NodeKind::Button {
                    label: Some(label), ..
                } if label == "retire" => Some(node.id),
                _ => None,
            })
            .unwrap();
        vm.click(&mut session, id).unwrap();
        models::drive(&mut vm, &mut session, false);
        assert_eq!(marker(&vm, "cancelled"), 1.);
        assert!(!vm.activities_need_time());
    }
}
