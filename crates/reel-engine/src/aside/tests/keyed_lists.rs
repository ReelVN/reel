use super::*;
use reel_ui::stage::{NodeId, NodeKind};

fn button(vm: &SessionVm, label: &str) -> NodeId {
    vm.stage()
        .nodes()
        .find(|n| matches!(&n.kind, NodeKind::Button { label: Some(s), .. } if &**s == label))
        .unwrap()
        .id
}
fn click(vm: &mut SessionVm, session: &mut Session, label: &str) {
    assert!(vm.click(session, button(vm, label)).unwrap());
    drive(vm, session, true);
}
fn has_text(vm: &SessionVm, value: &str) -> bool {
    vm.stage()
        .nodes()
        .any(|n| matches!(&n.kind, NodeKind::Text(s) if &**s == value))
}

fn drive(vm: &mut SessionVm, session: &mut Session, gc: bool) {
    for _ in 0..200_000 {
        match vm
            .step(
                session,
                SessionBudget::Exact(SliceBudget {
                    instructions: 23,
                    ..Default::default()
                }),
                || 0,
            )
            .unwrap()
        {
            SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                let list = {
                    let p = vm.vm.program().package();
                    p.symbols[p.natives[ticket.native.index()].symbol.index()] == "reel::list"
                };
                if let Err(error) = vm.dispatch(session, ticket, "en", NativeBudget::Exact(8)) {
                    vm.reject_native(session, ticket, error).unwrap();
                }
                if gc && list {
                    collect(vm);
                }
            }
            SessionStep::Vm(RunOutcome::Fault(id)) => {
                panic!("list fault: {:?}", vm.vm.delivery_fault(id).unwrap())
            }
            SessionStep::Vm(RunOutcome::ResourceBlocked(e)) => panic!("list resource: {e:?}"),
            SessionStep::Vm(RunOutcome::Returned(_)) => vm.discard_delivery(session).unwrap(),
            SessionStep::Idle | SessionStep::Vm(RunOutcome::Waiting(_))
                if !vm.has_runnable_tasks() =>
            {
                vm.sync_stage(session).unwrap();
                return;
            }
            _ => {}
        }
    }
    panic!("list did not settle");
}

#[test]
fn keys_preserve_nodes_local_state_and_callbacks_while_values_and_order_change() {
    let source = r#"
    fn opening() {
        let data = model({items: [{id: "a", label: "A"}, {id: "b", label: "B"}], selected: "", built: 0});
        let rows = list(|| data.items, key: |item| item.id, build: |row| {
            data.built += 1;
            let local = model({clicks: 0});
            button(label: derive(|| f"{row.index}:{row.value.label}:{local.clicks}"), on_click: |e| {
                local.clicks += 1; data.selected = row.value.label;
            })
        });
        stage.replace(column() {
            rows;
            text(value: derive(|| f"built {data.built}, selected {data.selected}"));
            button("replace", on_click: |e| { data.items = [{id: "b", label: "Bee"}, {id: "a", label: "Ay"}, {id: "c", label: "C"}]; });
            button("field", on_click: |e| { data.items[0].label = "B2"; });
            button("remove", on_click: |e| { data.items.remove(1); });
            button("return", on_click: |e| { data.items.append({id: "a", label: "Again"}); });
        });
        narrator("ready");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(source, encoded);
        drive(&mut vm, &mut session, true);
        let a = button(&vm, "0:A:0");
        let b = button(&vm, "1:B:0");
        click(&mut vm, &mut session, "0:A:0");
        click(&mut vm, &mut session, "replace");
        assert_eq!(button(&vm, "1:Ay:1"), a);
        assert_eq!(button(&vm, "0:Bee:0"), b);
        assert!(has_text(&vm, "built 3, selected A"));
        click(&mut vm, &mut session, "1:Ay:1");
        assert!(has_text(&vm, "built 3, selected Ay"));
        click(&mut vm, &mut session, "field");
        assert_eq!(button(&vm, "0:B2:0"), b);
        click(&mut vm, &mut session, "remove");
        assert!(vm.stage().node(a).is_none());
        assert!(vm.stage().node(b).is_some());
        assert!(!vm.click(&mut session, a).unwrap());
        click(&mut vm, &mut session, "return");
        assert_ne!(button(&vm, "2:Again:0"), a);
        assert!(has_text(&vm, "built 4, selected Ay"));
        vm.revoke(&mut session).unwrap();
        while vm.cleanup_pending() {
            vm.cleanup_step(
                &mut session,
                32,
                SessionBudget::Exact(SliceBudget::default()),
                || 0,
            )
            .unwrap();
        }
        collect(&mut vm);
        assert!(!vm.vm.heap().reactive_evaluating());
        assert!(vm.stage().nodes().next().is_none());
    }
}

