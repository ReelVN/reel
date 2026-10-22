use super::{
    models::{drive, prepared},
    *,
};
use reel_ui::stage::{NodeId, NodeKind, NodeText};
use reel_ui::widgets::TextInputEvent;

mod pending;

fn button(vm: &SessionVm, label: &str) -> NodeId {
    vm.stage()
        .nodes()
        .find(|node| {
            matches!(&node.kind,
        NodeKind::Button { label: Some(text), .. } if text == label)
        })
        .unwrap()
        .id
}
fn click(vm: &mut SessionVm, session: &mut Session, label: &str) {
    vm.click(session, button(vm, label)).unwrap();
    drive(vm, session, true);
}

#[test]
fn presentation_requests_settle_before_late_waits_and_follow_owner_cancellation() {
    let code = r#"fn opening() {
        let root = column { text("waiting"); };
        stage.replace(root);
        let first = stage.presented(owner: root);
        let late = stage.presented(owner: root);
        if (await first) != null { throw "wrong presentation result"; }
        root.remove();
        if late.cancel() || (await late) != null || (await first) != null {
            throw "accepted requests must keep their terminal result";
        }
        let owner = stack();
        stage.replace(owner);
        let revoked = stage.presented(owner: owner);
        owner.remove();
        let mut cancelled = false;
        try { await revoked; } catch error { cancelled = true; }
        if !cancelled { throw "owner retirement did not cancel"; }
        let explicit = stage.presented();
        if !explicit.cancel() || explicit.cancel() { throw "cancel must settle once"; }
        cancelled = false;
        try { await explicit; } catch error { cancelled = true; }
        if !cancelled { throw "explicit cancel did not reach await"; }
        stage.replace(text("done"));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, true);
        collect(&mut vm);
        assert_eq!(vm.presentation_request(), 2);
        vm.accept_presentation(&session, 0).unwrap();
        drive(&mut vm, &mut session, true);
        assert!(
            vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("waiting".into()))
        );
        vm.accept_presentation(&session, 2).unwrap();
        drive(&mut vm, &mut session, true);
        collect(&mut vm);
        assert!(
            vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("done".into()))
        );
        vm.accept_presentation(&session, 1).unwrap();
        vm.revoke(&mut session).unwrap();
        while vm.cleanup_pending() {
            vm.cleanup_step(
                &mut session,
                1,
                SessionBudget::Exact(SliceBudget::default()),
                || 0,
            )
            .unwrap();
        }
        collect(&mut vm);
        assert_eq!(vm.vm.heap().stats().roots, 0);
    }
}

#[test]
fn removing_a_hovered_control_freezes_its_pending_skin_derivations() {
    use reel_ui::stage::{InteractionState, NodeEvent, NodeInteraction};
    let code = r#"fn opening() {
        let data = model({caption: "hovered"});
        let control = button("dismiss", on_click: |event| event.target.remove());
        let skin = derive(|| if control.hover { data.caption } else { "idle" });
        control.add(text(skin));
        let extra = column();
        stage.replace(column() {
            control; extra;
            text(skin);
            button("change", on_click: |event| { data.caption = "changed"; });
            button("reuse", on_click: |event| extra.add(text(skin)));
        });
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, true);
        let control = button(&vm, "dismiss");
        vm.interactions(
            &mut session,
            &[NodeEvent::Interaction(NodeInteraction {
                node: control,
                state: InteractionState {
                    hover: true,
                    ..Default::default()
                },
            })],
        )
        .unwrap();
        drive(&mut vm, &mut session, true);
        click(&mut vm, &mut session, "dismiss");
        collect(&mut vm);
        click(&mut vm, &mut session, "change");
        click(&mut vm, &mut session, "reuse");
        assert!(vm.stage().node(control).is_none());
        assert!(
            vm.stage()
                .nodes()
                .any(|node| node.kind == NodeKind::Text("hovered".into()))
        );
        assert!(
            !vm.stage()
                .nodes()
                .any(|node| node.kind == NodeKind::Text("changed".into()))
        );
    }
}

