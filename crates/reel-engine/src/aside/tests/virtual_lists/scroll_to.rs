use super::*;
fn setup(n: usize, encoded: bool) -> (Session, SessionVm) {
    let code = format!(
        r#"
    let mut sources=0; let mut keys=0; let mut builds=0; let mut request=null;
    let state=model({{items:[],status:"idle"}});
    fn position(items,key,align){{
        request=items.scroll_to(key,align:align);state.status="waiting";
        try {{let row=await request; row.opacity=0.75;state.status="done";}} catch error {{state.status="cancelled";}}
    }}
    fn opening(){{
        let mut i=0; while i<{n} {{state.items.append(i);i+=1;}}
        let items=list(||{{sources+=1;state.items}},key:|x|{{keys+=1;x}},virtual:true,build:|row|{{builds+=1;column(height:20+(row.value%3)*20){{button(f"row {{row.value}}");}}}});
        let viewport=scroll(width:320,height:120){{items}};
        stage.replace(column{{
            viewport;text(value:derive(||state.status));
            button("jump",on_click:|e|position(items,{n}-20,"center"));
            button("start",on_click:|e|position(items,{n}-20,"start"));
            button("end",on_click:|e|position(items,{n}-20,"end"));
            button("nearest",on_click:|e|position(items,{n}-20,"nearest"));
            button("near",on_click:|e|{{request=items.scroll_to(5,align:"start");}});
            button("cancel",on_click:|e|request.cancel());
            button("reverse",on_click:|e|state.items.reverse());
            button("remove",on_click:|e|state.items.remove({n}-20));
            button("retire",on_click:|e|items.remove());
            button("position",on_click:|e|{{viewport.scroll_y=0;}});
            button("index",on_click:|e|{{if sources!=1||keys!={n}||builds>70{{throw "jump rebuilt source or intermediate rows";}}}});
        }});
        narrator("ready");
    }}"#
    );
    let (mut session, mut vm) = models::prepared(&code, encoded);
    drive(&mut vm, &mut session);
    for _ in 0..3 {
        feedback(&mut vm, &mut session, 0., 320., &[]);
    }
    (session, vm)
}
fn status(vm: &SessionVm, label: &str) -> bool {
    vm.stage()
        .nodes()
        .any(|n| matches!(&n.kind,NodeKind::Text(t) if t.as_ref()==label))
}
fn layout(vm: &mut SessionVm, session: &mut Session) {
    for _ in 0..8 {
        let (_, scroll) = ids(vm);
        let NodeKind::Scroll { offset, .. } = vm.stage().node(scroll).unwrap().kind else {
            unreachable!()
        };
        feedback(vm, session, offset.y, 320., &[]);
    }
}
fn accept(vm: &mut SessionVm, session: &mut Session) {
    vm.accept_presentation(session, vm.presentation_request())
        .unwrap();
    drive(vm, session);
}
#[test]
fn virtual_list_scroll_to_measures_then_waits_for_an_accepted_frame() {
    for encoded in [false, true] {
        let (mut session, mut vm) = setup(1000, encoded);
        click(&mut vm, &mut session, "jump");
        assert!(status(&vm, "waiting"));
        assert_eq!(vm.presentation_request(), 0);
        layout(&mut vm, &mut session);
        assert!(vm.presentation_request() > 0);
        assert!(status(&vm, "waiting"));
        vm.accept_presentation(&session, 0).unwrap();
        drive(&mut vm, &mut session);
        assert!(status(&vm, "waiting"));
        accept(&mut vm, &mut session);
        assert!(status(&vm, "done"));
        assert!(vm.stage().nodes().any(|n| n.props.opacity == 0.75));
        click(&mut vm, &mut session, "index");
    }
}
#[test]
fn virtual_list_scroll_to_tracks_keys_and_invalidates_armed_old_frames() {
    let (mut session, mut vm) = setup(1000, true);
    click(&mut vm, &mut session, "jump");
    layout(&mut vm, &mut session);
    let stale = vm.presentation_request();
    click(&mut vm, &mut session, "reverse");
    vm.accept_presentation(&session, stale).unwrap();
    drive(&mut vm, &mut session);
    assert!(status(&vm, "waiting"));
    layout(&mut vm, &mut session);
    assert!(vm.presentation_request() > stale);
    let (list, _) = ids(&vm);
    let NodeKind::VirtualList(view) = &vm.stage().node(list).unwrap().kind else {
        unreachable!()
    };
    assert!(view.rows.iter().any(|r| r.index == 19));
    assert!(view.rows.len() < 30);
    accept(&mut vm, &mut session);
    assert!(status(&vm, "done"));
}
#[test]
fn virtual_list_scroll_to_cancels_for_removed_retired_cancelled_and_newer_targets() {
    for action in ["remove", "retire", "cancel", "near", "position"] {
        let (mut session, mut vm) = setup(1000, true);
        click(&mut vm, &mut session, "jump");
        layout(&mut vm, &mut session);
        click(&mut vm, &mut session, action);
        if action == "position" {
            feedback(&mut vm, &mut session, 0., 320., &[]);
        }
        assert!(status(&vm, "cancelled"), "{action}");
        accept(&mut vm, &mut session);
        assert!(status(&vm, "cancelled"), "{action}");
    }
}
#[test]
fn virtual_list_scroll_to_far_window_is_bounded_without_reindexing() {
    let (mut session, mut vm) = setup(100_000, true);
    let entries = vm.vm.stats().native_entries;
    let start = std::time::Instant::now();
    click(&mut vm, &mut session, "jump");
    layout(&mut vm, &mut session);
    accept(&mut vm, &mut session);
    assert!(status(&vm, "done"));
    assert!(vm.stage().nodes().count() < 80);
    click(&mut vm, &mut session, "index");
    eprintln!(
        "scroll-to rows=100000 elapsed={:?} native_entries={} nodes={}",
        start.elapsed(),
        vm.vm.stats().native_entries - entries,
        vm.stage().nodes().count()
    );
}