#[test]
fn list_admission_is_readonly_unique_and_keeps_the_old_tree_on_factory_failure() {
    let source = r#"
    fn opening() {
        let original = text("kept"); stage.replace(original);
        let mut errors = 0;
        try { stage.replace(list(|| [1, 1], key: |x| x, build: |row| text("bad"))); } catch e { errors += 1; }
        try { let _ = list(|| [1, 2], key: |x| true, build: |row| null); } catch e { errors += 1; }
        try { let _ = list(|| 1, key: |x| x, build: |row| null); } catch e { errors += 1; }
        try { stage.replace(list(|| [1, 2], key: |x| x, build: |row| {
            if row.index == 1 { throw "factory failed"; } text("detached")
        })); } catch e { errors += 1; }
        try { let _ = list(|| [1], key: |x| x, build: |row| { row.value = 2; null }); } catch e { errors += 1; }
        try { stage.replace(list(|| [1], key: |x| x, build: |row| original)); } catch e { errors += 1; }
        if errors != 6 { throw "list accepted invalid input"; }
        let nothing = list(|| [1, "1", "长鍵", "长鍵前缀", true, false, null, 1.0, {}, []], key: |x| x, build: |row| null);
        narrator("ready");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(source, encoded);
        drive(&mut vm, &mut session, true);
        let root = vm.stage().root().unwrap();
        assert_eq!(
            vm.stage().node(root).unwrap().kind,
            NodeKind::Text("kept".into())
        );
        collect(&mut vm);
        vm.sync_stage(&session).unwrap();
        // Caught failed construction leaves no rooted native continuation.
        assert!(!vm.vm.heap().reactive_evaluating());
    }
}

#[test]
fn nested_lists_follow_replacement_records_without_rebuilding_the_parent() {
    let source = r#"
    fn opening() {
        let data = model({groups: [{id: 1, items: [{id: "x", name: "old"}]}], built: 0});
        stage.replace(column() {
            view(|| list(|| data.groups, key: |g| g.id, build: |group| {
                data.built += 1;
                list(|| group.value.items, key: |item| item.id, build: |row| text(value: derive(|| row.value.name)))
            }));
            text(value: derive(|| f"parents {data.built}"));
            button("replace", on_click: |e| { data.groups = [{id: 1, items: [{id: "x", name: "new"}, {id: "y", name: "added"}]}]; });
        });
        narrator("ready");
    }"#;
    let (mut session, mut vm) = models::prepared(source, true);
    drive(&mut vm, &mut session, true);
    let before = vm
        .stage()
        .nodes()
        .find(|n| n.kind == NodeKind::Text("old".into()))
        .unwrap()
        .id;
    click(&mut vm, &mut session, "replace");
    assert_eq!(
        vm.stage().node(before).unwrap().kind,
        NodeKind::Text("new".into())
    );
    assert!(has_text(&vm, "added"));
    assert!(has_text(&vm, "parents 1"));
}

#[test]
fn large_reorder_builds_only_the_new_row_and_idle_has_no_list_work() {
    let source = r#"
    fn opening() {
        let data = model({items: [], built: 0});
        let mut i = 0; while i < 1000 { data.items.append(i); i += 1; }
        stage.replace(column() {
            list(|| data.items, key: |x| x, build: |row| { data.built += 1; text(value: derive(|| f"{row.key}:{row.index}")) });
            text(value: derive(|| f"built {data.built}"));
            button("reverse", on_click: |e| { data.items.reverse(); data.items.append(1000); });
        });
        narrator("ready");
    }"#;
    let mut session = session();
    let story = source::compile_story(source, "list_cost", 1, "opening").unwrap();
    // This workload deliberately exceeds the embedding host's default object
    // budget. Give it an explicit budget, including all persistent row bindings.
    let mut limits = VmLimits::default();
    limits.heap.max_objects = 131_072;
    limits.heap.max_bytes = 256 << 20;
    let mut vm = SessionVm::load(&session, story.program, limits).unwrap();
    vm.start(&session, story.initializer).unwrap();
    drive(&mut vm, &mut session, false);
    vm.start(&session, story.entry).unwrap();
    drive(&mut vm, &mut session, false);
    let id = vm
        .stage()
        .nodes()
        .find(|n| n.kind == NodeKind::Text("0:0".into()))
        .unwrap()
        .id;
    let control = button(&vm, "reverse");
    let start = std::time::Instant::now();
    vm.click(&mut session, control).unwrap();
    drive(&mut vm, &mut session, false);
    eprintln!(
        "1000-row reorder + append (debug, VM/stage): {:?}",
        start.elapsed()
    );
    assert_eq!(
        vm.stage().node(id).unwrap().kind,
        NodeKind::Text("0:999".into())
    );
    assert!(has_text(&vm, "built 1001"));
    let heap_before = vm.vm.heap().stats();
    let entries_before = vm.vm.stats().native_entries;
    for _ in 0..1000 {
        assert!(matches!(
            vm.step(
                &mut session,
                SessionBudget::Exact(SliceBudget::default()),
                || 0
            )
            .unwrap(),
            SessionStep::Idle | SessionStep::Vm(RunOutcome::Waiting(_))
        ));
    }
    assert_eq!(
        vm.vm.heap().stats().charged_bytes,
        heap_before.charged_bytes
    );
    assert_eq!(
        vm.vm.heap().stats().chunk_allocations,
        heap_before.chunk_allocations
    );
    assert_eq!(vm.vm.stats().native_entries, entries_before);
}

