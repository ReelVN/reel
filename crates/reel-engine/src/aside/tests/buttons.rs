use super::*;
use reel_ui::stage::{NodeId, NodeKind};

fn pump(vm: &mut SessionVm, session: &mut Session) {
    drive(vm, session, true);
}
fn drive(vm: &mut SessionVm, session: &mut Session, project: bool) {
    for _ in 0..50_000 {
        match vm
            .step(
                session,
                SessionBudget::Exact(SliceBudget {
                    instructions: 7,
                    ..Default::default()
                }),
                || 0,
            )
            .unwrap()
        {
            SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                if let Err(error) = vm.dispatch(session, ticket, "en", 8) {
                    vm.reject_native(session, ticket, error).unwrap();
                }
            }
            SessionStep::Vm(RunOutcome::Returned(_)) => vm.discard_delivery(session).unwrap(),
            SessionStep::Vm(RunOutcome::Fault(id)) => {
                panic!("button fault: {:?}", vm.vm.delivery_fault(id).unwrap())
            }
            SessionStep::Vm(RunOutcome::ResourceBlocked(error)) => {
                panic!("button resource: {error:?}")
            }
            SessionStep::Idle | SessionStep::Vm(RunOutcome::Waiting(_))
                if !vm.has_runnable_tasks() && !vm.activities_pending() =>
            {
                if project {
                    vm.sync_stage(session).unwrap();
                }
                return;
            }
            _ => {}
        }
    }
    panic!("button tasks did not suspend");
}
fn button(vm: &SessionVm, name: &str) -> NodeId {
    vm.stage().nodes().find(|node| matches!(&node.kind, NodeKind::Button { label: Some(label), .. } if label == name)).unwrap().id
}
fn text_x(vm: &SessionVm, name: &str) -> f64 {
    vm.stage()
        .nodes()
        .find(|node| node.kind == NodeKind::Text(name.into()))
        .unwrap()
        .props
        .x
}

#[test]
fn text_input_commits_bindings_rejects_stale_edits_and_retires_callbacks() {
    use reel_ui::{stage::NodeText, widgets::TextInputEvent};
    let (mut session, mut vm) = setup(
        r#"fn opening() {
        let profile = model({name: "old"});
        let output = text("status");
        let editor = text_input(value: bind(profile.name), on_change: |event| {
            if event.target.value != event.value || profile.name != event.value { throw "binding not committed"; }
            output.x += 1;
        }, on_submit: |event| {
            if event.value != "中文" { throw "wrong submit"; }
            output.y += 1;
            await run(delay(20));
            throw "retired submit resumed";
        });
        stage.replace(column() { editor; output;
            button("replace", on_click: |event| { profile.name = "author"; });
            button("disable", on_click: |event| { editor.enabled = false; });
            button("enable", on_click: |event| { editor.enabled = true; });
            button("remove", on_click: |event| { editor.remove(); });
        });
        let mut rejected = 0;
        try { let _ = text_input(value: derive(|| profile.name)); } catch e { rejected += 1; }
        try { editor.value = "two\nlines"; } catch e { rejected += 1; }
        if rejected != 2 { throw "invalid text binding"; }
        narrator("still reading");
    }"#,
        Context::Ordinary,
    );
    pump(&mut vm, &mut session);
    let node = vm
        .stage()
        .nodes()
        .find(|n| matches!(n.kind, NodeKind::TextInput { .. }))
        .unwrap()
        .id;
    let change = |previous: &str, value: &str| NodeText {
        node,
        event: TextInputEvent::Changed {
            previous: previous.into(),
            value: value.into(),
        },
    };
    assert!(vm.text_input(&mut session, &change("old", "中文")).unwrap());
    pump(&mut vm, &mut session);
    assert_eq!(text_x(&vm, "status"), 1.);
    assert!(
        vm.text_input(
            &mut session,
            &NodeText {
                node,
                event: TextInputEvent::Submitted("中文".into())
            }
        )
        .unwrap()
    );
    pump(&mut vm, &mut session);
    assert!(vm.activities_need_time());
    vm.click(&mut session, button(&vm, "replace")).unwrap();
    drive(&mut vm, &mut session, false);
    assert!(
        !vm.text_input(&mut session, &change("中文", "late"))
            .unwrap()
    );
    vm.click(&mut session, button(&vm, "disable")).unwrap();
    drive(&mut vm, &mut session, false);
    assert!(
        !vm.text_input(&mut session, &change("author", "disabled"))
            .unwrap()
    );
    vm.click(&mut session, button(&vm, "enable")).unwrap();
    pump(&mut vm, &mut session);
    assert_eq!(text_x(&vm, "status"), 1.);
    vm.click(&mut session, button(&vm, "remove")).unwrap();
    pump(&mut vm, &mut session);
    assert!(vm.tasks.is_empty());
    assert!(!vm.activities_need_time());
    assert!(
        !vm.text_input(&mut session, &change("author", "retired"))
            .unwrap()
    );
    assert_eq!(session.storage_usage().pending_owner_retirements, 0);
}

