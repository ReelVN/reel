use super::*;
use reel_ui::stage::{NodeId, NodeKind};

fn pump(vm: &mut SessionVm, session: &mut Session) {
    for _ in 0..50_000 {
        match vm
            .step(
                session,
                SliceBudget {
                    instructions: 7,
                    ..Default::default()
                },
                || 0,
            )
            .unwrap()
        {
            SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                if let Err(error) = vm.dispatch(session, ticket, "en", 8) {
                    vm.reject_native(session, ticket, error.native_fault_kind().unwrap())
                        .unwrap();
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
                if !vm.has_runnable_tasks() =>
            {
                vm.sync_stage(session).unwrap();
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
        assert!(vm.callbacks.is_empty());
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
        "512 callbacks: {:?}, owner slots {owners}, heap before={before:?}, after={:?}",
        started.elapsed(),
        vm.vm.heap().stats()
    );
    vm.revoke(&mut s).unwrap();
    while vm.cleanup_pending() {
        vm.cleanup_step(&mut s, 1, SliceBudget::default(), || 0)
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
        let motion = run(tween(count, y: 100, duration: 1));
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
        stage.replace(column() { first; second; count; });
        narrator("main dialogue");
    }"#,
        Context::Ordinary,
    );
    pump(&mut vm, &mut s);
    let interaction = s.view().interaction.unwrap().id();
    for name in ["first", "second"] {
        let id = button(&vm, name);
        assert!(vm.click(&mut s, id).unwrap());
    }
    pump(&mut vm, &mut s);
    assert_eq!(vm.callbacks.len(), 2);
    assert!(!vm.has_runnable_tasks());
    collect(&mut vm);
    assert!(vm.advance_time(&s, 1., 1).unwrap().woke);
    pump(&mut vm, &mut s);
    assert_eq!(text_x(&vm, "count"), 1111.);
    assert!(vm.callbacks.is_empty());
    assert_eq!(s.view().interaction.unwrap().id(), interaction);
    vm.revoke(&mut s).unwrap();
    while vm.cleanup_pending() {
        vm.cleanup_step(&mut s, 1, SliceBudget::default(), || 0)
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
        stage.replace(column() { holder; count; });
        narrator("keep main alive");
    }"#,
        Context::Ordinary,
    );
    pump(&mut vm, &mut s);
    let waiting = button(&vm, "waiting");
    let remove = button(&vm, "remove");
    vm.click(&mut s, waiting).unwrap();
    pump(&mut vm, &mut s);
    assert_eq!(vm.callbacks.len(), 1);
    vm.advance_time(&s, 0.5, 1).unwrap();
    vm.click(&mut s, remove).unwrap();
    pump(&mut vm, &mut s);
    assert!(vm.callbacks.is_empty());
    assert!(!vm.click(&mut s, waiting).unwrap());
    vm.advance_time(&s, 2., 16).unwrap();
    pump(&mut vm, &mut s);
    assert!(!vm.activities_need_time());
    assert!(s.view().interaction.is_some());
    assert_eq!(s.storage_usage().pending_owner_retirements, 0);
    vm.revoke(&mut s).unwrap();
    while vm.cleanup_pending() {
        vm.cleanup_step(&mut s, 1, SliceBudget::default(), || 0)
            .unwrap();
    }
}