#[test]
fn deleting_a_row_cancels_its_waiting_callback_and_activity() {
    let source = r#"
    fn opening() {
        let data = model({items: [1], completed: false});
        stage.replace(column() {
            list(|| data.items, key: |x| x, build: |row| button("working", on_click: |e| {
                await run(tween(e.target, x: 100, duration: 100));
                data.completed = true;
            }));
            button("remove", on_click: |e| data.items.clear());
            text(value: derive(|| if data.completed { "leaked callback" } else { "not completed" }));
        });
        narrator("ready");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(source, encoded);
        drive(&mut vm, &mut session, true);
        click(&mut vm, &mut session, "working");
        assert!(vm.activities_need_time());
        click(&mut vm, &mut session, "remove");
        activities::advance_clock(&mut vm, &session, 200., 1);
        drive(&mut vm, &mut session, true);
        assert!(!vm.activities_need_time());
        assert!(has_text(&vm, "not completed"));
        assert!(
            vm.tasks
                .iter()
                .all(|c| c.cancelling || c.node.is_none_or(|node| vm.stage().node(node).is_some()))
        );
    }
}

#[test]
fn cancelling_inside_a_row_factory_releases_candidates_and_native_roots() {
    let source = r#"fn opening() {
        stage.replace(view(|| list(|| [1, 2, 3], key: |x| x, build: |row| {
            let child = text("private");
            if row.index == 1 { let mut i = 0; while i < 100000 { i += 1; } }
            child
        })));
    }"#;
    let (mut session, mut vm) = models::prepared(source, false);
    loop {
        if let SessionStep::Vm(RunOutcome::HostCall(ticket)) = vm
            .step(
                &mut session,
                SessionBudget::Exact(SliceBudget {
                    instructions: 1,
                    ..Default::default()
                }),
                || 0,
            )
            .unwrap()
        {
            vm.dispatch(&mut session, ticket, "en", NativeBudget::Exact(1))
                .unwrap();
            collect(&mut vm);
            if vm
                .stage()
                .nodes()
                .filter(|n| matches!(n.kind, NodeKind::Text(_)))
                .count()
                == 2
            {
                break;
            }
        }
    }
    assert!(vm.stage().root().is_none());
    vm.revoke(&mut session).unwrap();
    while vm.cleanup_pending() {
        vm.cleanup_step(
            &mut session,
            8,
            SessionBudget::Exact(SliceBudget {
                instructions: 1,
                ..Default::default()
            }),
            || 0,
        )
        .unwrap();
    }
    collect(&mut vm);
    assert!(!vm.vm.heap().reactive_evaluating());
    assert!(vm.stage().nodes().next().is_none());
}

#[test]
fn failed_reconciliation_preserves_mounted_children_and_reports_the_source() {
    let source = r#"fn opening() {
        let data = model({items: [1, 2]});
        stage.replace(column() {
            list(|| data.items, key: |x| x, build: |row| text(value: f"{row.value}"));
            button("duplicate", on_click: |e| { data.items.append(1); });
        });
        narrator("ready");
    }"#;
    let (mut session, mut vm) = models::prepared(source, true);
    drive(&mut vm, &mut session, true);
    let before: Vec<_> = vm
        .stage()
        .nodes()
        .map(|n| (n.id, n.children.clone()))
        .collect();
    let duplicate = button(&vm, "duplicate");
    vm.click(&mut session, duplicate).unwrap();
    loop {
        match vm
            .step(
                &mut session,
                SessionBudget::Exact(SliceBudget::default()),
                || 0,
            )
            .unwrap()
        {
            SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                if let Err(error) = vm.dispatch(&mut session, ticket, "en", NativeBudget::Exact(1))
                {
                    vm.reject_native(&session, ticket, error).unwrap();
                }
                collect(&mut vm);
            }
            SessionStep::Vm(RunOutcome::Fault(id)) => {
                let fault = vm.vm.delivery_fault(id).unwrap().unwrap();
                assert_eq!(fault.kind, FaultKind::Type);
                assert!(fault.location.source.is_some());
                break;
            }
            _ => {}
        }
    }
    let after: Vec<_> = vm
        .stage()
        .nodes()
        .map(|n| (n.id, n.children.clone()))
        .collect();
    assert_eq!(after, before);
    assert!(!vm.vm.heap().reactive_evaluating());
}

mod object_restore;
mod sort;