#[test]
fn modal_click_validation_reads_script_writes_before_projection() {
    let (mut s, mut vm) = setup(
        r#"fn opening() {
        let count = text("count");
        let panel = column(modal: true, visible: false);
        panel.add(button("close", on_click: |event| { panel.visible = false; }));
        stage.replace(stack() {
            button("open", on_click: |event| { panel.visible = true; });
            button("blocked", on_click: |event| { count.x += 1; });
            count;
            panel;
        });
    }"#,
        Context::Ordinary,
    );
    pump(&mut vm, &mut s);
    let open = button(&vm, "open");
    let blocked = button(&vm, "blocked");
    let close = button(&vm, "close");
    let panel = vm.stage().nodes().find(|node| node.props.modal).unwrap().id;
    assert!(vm.click(&mut s, open).unwrap());
    drive(&mut vm, &mut s, false);
    assert!(
        !vm.stage().node(panel).unwrap().props.visible,
        "projection still describes the old frame"
    );
    let before = vm.vm.heap().stats();
    assert!(!vm.click(&mut s, blocked).unwrap());
    assert_eq!(
        before.chunk_allocations,
        vm.vm.heap().stats().chunk_allocations
    );
    assert!(vm.tasks.is_empty());
    assert!(vm.click(&mut s, close).unwrap());
    drive(&mut vm, &mut s, false);
    assert!(vm.click(&mut s, blocked).unwrap());
    pump(&mut vm, &mut s);
    assert_eq!(text_x(&vm, "count"), 1.);
}

#[test]
fn callbacks_share_captures_reuse_retired_storage_and_outlive_main() {
    let (mut s, mut vm) = setup(
        r#"fn opening() {
        let count = text("count");
        let mut n = 0;
        let click = button("increment", on_click: |event| {
            n += 1;
            count.x = n;
            event.target.label = "increment";
        });
        stage.replace(column() { click; count; });
    }"#,
        Context::Ordinary,
    );
    pump(&mut vm, &mut s);
    assert!(vm.task().is_none());
    assert!(vm.has_live_work());
    let id = button(&vm, "increment");
    vm.click(&mut s, id).unwrap();
    pump(&mut vm, &mut s);
    collect(&mut vm);
    let owners = s.storage_usage().reserved_owner_slots;
    let before = vm.vm.heap().stats();
    let started = std::time::Instant::now();
    for n in 2..=512 {
        assert!(vm.click(&mut s, id).unwrap());
        pump(&mut vm, &mut s);
        assert!(vm.tasks.is_empty());
        if n % 16 == 0 {
            collect(&mut vm);
        }
    }
    assert_eq!(text_x(&vm, "count"), 512.);
    assert_eq!(s.storage_usage().reserved_owner_slots, owners);
    assert_eq!(s.storage_usage().pending_owner_retirements, 0);
    assert!(!vm.has_runnable_tasks());
    let after = vm.vm.heap().stats();
    assert_eq!(
        (
            after.live_objects,
            after.roots,
            after.root_ranges,
            after.payload_bytes
        ),
        (
            before.live_objects,
            before.roots,
            before.root_ranges,
            before.payload_bytes
        )
    );
    eprintln!(
        "511 callbacks after one warmup, including GC: {:?}, owner slots {owners}, heap before={before:?}, after={:?}",
        started.elapsed(),
        vm.vm.heap().stats()
    );
    vm.revoke(&mut s).unwrap();
    while vm.cleanup_pending() {
        vm.cleanup_step(
            &mut s,
            1,
            SessionBudget::Exact(SliceBudget::default()),
            || 0,
        )
        .unwrap();
    }
    assert_eq!(
        s.owner_lifecycle(s.root_owner()),
        Some(OwnerLifecycle::Retired)
    );
}