#[test]
fn retired_consumers_release_conditional_skins_and_retained_derivations_can_rebind() {
    let code = r#"fn opening() {
        let data = model({active: true, count: 1});
        let control = button("dismiss", on_click: |event| event.target.remove());
        control.add(text(derive(|| if data.active { "selected" }
            else if control.hover { "hover" } else { "idle" })));
        let value = derive(|| data.count * 2);
        let caption = derive(|| f"count {value.value}");
        let first = text(caption);
        let second = text(caption);
        let extra = column();
        stage.replace(column() {
            control; first; second; extra;
            button("remove first", on_click: |event| first.remove());
            button("remove second", on_click: |event| second.remove());
            button("change", on_click: |event| { data.active = false; data.count += 1; });
            button("reuse", on_click: |event| extra.add(text(caption)));
        });
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, true);
        click(&mut vm, &mut session, "dismiss");
        click(&mut vm, &mut session, "remove first");
        collect(&mut vm);
        click(&mut vm, &mut session, "change");
        assert_eq!(
            vm.stage()
                .nodes()
                .filter(|node| node.kind == NodeKind::Text("count 4".into()))
                .count(),
            1,
            "a shared consumer keeps the computed chain live"
        );
        click(&mut vm, &mut session, "remove second");
        click(&mut vm, &mut session, "change");
        collect(&mut vm);
        click(&mut vm, &mut session, "reuse");
        assert!(
            vm.stage()
                .nodes()
                .any(|node| node.kind == NodeKind::Text("count 6".into())),
            "rebinding refreshes the entire suspended chain"
        );
        click(&mut vm, &mut session, "change");
        assert!(
            vm.stage()
                .nodes()
                .any(|node| node.kind == NodeKind::Text("count 8".into()))
        );
    }
}

#[test]
fn form_and_child_game_return_values_without_story_interactions() {
    let code = include_str!("../../../../../examples/author_results/src/main.aside")
        .replace("fn main()", "fn opening()");
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(&code, encoded);
        drive(&mut vm, &mut session, true);
        let editor = vm
            .stage()
            .nodes()
            .find(|node| matches!(node.kind, NodeKind::TextInput { .. }))
            .unwrap()
            .id;
        vm.text_input(
            &mut session,
            &NodeText {
                node: editor,
                event: TextInputEvent::replace_all("", "小明", 0),
            },
        )
        .unwrap();
        drive(&mut vm, &mut session, true);
        collect(&mut vm);
        click(&mut vm, &mut session, "确认");
        assert!(vm.pending.is_none());
        click(&mut vm, &mut session, "加一分");
        click(&mut vm, &mut session, "加一分");
        collect(&mut vm);
        click(&mut vm, &mut session, "完成");
        assert!(
            vm.stage()
                .nodes()
                .any(|node| node.kind == NodeKind::Text("小明，你的分数是 2。".into()))
        );
    }
}

#[test]
fn shared_results_survive_waiter_cancellation_and_bounded_delivery_rounds() {
    let code = r#"fn opening() {
        let root = column();
        let result = completion(owner: root);
        let data = model({count: 0, seen: {}, tail: false});
        let waiters = column();
        let cancelled = button("cancelled", on_click: |event| {
            await result;
            throw "cancelled waiter resumed";
        });
        root.add(cancelled);
        root.add(button("remove waiter", on_click: |event| cancelled.remove()));
        root.add(waiters);
        let tail_owner = stack();
        root.add(tail_owner);
        let tail = completion(owner: tail_owner);
        root.add(button("tail", on_click: |event| {
            try { await tail; } catch error { data.tail = true; }
        }));
        root.add(button("remove tail", on_click: |event| tail_owner.remove()));
        for i in 0..130 {
            waiters.add(button(f"wait {i}", on_click: |event| {
                let answer = await result;
                if answer.score != 42 || data.seen.has_own(i) { throw "wrong or duplicate result"; }
                data.seen[i] = true;
                data.count += 1;
            }));
        }
        root.add(button("resolve", on_click: |event| {
            if !result.resolve({score: 42}) || result.resolve(9) || result.cancel() || result.reject("late") {
                throw "settlement replaced";
            }
        }));
        root.add(button("verify", on_click: |event| {
            if !data.tail || data.count != 130 || (await result).score != 42 { throw "lost subscription"; }
            root.remove();
        }));
        stage.replace(root);
    }"#;
    for encoded in [false, true] {
        let mut session = session();
        let mut story = source::compile_story(code, "results", 1, "opening").unwrap();
        if encoded {
            let bytes = bc::encode(story.program.package(), bc::Limits::default()).unwrap();
            story.program = bc::verify(
                bc::decode(&bytes, bc::Limits::default()).unwrap(),
                bc::Limits::default(),
            )
            .unwrap();
        }
        let mut vm = SessionVm::load(&session, story.program, VmLimits::default()).unwrap();
        vm.start(&session, story.initializer).unwrap();
        drive(&mut vm, &mut session, true);
        vm.start(&session, story.entry).unwrap();
        drive(&mut vm, &mut session, true);
        click(&mut vm, &mut session, "cancelled");
        click(&mut vm, &mut session, "remove waiter");
        collect(&mut vm);
        for i in 0..130 {
            click(&mut vm, &mut session, &format!("wait {i}"));
        }
        collect(&mut vm);
        click(&mut vm, &mut session, "tail");
        click(&mut vm, &mut session, "remove tail");
        click(&mut vm, &mut session, "resolve");
        click(&mut vm, &mut session, "verify");
    }
}

