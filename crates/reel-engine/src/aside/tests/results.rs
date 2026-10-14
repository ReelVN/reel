use super::{
    models::{drive, prepared},
    *,
};
use reel_ui::stage::{NodeId, NodeKind, NodeText};
use reel_ui::widgets::TextInputEvent;

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
fn form_and_child_game_return_values_without_story_interactions() {
    let code = include_str!("../../../../../examples/author_results.aside")
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
                event: TextInputEvent::Changed {
                    previous: "".into(),
                    value: "小明".into(),
                },
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
        let mut limits = VmLimits {
            max_tasks: 160,
            ..Default::default()
        };
        limits.slots.max_capacity = 262_144;
        limits.slots.initial_capacity = 262_144;
        let mut vm = SessionVm::load(&session, story.program, limits).unwrap();
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