#[test]
fn independent_callbacks_await_one_producer_beside_main_dialogue() {
    let (mut s, mut vm) = setup(
        r#"fn opening() {
        let count = text("count");
        let motion = run(sequence(tween(count, y: 50, duration: 0.5), tween(count, y: 100, duration: 0.5)));
        let first = button("first", on_click: |event| {
            if await motion != "Completed" { throw "wrong completion"; }
            count.x += 1;
        });
        let second = button("second", on_click: |event| {
            if await motion != "Completed" { throw "wrong shared completion"; }
            count.x += 10;
            try { narrator("must not steal dialogue"); } catch e { count.x += 100; }
            try { choose([{key: "bad", label: "bad"}]); } catch e { count.x += 1000; }
        });
        let cancelled = button("cancelled", on_click: |event| {
            await motion;
            throw "cancelled consumer resumed";
        });
        let remove = button("cancel consumer", on_click: |event| { cancelled.remove(); });
        stage.replace(column() { first; second; cancelled; remove; count; });
        narrator("main dialogue");
    }"#,
        Context::Ordinary,
    );
    pump(&mut vm, &mut s);
    let interaction = s.view().interaction.unwrap().id();
    for name in ["first", "second", "cancelled"] {
        let id = button(&vm, name);
        assert!(vm.click(&mut s, id).unwrap());
    }
    pump(&mut vm, &mut s);
    assert_eq!(vm.tasks.len(), 3);
    let remove = button(&vm, "cancel consumer");
    vm.click(&mut s, remove).unwrap();
    pump(&mut vm, &mut s);
    assert_eq!(vm.tasks.len(), 2);
    assert!(!vm.has_runnable_tasks());
    collect(&mut vm);
    assert!(super::activities::advance_clock(&mut vm, &s, 1., 1).woke);
    pump(&mut vm, &mut s);
    assert_eq!(text_x(&vm, "count"), 1111.);
    assert!(vm.tasks.is_empty());
    assert_eq!(s.view().interaction.unwrap().id(), interaction);
    vm.revoke(&mut s).unwrap();
    while vm.cleanup_pending() {
        vm.cleanup_step(
            &mut s,
            1,
            SessionBudget::Exact(SliceBudget::default()),
            || 0,
        )
        .unwrap();
    }
}

#[test]
fn removing_callback_owner_cancels_task_and_its_motion_before_next_statement() {
    let (mut s, mut vm) = setup(
        r#"fn opening() {
        let count = text("count");
        let holder = column();
        holder.add(button("waiting", on_click: |event| {
            await run(tween(count, y: 100, duration: 1));
            throw "retired waiting callback resumed";
        }));
        holder.add(button("remove", on_click: |event| {
            holder.clear();
            throw "self-removal continued";
        }));
        stage.replace(column() { holder; count;
            button("close pending", on_click: |event| {
                await run(tween(count, x: 100, duration: 1));
                throw "closed callback resumed";
            });
        });
        narrator("keep main alive");
    }"#,
        Context::Ordinary,
    );
    pump(&mut vm, &mut s);
    let waiting = button(&vm, "waiting");
    let remove = button(&vm, "remove");
    vm.click(&mut s, waiting).unwrap();
    pump(&mut vm, &mut s);
    assert_eq!(vm.tasks.len(), 1);
    super::activities::advance_clock(&mut vm, &s, 0.5, 1);
    vm.click(&mut s, remove).unwrap();
    pump(&mut vm, &mut s);
    assert!(vm.tasks.is_empty());
    assert!(!vm.click(&mut s, waiting).unwrap());
    vm.advance_time(&s, 2., 16).unwrap();
    pump(&mut vm, &mut s);
    assert!(!vm.activities_need_time());
    assert!(s.view().interaction.is_some());
    assert_eq!(s.storage_usage().pending_owner_retirements, 0);
    let close = button(&vm, "close pending");
    vm.click(&mut s, close).unwrap();
    pump(&mut vm, &mut s);
    assert_eq!(vm.tasks.len(), 1);
    vm.revoke(&mut s).unwrap();
    while vm.cleanup_pending() {
        vm.cleanup_step(
            &mut s,
            1,
            SessionBudget::Exact(SliceBudget::default()),
            || 0,
        )
        .unwrap();
    }
    assert!(vm.tasks.is_empty());
    assert_eq!(s.storage_usage().pending_owner_retirements, 0);
}