fn restore(vm: &mut SessionVm, session: &Session) -> (Session, SessionVm) {
    use aside_vm::snapshot::SnapshotBudget;
    vm.advance_time(session, 0., 64).unwrap();
    let mut capture = vm.snapshot_begin(session).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let bytes = capture.finish().unwrap().encode().unwrap();
    let mut saved = snapshot::SessionSnapshot::decode(&bytes).unwrap();
    let mut restore = SessionVm::restore_begin(
        &mut saved,
        vm.vm.program().clone(),
        VmLimits::default(),
        SessionIdentity {
            session_id: SessionId(71),
            epoch: Epoch(19),
        },
        Default::default(),
        None,
        None,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    restore.finish().unwrap()
}
#[test]
fn virtual_list_scroll_to_snapshot_before_measurement_inside_factory_and_after_arming() {
    for phase in 0..3 {
        let (mut session, mut vm) = setup(1000, true);
        let (list, _) = ids(&vm);
        if phase == 1 {
            let before = vm.stage().node(list).unwrap().kind.clone();
            let nodes = vm.stage().nodes().count();
            let button = vm
                .stage()
                .nodes()
                .find(|n| matches!(&n.kind,NodeKind::Button{label:Some(label),..}if label=="jump"))
                .unwrap()
                .id;
            vm.click(&mut session, button).unwrap();
            for _ in 0..10_000 {
                match vm
                    .step(
                        &mut session,
                        SliceBudget {
                            instructions: 1,
                            ..Default::default()
                        },
                        || 0,
                    )
                    .unwrap()
                {
                    SessionStep::Vm(RunOutcome::HostCall(t)) => {
                        vm.dispatch(&mut session, t, "en", 1).unwrap();
                    }
                    SessionStep::Vm(RunOutcome::Returned(_)) => {
                        vm.discard_delivery(&mut session).unwrap()
                    }
                    _ => {}
                }
                if vm.derived_task.is_some() && vm.stage().nodes().count() > nodes {
                    break;
                }
            }
            assert!(vm.derived_task.is_some());
            assert_eq!(vm.stage().node(list).unwrap().kind, before);
        } else {
            click(&mut vm, &mut session, "jump");
            if phase == 2 {
                layout(&mut vm, &mut session);
            }
        }
        let old = vm.presentation_request();
        let (mut restored_session, mut restored) = restore(&mut vm, &session);
        restored
            .accept_presentation(&restored_session, old)
            .unwrap();
        drive(&mut restored, &mut restored_session);
        assert!(status(&restored, "waiting"), "phase {phase}");
        layout(&mut restored, &mut restored_session);
        assert!(restored.presentation_request() > old, "phase {phase}");
        accept(&mut restored, &mut restored_session);
        assert!(status(&restored, "done"), "phase {phase}");
        click(&mut restored, &mut restored_session, "index");
    }
}
#[test]
fn virtual_list_scroll_to_new_target_discards_inflight_far_candidate() {
    let (mut session, mut vm) = setup(1000, true);
    let initial = vm.stage().nodes().count();
    let jump = vm
        .stage()
        .nodes()
        .find(|n| matches!(&n.kind,NodeKind::Button{label:Some(label),..}if label=="jump"))
        .unwrap()
        .id;
    vm.click(&mut session, jump).unwrap();
    for _ in 0..10_000 {
        if let SessionStep::Vm(RunOutcome::HostCall(t)) = vm
            .step(
                &mut session,
                SliceBudget {
                    instructions: 1,
                    ..Default::default()
                },
                || 0,
            )
            .unwrap()
        {
            vm.dispatch(&mut session, t, "en", 1).unwrap();
        }
        if vm.derived_task.is_some() && vm.stage().nodes().count() > initial {
            break;
        }
    }
    assert!(vm.derived_task.is_some());
    // A newer physical scroll owns the viewport while the old row factory is paused.
    let stale = event(&vm, 0., 320., &[]);
    let e = event(&vm, 1., 320., &[]);
    let (_, scroll) = ids(&vm);
    let NodeKind::VirtualList(view) = &vm.stage().node(e.node).unwrap().kind else {
        unreachable!()
    };
    let content = Size::new(320., view.total_height);
    vm.interactions(
        &mut session,
        &[
            NodeEvent::Scroll(NodeScroll {
                input: true,
                node: scroll,
                requested: e.requested,
                offset: e.offset,
                viewport: e.viewport,
                content,
            }),
            NodeEvent::VirtualList(e),
        ],
    )
    .unwrap();
    drive(&mut vm, &mut session);
    layout(&mut vm, &mut session);
    vm.interactions(
        &mut session,
        &[
            NodeEvent::Scroll(NodeScroll {
                input: false,
                node: scroll,
                requested: stale.requested,
                offset: stale.offset,
                viewport: stale.viewport,
                content,
            }),
            NodeEvent::VirtualList(stale),
        ],
    )
    .unwrap();
    drive(&mut vm, &mut session);
    let NodeKind::Scroll { offset, .. } = vm.stage().node(scroll).unwrap().kind else {
        unreachable!()
    };
    assert_eq!(offset.y, 1., "old layout must not take back user scroll");
    assert!(status(&vm, "cancelled"));
    assert!(vm.stage().nodes().count() < 80);
    let (list, _) = ids(&vm);
    let NodeKind::VirtualList(view) = &vm.stage().node(list).unwrap().kind else {
        unreachable!()
    };
    assert!(view.rows.iter().all(|r| r.index < 30));
}

#[test]
fn virtual_list_scroll_to_alignment_and_visible_target_still_wait_for_new_frame() {
    for (action, align) in [("start", 0.), ("jump", 0.5), ("end", 1.)] {
        let (mut session, mut vm) = setup(1000, true);
        click(&mut vm, &mut session, action);
        layout(&mut vm, &mut session);
        let (list, scroll) = ids(&vm);
        let NodeKind::VirtualList(view) = &vm.stage().node(list).unwrap().kind else {
            unreachable!()
        };
        let row = view.rows.iter().find(|r| r.index == 980).unwrap();
        let height = vm.stage().node(row.node).unwrap().props.height.unwrap();
        let NodeKind::Scroll { offset, .. } = vm.stage().node(scroll).unwrap().kind else {
            unreachable!()
        };
        assert_eq!(offset.y, row.offset + (height - 120.) * align, "{action}");
        accept(&mut vm, &mut session);
        let old = vm.presentation_request();
        click(&mut vm, &mut session, "nearest");
        layout(&mut vm, &mut session);
        let NodeKind::Scroll { offset: next, .. } = vm.stage().node(scroll).unwrap().kind else {
            unreachable!()
        };
        assert_eq!(offset, next);
        assert!(status(&vm, "waiting"));
        assert!(vm.presentation_request() > old);
        vm.accept_presentation(&session, old).unwrap();
        drive(&mut vm, &mut session);
        assert!(status(&vm, "waiting"));
        accept(&mut vm, &mut session);
        assert!(status(&vm, "done"));
    }
}

#[test]
fn virtual_list_scroll_to_remeasures_a_width_change_before_acceptance() {
    let (mut session, mut vm) = setup(1000, true);
    click(&mut vm, &mut session, "jump");
    layout(&mut vm, &mut session);
    let old = vm.presentation_request();
    let (_, scroll) = ids(&vm);
    let NodeKind::Scroll { offset, .. } = vm.stage().node(scroll).unwrap().kind else {
        unreachable!()
    };
    feedback(&mut vm, &mut session, offset.y, 160., &[]);
    vm.accept_presentation(&session, old).unwrap();
    drive(&mut vm, &mut session);
    assert!(status(&vm, "waiting"));
    layout(&mut vm, &mut session);
    assert!(vm.presentation_request() > old);
    accept(&mut vm, &mut session);
    assert!(status(&vm, "done"));
}
