use super::*;
use reel_ui::{
    kurbo::{Size, Vec2},
    stage::{NodeEvent, NodeId, NodeKind, NodeScroll, NodeVirtualList, VirtualRowMeasurement},
};
mod nested;
mod scroll_activity;

fn drive(vm: &mut SessionVm, session: &mut Session) {
    for _ in 0..2_000_000 {
        match vm
            .step(
                session,
                SessionBudget::Exact(SliceBudget {
                    instructions: 1000,
                    ..Default::default()
                }),
                || 0,
            )
            .unwrap()
        {
            SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                if let Err(error) = vm.dispatch(session, ticket, "en", NativeBudget::Exact(32)) {
                    vm.reject_native(session, ticket, error).unwrap();
                }
            }
            SessionStep::Vm(RunOutcome::Returned(_)) => vm.discard_delivery(session).unwrap(),
            SessionStep::Vm(RunOutcome::Fault(id)) => panic!(
                "virtual list fault: {:?}",
                vm.vm.delivery_fault(id).unwrap()
            ),
            SessionStep::Vm(RunOutcome::ResourceBlocked(e)) => {
                panic!("virtual list blocked: {e:?}")
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
    panic!("virtual list did not settle");
}
fn prepared(n: usize) -> (Session, SessionVm) {
    prepared_options(n, false)
}
fn prepared_options(n: usize, follow_end: bool) -> (Session, SessionVm) {
    let code = format!(
        r#"
        let mut builds=0; let mut keys=0; let mut sources=0; let mut next={n};
        let data=model({{items:[]}});
        fn opening() {{
            let mut i=0; while i<{n} {{ data.items.append(i); i+=1; }}
            stage.replace(column() {{
                scroll(width:320,height:120,follow_end:{follow_end}) {{
                    list(|| {{ sources+=1; data.items }},key:|x|{{keys+=1;x}},virtual:true,build:|row|{{
                        builds+=1;
                        column(height:20+(row.value%3)*20) {{ button(f"row {{row.value}}"); }}
                    }});
                }};
                button("check",on_click:|e|{{if keys!={n} || sources!=1 {{throw "scroll rebuilt source index";}}}});
                button("reverse",on_click:|e|data.items.reverse());
                button("remove",on_click:|e|data.items.remove(0));
                button("append",on_click:|e|{{data.items.append(next);next+=1;}});
            }});
            narrator("ready");
        }}
    "#
    );
    let (mut session, mut vm) = models::prepared(&code, true);
    drive(&mut vm, &mut session);
    (session, vm)
}
fn ids(vm: &SessionVm) -> (NodeId, NodeId) {
    let list = vm
        .stage()
        .nodes()
        .find(|n| n.parent.is_some() && matches!(n.kind, NodeKind::VirtualList(_)))
        .unwrap();
    (list.id, list.parent.unwrap())
}
fn event(vm: &SessionVm, y: f64, width: f64, retained: &[NodeId]) -> NodeVirtualList {
    let (id, scroll) = ids(vm);
    let NodeKind::VirtualList(view) = &vm.stage().node(id).unwrap().kind else {
        unreachable!()
    };
    let offset = match vm.stage().node(scroll).unwrap().kind {
        NodeKind::Scroll { offset, .. } => offset,
        _ => unreachable!(),
    };
    NodeVirtualList {
        node: id,
        scroll,
        revision: view.revision,
        requested: offset,
        offset: Vec2::new(0., y),
        viewport: Size::new(width, 120.),
        scroll_scale: 1.,
        scroll_limit: (view.total_height - 120.).max(0.),
        width,
        visible_start: y,
        visible_end: y + 120.,
        measurements: view
            .rows
            .iter()
            .map(|r| VirtualRowMeasurement {
                node: r.node,
                height: vm.stage().node(r.node).unwrap().props.height.unwrap(),
            })
            .collect(),
        retained: retained.into(),
        navigation: None,
    }
}
fn feedback(vm: &mut SessionVm, session: &mut Session, y: f64, width: f64, retained: &[NodeId]) {
    let event = event(vm, y, width, retained);
    let NodeKind::VirtualList(view) = &vm.stage().node(event.node).unwrap().kind else {
        unreachable!()
    };
    let scroll = NodeScroll {
        input: false,
        node: event.scroll,
        requested: event.requested,
        offset: event.offset,
        viewport: event.viewport,
        content: Size::new(width, view.total_height),
    };
    vm.interactions(
        session,
        &[NodeEvent::Scroll(scroll), NodeEvent::VirtualList(event)],
    )
    .unwrap();
    drive(vm, session);
}
fn click(vm: &mut SessionVm, session: &mut Session, label: &str) {
    let id = vm
        .stage()
        .nodes()
        .find(|n| matches!(&n.kind,NodeKind::Button {label:Some(s),..} if s==label))
        .unwrap()
        .id;
    vm.click(session, id).unwrap();
    drive(vm, session);
}

fn request_feedback(vm: &mut SessionVm, session: &mut Session, y: f64, retained: &[NodeId]) {
    let request = event(vm, y, 320., retained);
    let NodeKind::VirtualList(view) = &vm.stage().node(request.node).unwrap().kind else {
        unreachable!()
    };
    let scroll = NodeScroll {
        input: false,
        node: request.scroll,
        requested: request.requested,
        offset: request.offset,
        viewport: request.viewport,
        content: Size::new(320., view.total_height),
    };
    vm.interactions(
        session,
        &[NodeEvent::Scroll(scroll), NodeEvent::VirtualList(request)],
    )
    .unwrap();
}
fn next_row_boundary(vm: &mut SessionVm, session: &mut Session) {
    let nodes = vm.stage().nodes().count();
    for _ in 0..100_000 {
        match vm
            .step(
                session,
                SessionBudget::Exact(SliceBudget {
                    instructions: 100,
                    ..Default::default()
                }),
                || 0,
            )
            .unwrap()
        {
            SessionStep::Vm(RunOutcome::HostCall(t)) => {
                vm.dispatch(session, t, "en", NativeBudget::Exact(32))
                    .unwrap();
            }
            SessionStep::Vm(RunOutcome::Fault(id)) => {
                panic!("{:?}", vm.vm.delivery_fault(id).unwrap())
            }
            _ => {}
        }
        if vm.reactive_ready
            && vm.derived_task.is_none()
            && vm.vm.heap().derived_pending()
            && vm.stage().nodes().count() > nodes
        {
            return;
        }
    }
    panic!("no complete row boundary");
}

#[test]
fn virtual_list_continuous_small_moves_keep_candidate_progress_and_new_pins() {
    let (mut session, mut vm) = prepared(1000);
    for _ in 0..3 {
        feedback(&mut vm, &mut session, 0., 320., &[]);
    }
    let (list, _) = ids(&vm);
    let pin = vm.stage().node(list).unwrap().children[0];
    request_feedback(&mut vm, &mut session, 15_000., &[]);
    next_row_boundary(&mut vm, &mut session);
    assert!(vm.virtual_feedback_ready());
    assert!(!vm.stage_ready());
    // A pin added after preparation must restart the distant candidate.
    request_feedback(&mut vm, &mut session, 15_000.25, &[pin]);
    next_row_boundary(&mut vm, &mut session);
    let candidates = vm
        .stage()
        .nodes()
        .filter(|n| n.parent.is_none() && Some(n.id) != vm.stage().root())
        .map(|n| n.id)
        .collect::<Vec<_>>();
    for i in 1..5 {
        request_feedback(
            &mut vm,
            &mut session,
            15_000.25 + f64::from(i) * 0.25,
            &[pin],
        );
        next_row_boundary(&mut vm, &mut session);
        assert!(
            candidates.iter().all(|id| vm.stage().node(*id).is_some()),
            "small moves retired completed candidates"
        );
    }
    drive(&mut vm, &mut session);
    assert!(vm.stage().node(list).unwrap().children.contains(&pin));
}

#[test]
fn virtual_list_source_reorder_after_row_boundary_cannot_reuse_retired_candidates() {
    use aside_vm::snapshot::SnapshotBudget;
    let code = r#"fn opening(){
        let values=[];for i in 0..1000 {values.append(i);}
        let viewport=scroll(width:320,height:120);
        let mut reversed=false;
        let rows=list(||{
            let position=viewport.scroll_y;
            if position>15000.5 && !reversed {values.reverse();reversed=true;}
            values
        },key:|x|x,virtual:true,build:|row|column(height:40){button(f"row {row.value}");});
        viewport.add(rows); stage.replace(viewport); narrator("ready");
    }"#;
    let (mut session, mut vm) = models::prepared(code, true);
    drive(&mut vm, &mut session);
    for _ in 0..3 {
        feedback(&mut vm, &mut session, 0., 320., &[]);
    }
    request_feedback(&mut vm, &mut session, 15_000., &[]);
    next_row_boundary(&mut vm, &mut session);
    let mut capture = vm.snapshot_begin(&session).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let mut saved = capture.finish().unwrap();
    let mut restore = SessionVm::restore_begin(
        &mut saved,
        vm.vm.program().clone(),
        VmLimits::default(),
        SessionIdentity {
            session_id: SessionId(73),
            epoch: Epoch(2),
        },
        Default::default(),
        None,
        None,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let (restored_session, restored) = restore.finish().unwrap();
    session = restored_session;
    vm = restored;
    assert!(
        vm.derived_task.is_none(),
        "snapshot is between whole row factories"
    );
    let detached = vm
        .stage()
        .nodes()
        .find(|n| n.parent.is_none() && Some(n.id) != vm.stage().root())
        .expect("a detached row must still be in flight")
        .id;
    // Deliver the source-changing scroll after another factory returns but
    // before its native BUILT continuation requeues Window at the FIFO tail.
    let mut delivered = false;
    for _ in 0..100_000 {
        if let SessionStep::Vm(RunOutcome::HostCall(t)) = vm
            .step(
                &mut session,
                SessionBudget::Exact(SliceBudget {
                    instructions: 100,
                    ..Default::default()
                }),
                || 0,
            )
            .unwrap()
        {
            let p = vm.vm.program().package();
            let built = t.input == NativeInput::CallbackReturned
                && p.symbols[p.natives[t.native.index()].symbol.index()] == "reel::list";
            if built {
                request_feedback(&mut vm, &mut session, 15_001., &[]);
            }
            vm.dispatch(&mut session, t, "en", NativeBudget::Exact(32))
                .unwrap();
            if built {
                delivered = true;
                break;
            }
        }
    }
    assert!(delivered);
    assert!(
        !vm.virtual_feedback_boundary(),
        "scroll dependency queues Source before the candidate can complete"
    );
    let mut hit_source = false;
    for _ in 0..100_000 {
        if let SessionStep::Vm(RunOutcome::HostCall(t)) = vm
            .step(
                &mut session,
                SessionBudget::Exact(SliceBudget {
                    instructions: 100,
                    ..Default::default()
                }),
                || 0,
            )
            .unwrap()
        {
            let p = vm.vm.program().package();
            if t.input == NativeInput::Entry
                && p.symbols[p.natives[t.native.index()].symbol.index()] == "reel::list"
            {
                assert!(
                    vm.stage().node(detached).is_some(),
                    "source must start before the old window commits"
                );
                vm.dispatch(&mut session, t, "en", NativeBudget::Exact(32))
                    .unwrap();
                assert!(
                    vm.stage().node(detached).is_none(),
                    "source clears detached rows before copying entries"
                );
                hit_source = true;
                break;
            }
            vm.dispatch(&mut session, t, "en", NativeBudget::Exact(32))
                .unwrap();
        }
    }
    assert!(hit_source);
    drive(&mut vm, &mut session);
    let (list, _) = ids(&vm);
    let NodeKind::VirtualList(view) = &vm.stage().node(list).unwrap().kind else {
        unreachable!()
    };
    for row in view.rows.iter() {
        let child = vm
            .stage()
            .node(row.node)
            .expect("committed child remains live");
        let label = vm.stage().node(child.children[0]).unwrap();
        assert!(
            matches!(&label.kind,NodeKind::Button{label:Some(s),..} if s==&format!("row {}",999-row.index))
        );
    }
}

#[test]
fn virtual_list_materialization_depends_on_viewport_not_total_rows() {
    let mut counts = Vec::new();
    for n in [1000, 10_000, 100_000] {
        let start = std::time::Instant::now();
        let (mut session, mut vm) = prepared(n);
        let (list, _) = ids(&vm);
        let NodeKind::VirtualList(view) = &vm.stage().node(list).unwrap().kind else {
            unreachable!()
        };
        assert_eq!(view.rows.len(), 1, "only a seed row is built before layout");
        for _ in 0..3 {
            feedback(&mut vm, &mut session, 0., 320., &[]);
        }
        let nodes = vm.stage().nodes().count();
        counts.push(nodes);
        let entries = vm.vm.stats().native_entries;
        feedback(&mut vm, &mut session, 1., 320., &[]);
        assert_eq!(
            vm.vm.stats().native_entries,
            entries,
            "scroll inside the buffer does not run list natives"
        );
        click(&mut vm, &mut session, "check");
        eprintln!(
            "virtual-list rows={n} nodes={nodes} index+first-window={:?} heap={}",
            start.elapsed(),
            vm.vm.heap().stats().charged_bytes
        );
    }
    assert_eq!(counts[0], counts[1]);
    assert_eq!(counts[0], counts[2]);
}

#[test]
fn virtual_list_native_capture_survives_snapshot_and_cancellation_inside_factory() {
    use aside_vm::snapshot::SnapshotBudget;
    let code = r#"
        let mut builds=0;
        fn opening() {
            stage.replace(view(|| scroll(width:320,height:120) {
                list(|| [1,2,3],key:|x|x,virtual:true,build:|row|{
                    builds+=1;
                    let result=button("seed",height:40);
                    let mut i=0; while i<2000 {i+=1;}
                    result
                });
            }));
            if builds!=1 {throw "factory repeated";}
            narrator("ready");
        }
    "#;
    let (mut session, mut vm) = models::prepared(code, true);
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
            if vm
                .stage()
                .nodes()
                .any(|n| matches!(&n.kind,NodeKind::Button {label:Some(s),..} if s=="seed"))
            {
                break;
            }
        }
    }
    assert!(vm.vm.heap().reactive_evaluating());
    collect(&mut vm);
    let program = vm.vm.program().clone();
    vm.advance_time(&session, 0., 64).unwrap();
    let mut capture = vm.snapshot_begin(&session).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let mut saved =
        snapshot::SessionSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
    let mut restore = SessionVm::restore_begin(
        &mut saved,
        program,
        VmLimits::default(),
        SessionIdentity {
            session_id: SessionId(56),
            epoch: Epoch(15),
        },
        Default::default(),
        None,
        None,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let (mut restored_session, mut restored) = restore.finish().unwrap();
    drive(&mut restored, &mut restored_session);
    assert!(!restored.vm.heap().reactive_evaluating());
    assert_eq!(
        restored
            .stage()
            .nodes()
            .filter(|n| matches!(&n.kind,NodeKind::Button {label:Some(s),..} if s=="seed"))
            .count(),
        1
    );
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
fn virtual_list_failed_factory_restores_outer_capture_and_previous_stage() {
    let code = r#"
        fn opening() {
            let data=model({label:"old"});
            let original=text("kept"); stage.replace(original);
            let mut failed=false;
            try {
                let _ = view(|| scroll(width:320,height:120) {
                    list(|| [1],key:|x|x,virtual:true,build:|row|{let _=text("private");throw "failed";});
                });
            } catch error {failed=true;}
            if !failed {throw "missed failure";}
            original.value=derive(|| data.label);
            data.label="updated";
            narrator("ready");
        }
    "#;
    let (mut session, mut vm) = models::prepared(code, true);
    drive(&mut vm, &mut session);
    collect(&mut vm);
    vm.sync_stage(&session).unwrap();
    assert!(!vm.vm.heap().reactive_evaluating());
    assert!(
        vm.stage()
            .nodes()
            .any(|n| matches!(&n.kind,NodeKind::Text(t) if t.as_ref()=="updated"))
    );
    assert!(
        !vm.stage()
            .nodes()
            .any(|n| matches!(&n.kind,NodeKind::Text(t) if t.as_ref()=="private"))
    );
}

#[test]
fn virtual_list_old_feedback_does_not_override_a_new_script_scroll() {
    let (mut session, mut vm) = prepared(1000);
    feedback(&mut vm, &mut session, 0., 320., &[]);
    let old = event(&vm, 0., 320., &[]);
    let scroll = vm.stage.value(vm.vm.heap(), old.scroll).unwrap();
    let property = aside_vm::PropertyId {
        host: scroll,
        field: stage::scroll::ScrollField::Y.index() as u32,
    };
    vm.vm
        .heap_mut()
        .write_host_property(property, Value::float(9000.), true)
        .unwrap();
    assert!(!vm.virtual_feedback(&old).unwrap());
    assert_eq!(
        stage::number(vm.vm.heap().host_property(property).unwrap()).unwrap(),
        9000.
    );
}

#[test]
fn virtual_list_jumps_retires_rows_and_preserves_a_retained_row() {
    let (mut session, mut vm) = prepared(1000);
    for _ in 0..3 {
        feedback(&mut vm, &mut session, 0., 320., &[]);
    }
    let (list, _) = ids(&vm);
    let NodeKind::VirtualList(view) = &vm.stage().node(list).unwrap().kind else {
        unreachable!()
    };
    let retained = view.rows[0].node;
    let removed = view.rows[1].node;
    feedback(&mut vm, &mut session, 15_000., 320., &[retained]);
    assert!(vm.stage().node(retained).is_some());
    assert!(vm.stage().node(removed).is_none());
    assert!(vm.stage().nodes().count() < 70);
    feedback(&mut vm, &mut session, 15_000., 320., &[]);
    assert!(vm.stage().node(retained).is_none());
    feedback(&mut vm, &mut session, 0., 320., &[]);
    let NodeKind::VirtualList(view) = &vm.stage().node(list).unwrap().kind else {
        unreachable!()
    };
    assert_ne!(view.rows[0].node, retained);
    click(&mut vm, &mut session, "check");
    collect(&mut vm);
}

#[test]
fn virtual_list_reorder_keeps_anchor_and_snapshot_keeps_window() {
    use aside_vm::snapshot::SnapshotBudget;
    let (mut session, mut vm) = prepared(1000);
    for _ in 0..3 {
        feedback(&mut vm, &mut session, 400., 320., &[]);
    }
    let (list, scroll) = ids(&vm);
    let NodeKind::Scroll { offset, .. } = vm.stage().node(scroll).unwrap().kind else {
        unreachable!()
    };
    let NodeKind::VirtualList(view) = &vm.stage().node(list).unwrap().kind else {
        unreachable!()
    };
    let anchor = *view
        .rows
        .iter()
        .filter(|r| r.offset <= offset.y)
        .max_by_key(|r| r.index)
        .unwrap();
    let within = offset.y - anchor.offset;
    click(&mut vm, &mut session, "reverse");
    let NodeKind::VirtualList(view) = &vm.stage().node(list).unwrap().kind else {
        unreachable!()
    };
    let next = view
        .rows
        .iter()
        .find(|r| r.index == 999 - anchor.index)
        .unwrap();
    let NodeKind::Scroll { offset, .. } = vm.stage().node(scroll).unwrap().kind else {
        unreachable!()
    };
    assert_eq!(
        offset.y,
        next.offset + within,
        "reorder keeps the visible key and intra-row offset"
    );
    let before = vm.stage().node(list).unwrap().kind.clone();
    let mut capture = vm.snapshot_begin(&session).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let bytes = capture.finish().unwrap().encode().unwrap();
    let mut saved = snapshot::SessionSnapshot::decode(&bytes).unwrap();
    let mut restore = SessionVm::restore_begin(
        &mut saved,
        vm.vm.program().clone(),
        VmLimits::default(),
        SessionIdentity {
            session_id: SessionId(52),
            epoch: Epoch(13),
        },
        Default::default(),
        None,
        None,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let (mut restored_session, mut restored) = restore.finish().unwrap();
    assert_eq!(restored.stage().node(list).unwrap().kind, before);
    feedback(&mut restored, &mut restored_session, 0., 280., &[]);
    click(&mut restored, &mut restored_session, "remove");
    collect(&mut restored);
    assert!(restored.stage().nodes().count() < 70);
}

#[test]
fn virtual_list_derived_factory_snapshot_keeps_old_projection_and_latest_request() {
    use aside_vm::snapshot::SnapshotBudget;
    let (mut session, mut vm) = prepared(1000);
    for _ in 0..3 {
        feedback(&mut vm, &mut session, 0., 320., &[]);
    }
    let (list, scroll) = ids(&vm);
    let before = vm.stage().node(list).unwrap().kind.clone();
    let request = event(&vm, 15_000., 320., &[]);
    let NodeKind::VirtualList(view) = &before else {
        unreachable!()
    };
    vm.interactions(
        &mut session,
        &[
            NodeEvent::Scroll(NodeScroll {
                input: false,
                node: scroll,
                requested: request.requested,
                offset: request.offset,
                viewport: request.viewport,
                content: Size::new(320., view.total_height),
            }),
            NodeEvent::VirtualList(request),
        ],
    )
    .unwrap();
    let previous_nodes = vm.stage().nodes().count();
    for _ in 0..10_000 {
        if let SessionStep::Vm(RunOutcome::HostCall(t)) = vm
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
            vm.dispatch(&mut session, t, "en", NativeBudget::Exact(1))
                .unwrap();
            if vm.stage().nodes().count() > previous_nodes {
                break;
            }
        }
    }
    assert!(vm.derived_task.is_some());
    assert!(vm.vm.heap().reactive_evaluating());
    assert_eq!(vm.stage().node(list).unwrap().kind, before);
    vm.advance_time(&session, 0., 64).unwrap();
    let mut capture = vm.snapshot_begin(&session).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let mut saved = capture.finish().unwrap();
    let mut restore = SessionVm::restore_begin(
        &mut saved,
        vm.vm.program().clone(),
        VmLimits::default(),
        SessionIdentity {
            session_id: SessionId(59),
            epoch: Epoch(18),
        },
        Default::default(),
        None,
        None,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let (mut restored_session, mut restored) = restore.finish().unwrap();
    assert!(restored.derived_task.is_some());
    assert_eq!(
        restored.stage().node(list).unwrap().kind,
        before,
        "in-flight candidate is not published by restore"
    );
    drive(&mut restored, &mut restored_session);
    assert!(restored.derived_task.is_none());
    assert!(restored.stage().nodes().count() < 70);
    click(&mut restored, &mut restored_session, "check");

    // A newer drag supersedes the one in progress; no FIFO of distant windows.
    feedback(&mut vm, &mut session, 0., 320., &[]);
    let NodeKind::VirtualList(view) = &vm.stage().node(list).unwrap().kind else {
        unreachable!()
    };
    assert!(view.rows.iter().all(|r| r.index < 20));
    assert!(vm.stage().nodes().count() < 70);
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
    assert!(vm.derived_task.is_none());
    assert!(!vm.vm.heap().reactive_evaluating());
}

#[test]
fn virtual_list_invalid_factory_retires_candidates_and_restores_queued_fault() {
    use aside_vm::snapshot::SnapshotBudget;
    let code = r#"fn opening() {
        stage.replace(scroll(width:320,height:120) {
            list(||[0,1,2,3],key:|x|x,virtual:true,build:|row|{
                if row.value==2 {return null;}
                column(height:40) {button(f"row {row.value}");}
            });
        });
        narrator("ready");
    }"#;
    let (mut session, mut vm) = models::prepared(code, true);
    drive(&mut vm, &mut session);
    let (list, _) = ids(&vm);
    let before = vm.stage().node(list).unwrap().kind.clone();
    let nodes = vm.stage().nodes().count();
    let request = event(&vm, 0., 320., &[]);
    vm.interactions(&mut session, &[NodeEvent::VirtualList(request)])
        .unwrap();
    loop {
        match vm
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
            SessionStep::Vm(RunOutcome::HostCall(t)) => {
                if let Err(error) = vm.dispatch(&mut session, t, "en", NativeBudget::Exact(1)) {
                    vm.reject_native(&session, t, error).unwrap();
                }
            }
            SessionStep::Vm(RunOutcome::Fault(id)) => {
                assert!(
                    vm.vm
                        .delivery_fault(id)
                        .unwrap()
                        .unwrap()
                        .location
                        .source
                        .is_some()
                );
                break;
            }
            _ => {}
        }
    }
    collect(&mut vm);
    assert!(!vm.vm.heap().reactive_evaluating());
    assert_eq!(
        vm.stage().nodes().count(),
        nodes,
        "failed candidates are not held by the persistent index"
    );
    assert_eq!(vm.stage().node(list).unwrap().kind, before);
    vm.advance_time(&session, 0., 64).unwrap();
    let mut capture = vm.snapshot_begin(&session).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let mut saved = capture.finish().unwrap();
    let mut restore = SessionVm::restore_begin(
        &mut saved,
        vm.vm.program().clone(),
        VmLimits::default(),
        SessionIdentity {
            session_id: SessionId(64),
            epoch: Epoch(19),
        },
        Default::default(),
        None,
        None,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let (mut restored_session, mut restored) = restore.finish().unwrap();
    assert_eq!(restored.stage().node(list).unwrap().kind, before);
    let delivery = restored.delivery.unwrap();
    assert!(
        restored
            .vm
            .delivery_fault(delivery)
            .unwrap()
            .unwrap()
            .location
            .source
            .is_some()
    );
    assert_eq!(restored.reactive_ready, vm.reactive_ready);
    assert_eq!(restored.delivery_task, restored.derived_task);
    restored.discard_delivery(&mut restored_session).unwrap();
    assert!(restored.derived_task.is_none());
}

#[test]
fn virtual_list_follow_end_tracks_appends_only_while_at_the_end() {
    let (mut session, mut vm) = prepared_options(100, true);
    let (list, scroll) = ids(&vm);
    for _ in 0..4 {
        let NodeKind::VirtualList(view) = &vm.stage().node(list).unwrap().kind else {
            unreachable!()
        };
        let end = (view.total_height - 120.).max(0.);
        feedback(&mut vm, &mut session, end, 320., &[]);
    }
    click(&mut vm, &mut session, "append");
    let NodeKind::VirtualList(view) = &vm.stage().node(list).unwrap().kind else {
        unreachable!()
    };
    let NodeKind::Scroll { offset, .. } = vm.stage().node(scroll).unwrap().kind else {
        unreachable!()
    };
    assert_eq!(offset.y, view.total_height - 120.);
    feedback(&mut vm, &mut session, 0., 320., &[]);
    click(&mut vm, &mut session, "append");
    let NodeKind::Scroll { offset, .. } = vm.stage().node(scroll).unwrap().kind else {
        unreachable!()
    };
    assert_eq!(
        offset.y, 0.,
        "reading earlier rows disables follow-end correction"
    );
}

#[test]
#[ignore = "release timing comparison; run explicitly with --release --ignored --nocapture"]
fn virtual_list_first_window_cost_against_full_materialization() {
    for n in [1000, 10_000] {
        for virtualized in [false, true] {
            let code = format!(
                r#"
                let mut builds=0;
                fn opening() {{
                    let data=model({{items:[]}});
                    let mut i=0; while i<{n} {{data.items.append(i);i+=1;}}
                    narrator("data ready");
                    stage.replace(scroll(width:320,height:120) {{
                        list(||data.items,key:|x|x,virtual:{virtualized},build:|row|{{
                            builds+=1;
                            column(height:20+(row.value%3)*20) {{button(f"row {{row.value}}");}}
                        }});
                    }});
                    narrator("list ready");
                    stage.replace(text(f"{{builds}}"));
                    narrator("count ready");
                }}
            "#
            );
            let (mut session, mut vm) = models::prepared(&code, true);
            let generated = std::time::Instant::now();
            drive(&mut vm, &mut session);
            let source_time = generated.elapsed();
            collect(&mut vm);
            let source_bytes = vm.vm.heap().stats().charged_bytes;
            let pending = vm.pending.as_ref().unwrap().interaction;
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(pending))
                .unwrap();
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(pending))
                .unwrap();
            let construct = std::time::Instant::now();
            let mut index_start = None;
            let mut index_time = None;
            loop {
                match vm
                    .step(
                        &mut session,
                        SessionBudget::Exact(SliceBudget {
                            instructions: 1000,
                            ..Default::default()
                        }),
                        || 0,
                    )
                    .unwrap()
                {
                    SessionStep::Vm(RunOutcome::HostCall(t)) => {
                        let package = vm.vm.program().package();
                        let name =
                            &package.symbols[package.natives[t.native.index()].symbol.index()];
                        if t.input == NativeInput::Entry && name == "reel::list" {
                            index_start = Some(std::time::Instant::now());
                        }
                        if t.input == NativeInput::Entry
                            && name == "reel::button"
                            && index_time.is_none()
                        {
                            index_time = Some(index_start.unwrap().elapsed());
                        }
                        if let Err(error) =
                            vm.dispatch(&mut session, t, "en", NativeBudget::Exact(32))
                        {
                            vm.reject_native(&session, t, error).unwrap();
                        }
                    }
                    SessionStep::Idle | SessionStep::Vm(RunOutcome::Waiting(_))
                        if !vm.has_runnable_tasks() =>
                    {
                        break;
                    }
                    SessionStep::Vm(RunOutcome::Fault(id)) => {
                        panic!("{:?}", vm.vm.delivery_fault(id).unwrap())
                    }
                    SessionStep::Vm(RunOutcome::ResourceBlocked(error)) => panic!("{error:?}"),
                    _ => {}
                }
            }
            vm.sync_stage(&session).unwrap();
            if virtualized {
                for _ in 0..3 {
                    feedback(&mut vm, &mut session, 0., 320., &[]);
                }
            }
            let construct_time = construct.elapsed();
            let nodes = vm.stage().nodes().count();
            collect(&mut vm);
            let bytes = vm.vm.heap().stats().charged_bytes;
            // Read the script's factory counter after ending the measured phase.
            let pending = vm.pending.as_ref().unwrap().interaction;
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(pending))
                .unwrap();
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(pending))
                .unwrap();
            drive(&mut vm, &mut session);
            let NodeKind::Text(factory_count) =
                &vm.stage().node(vm.stage().root().unwrap()).unwrap().kind
            else {
                unreachable!()
            };
            let factories: usize = factory_count.as_ref().parse().unwrap();
            eprintln!(
                "virtual-list-cost rows={n} virtual={virtualized} source_ms={:.3} source_live_bytes={source_bytes} source_and_key_index_ms={:.3} first_window_ms={:.3} factories={factories} nodes={nodes} live_bytes={bytes} list_live_delta={}",
                source_time.as_secs_f64() * 1000.,
                index_time.unwrap().as_secs_f64() * 1000.,
                construct_time.as_secs_f64() * 1000.,
                bytes - source_bytes
            );
            if virtualized {
                assert!(nodes < 40);
                assert!(factories < 20);
            } else {
                assert_eq!(nodes, n * 2 + 2);
                assert_eq!(factories, n);
            }
        }
    }
}

mod scroll_to;

mod natural_height;