#[test]
fn retiring_a_future_target_wakes_waiters_of_paused_compositions_without_clock_ticks() {
    let (mut s, mut vm) = setup(
        r#"fn opening() {
        let count = text("count");
        let future = text("future");
        let motion = run(sequence(tween(count, y: 10), tween(future, y: 20)));
        pause(motion);
        stage.replace(column() { count; future;
            button("wait", on_click: |event| {
                try { await motion; } catch error { count.x += 1; }
            });
            button("retire", on_click: |event| { future.remove(); });
        });
        narrator("waiting");
    }"#,
        Context::Ordinary,
    );
    pump(&mut vm, &mut s);
    assert!(!vm.activities_need_time());
    let waiter = button(&vm, "wait");
    vm.click(&mut s, waiter).unwrap();
    pump(&mut vm, &mut s);
    assert_eq!(vm.tasks.len(), 1);
    let retire = button(&vm, "retire");
    vm.click(&mut s, retire).unwrap();
    pump(&mut vm, &mut s);
    collect(&mut vm);
    assert_eq!(text_x(&vm, "count"), 1.);
    assert!(vm.tasks.is_empty());
    assert!(!vm.activities.has_active());
}

#[test]
fn value_controls_recheck_current_flags_and_retire_waiting_change_tasks() {
    use reel_ui::{
        stage::{ControlValue, NodeChange},
        widgets::SliderRange,
    };
    let (mut session, mut vm) = setup(
        r#"fn opening() {
        let count = text("count");
        let value = slider(on_change: |event| {
            if event.target.value != event.value { throw "uncommitted value"; }
            await run(tween(count, y: 100, duration: 1));
            throw "retired change callback resumed";
        });
        let silent = toggle();
        stage.replace(column() { value; silent; count;
            button("disable", on_click: |event| { value.enabled = false; });
            button("enable", on_click: |event| { value.enabled = true; });
            button("range", on_click: |event| { value.max = 2; });
            button("remove", on_click: |event| { value.remove(); });
        });
        narrator("main wait");
    }"#,
        Context::Ordinary,
    );
    pump(&mut vm, &mut session);
    let value = vm
        .stage()
        .nodes()
        .find(|n| matches!(n.kind, NodeKind::Slider { .. }))
        .unwrap()
        .id;
    let silent = vm
        .stage()
        .nodes()
        .find(|n| matches!(n.kind, NodeKind::Toggle { .. }))
        .unwrap()
        .id;
    assert!(
        vm.change(
            &mut session,
            &NodeChange {
                node: silent,
                value: ControlValue::Bool(true),
                range: None
            }
        )
        .unwrap()
    );
    assert!(matches!(
        vm.stage().node(silent).unwrap().kind,
        NodeKind::Toggle { value: true, .. }
    ));
    assert!(vm.tasks.is_empty());
    let mut change = NodeChange {
        node: value,
        value: ControlValue::Number(0.5),
        range: Some(SliderRange::new(0., 1., None).unwrap()),
    };
    vm.click(&mut session, button(&vm, "disable")).unwrap();
    drive(&mut vm, &mut session, false);
    assert!(
        !vm.change(&mut session, &change).unwrap(),
        "unprojected disable must win"
    );
    vm.click(&mut session, button(&vm, "enable")).unwrap();
    pump(&mut vm, &mut session);
    vm.click(&mut session, button(&vm, "range")).unwrap();
    drive(&mut vm, &mut session, false);
    assert!(
        !vm.change(&mut session, &change).unwrap(),
        "old input range must not write"
    );
    change.range = Some(SliderRange::new(0., 2., None).unwrap());
    assert!(vm.change(&mut session, &change).unwrap());
    pump(&mut vm, &mut session);
    assert_eq!(vm.tasks.len(), 1);
    vm.click(&mut session, button(&vm, "remove")).unwrap();
    pump(&mut vm, &mut session);
    assert!(vm.tasks.is_empty());
    assert!(!vm.change(&mut session, &change).unwrap());
    vm.advance_time(&session, 2., 16).unwrap();
    pump(&mut vm, &mut session);
    assert!(!vm.activities_need_time());
    assert_eq!(session.storage_usage().pending_owner_retirements, 0);
}