#[test]
fn null_failure_owner_retirement_and_late_waits_share_terminal_state() {
    let code = r#"fn opening() {
        let resolved = completion();
        if !resolved.resolve() || await resolved != null { throw "null result"; }
        let rejected = completion();
        rejected.reject({message: "failed"});
        for i in 0..2 {
            let mut caught = false;
            try { await rejected; } catch error {
                if error.message != "failed" { throw "wrong rejection"; }
                caught = true;
            }
            if !caught { throw "missing rejection"; }
        }
        let cancelled = completion();
        if !cancelled.cancel() || cancelled.resolve(1) { throw "wrong cancellation"; }
        let root = column();
        let result = completion(owner: root);
        let late = completion(owner: root);
        root.add(button("remove", on_click: |event| root.remove()));
        stage.replace(root);
        let mut caught = 0;
        try { await result; } catch error { caught += 1; }
        try { await late; } catch error { caught += 1; }
        try { await cancelled; } catch error { caught += 1; }
        if caught != 3 || result.resolve(1) || late.cancel() { throw "owner cancellation"; }
        stage.replace(text("finished"));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, true);
        collect(&mut vm);
        click(&mut vm, &mut session, "remove");
        assert!(
            vm.stage()
                .nodes()
                .any(|node| node.kind == NodeKind::Text("finished".into()))
        );
    }
}

#[test]
fn embedded_puzzle_source_pauses_restores_and_returns_to_dialogue() {
    use aside_vm::snapshot::SnapshotBudget;
    let code = concat!(
        include_str!("../../../../../examples/author_embedded_puzzle/src/main.aside"),
        "\nfn opening() { main(); }"
    );
    for leave in [false, true] {
        let (mut session, mut vm) = prepared(code, true);
        drive(&mut vm, &mut session, false);
        let intro = session.view().interaction.unwrap().id();
        for _ in 0..2 {
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(intro))
                .unwrap();
        }
        drive(&mut vm, &mut session, false);
        click(&mut vm, &mut session, "A");
        super::activities::advance_clock(&mut vm, &session, 0.3, 1);
        drive(&mut vm, &mut session, false);
        click(&mut vm, &mut session, "Pause");
        let hint = |vm: &SessionVm| {
            vm.stage().nodes().find(|n| matches!(&n.kind, NodeKind::Text(t) if t.as_ref().starts_with("Match the pattern:"))).unwrap().props.opacity
        };
        let frozen = hint(&vm);
        assert_eq!(
            vm.activities_poll_after(),
            None,
            "paused puzzle does not request frames"
        );
        super::activities::advance_clock(&mut vm, &session, 30., 1);
        drive(&mut vm, &mut session, false);
        assert_eq!(hint(&vm), frozen);
        let program = vm.vm.program().clone();
        let mut capture = vm.snapshot_begin(&session).unwrap();
        while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let bytes = capture.finish().unwrap().encode().unwrap();
        let mut image = snapshot::SessionSnapshot::decode(&bytes).unwrap();
        let mut restore = SessionVm::restore_begin(
            &mut image,
            program,
            VmLimits::default(),
            SessionIdentity {
                session_id: SessionId(41),
                epoch: Epoch(19),
            },
            crate::ReadProgress::default(),
            None,
            None,
        )
        .unwrap();
        while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let (mut session, mut vm) = restore.finish().unwrap();
        drive(&mut vm, &mut session, false);
        assert_eq!(hint(&vm), frozen);
        assert_eq!(vm.activities_poll_after(), None);
        if leave {
            click(&mut vm, &mut session, "Leave puzzle");
        } else {
            click(&mut vm, &mut session, "Resume");
            assert!(vm.activities_poll_after().is_some());
            click(&mut vm, &mut session, "B");
        }
        let crate::InteractionView::Dialogue(dialogue) = session.view().interaction.unwrap() else {
            panic!("story must resume");
        };
        assert_eq!(
            dialogue.text(),
            if leave {
                "We leave the puzzle for another day."
            } else {
                "The door opens after 2 moves."
            }
        );
        assert!(
            vm.stage()
                .nodes()
                .all(|n| !matches!(n.kind, NodeKind::Button { .. })),
            "component tree retired"
        );
        assert_eq!(
            vm.activities_poll_after(),
            None,
            "clock and animation retired with the component"
        );
    }
}