#[test]
fn event_callbacks_adapt_zero_parameters_bound_receivers_defaults_and_rest() {
    let code = r#"class Controller {
        let output;
        fn init(self, output) { self.output = output; }
        fn zero(self) { self.output.x += 10; }
        fn event(self, event) {
            if !event.target.enabled { throw "bound event"; }
            self.output.x += 100;
        }
    }
    fn opening() {
        let output = text("result");
        let controller = Controller(output);
        let removed = text("removed");
        fn zero() { output.x += 1; }
        fn optional(event = null) {
            if event == null || !event.target.enabled { throw "optional event"; }
            output.x += 1000;
        }
        fn rest(...events) {
            if events.len() != 1 || !events[0].target.enabled { throw "rest event"; }
            output.x += 10000;
        }
        let mut strict = false;
        let dynamic = {call: zero};
        try { dynamic.call(null); } catch error { strict = true; }
        if !strict { throw "ordinary calls must reject excess arguments"; }
        stage.replace(column() {
            output; removed;
            button("zero", on_click: zero);
            button("bound zero", on_click: controller.zero);
            button("bound event", on_click: controller.event);
            button("optional", on_click: optional);
            button("rest", on_click: rest);
            button("native zero", on_click: removed.remove);
            button("waiting", on_click: || {
                await run(delay(10));
                output.y = 1;
            });
        });
        narrator("callbacks");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        pump(&mut vm, &mut session);
        for label in [
            "zero",
            "bound zero",
            "bound event",
            "optional",
            "rest",
            "native zero",
        ] {
            let id = button(&vm, label);
            vm.click(&mut session, id).unwrap();
            pump(&mut vm, &mut session);
        }
        assert_eq!(text_x(&vm, "result"), 11111.);
        assert!(
            vm.stage()
                .nodes()
                .all(|n| n.kind != NodeKind::Text("removed".into()))
        );
        let waiting = button(&vm, "waiting");
        vm.click(&mut session, waiting).unwrap();
        pump(&mut vm, &mut session);
        assert_eq!(
            vm.activities_poll_after(),
            Some(std::time::Duration::from_secs(10))
        );
        super::activities::advance_clock(&mut vm, &session, 10., 1);
        pump(&mut vm, &mut session);
        let output = vm
            .stage()
            .nodes()
            .find(|n| n.kind == NodeKind::Text("result".into()))
            .unwrap();
        assert_eq!(output.props.y, 1.);
        assert_eq!(vm.activities_poll_after(), None);
    }
}

#[test]
fn pending_pointer_moves_coalesce_across_gc_with_and_without_event_arguments() {
    use reel_ui::stage::{NodePointer, PointerPhase};
    for handler in [
        "|| { output.x += 1; }",
        "|event| { output.x += 1; output.y = event.x; let initial = event.x; await run(delay(1)); if event.x != initial { throw \"started argument changed\"; } output.width = event.x; }",
    ] {
        for encoded in [false, true] {
            let code = format!(
                r#"fn opening() {{
                let output = text("pointer");
                stage.replace(column() {{ output; input(on_pointer: {handler}); }});
                narrator("pointer callbacks");
            }}"#
            );
            let (mut session, mut vm) = models::prepared(&code, encoded);
            pump(&mut vm, &mut session);
            let input = vm
                .stage()
                .nodes()
                .find(|n| matches!(n.kind, NodeKind::Input { .. }))
                .unwrap()
                .id;
            let moved = |x| NodePointer {
                node: input,
                phase: PointerPhase::Moved,
                position: reel_ui::kurbo::Point::new(x, 0.),
            };
            assert!(vm.input_pointer(&mut session, &moved(10.)).unwrap());
            collect(&mut vm);
            assert!(vm.input_pointer(&mut session, &moved(20.)).unwrap());
            pump(&mut vm, &mut session);
            assert_eq!(text_x(&vm, "pointer"), 1., "pending moves share one task");
            assert!(vm.input_pointer(&mut session, &moved(30.)).unwrap());
            collect(&mut vm);
            pump(&mut vm, &mut session);
            assert_eq!(
                text_x(&vm, "pointer"),
                2.,
                "started arguments stay immutable"
            );
            super::activities::advance_clock(&mut vm, &session, 1., 1);
            pump(&mut vm, &mut session);
            if handler.starts_with("|event|") {
                let output = vm
                    .stage()
                    .nodes()
                    .find(|n| n.kind == NodeKind::Text("pointer".into()))
                    .unwrap();
                assert_eq!(output.props.y, 30.);
                assert_eq!(output.props.width, Some(30.));
            }
        }
    }
}
