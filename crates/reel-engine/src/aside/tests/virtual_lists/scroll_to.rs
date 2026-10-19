use super::*;
fn setup(n: usize, encoded: bool) -> (Session, SessionVm) {
    setup_source(n, encoded, false)
}
fn setup_source(n: usize, encoded: bool, upstream: bool) -> (Session, SessionVm) {
    let declaration = if upstream {
        r#"let ordered=derive(||{
        state.version;
        if !state.reverse { return state.items; }
        let copy=[]; for item in state.items { copy.append(item); } copy.reverse(); copy
    });"#
    } else {
        ""
    };
    let source = if upstream {
        "ordered.value"
    } else {
        "state.items"
    };
    let reverse = if upstream {
        "state.reverse=!state.reverse"
    } else {
        "state.items.reverse()"
    };
    let code = format!(
        r#"
    let mut sources=0; let mut keys=0; let mut builds=0; let mut request=null;
    let state=model({{items:[],status:"idle",frame:"frame idle",reverse:false,version:0}});
    fn position(items,key,align){{
        request=items.scroll_to(key,align:align);state.status="waiting";
        try {{let row=await request; row.opacity=0.75;state.status="done";}} catch error {{state.status="cancelled";}}
    }}
    fn opening(){{
        let mut i=0; while i<{n} {{state.items.append(i);i+=1;}}
        {declaration}
        let items=list(||{{sources+=1;{source}}},key:|x|{{keys+=1;x}},virtual:true,build:|row|{{builds+=1;column(height:20+(row.value%3)*20){{button(f"row {{row.value}}");}}}});
        let viewport=scroll(width:320,height:120){{items}};
        stage.replace(column{{
            viewport;text(value:derive(||state.status));text(value:derive(||state.frame));
            button("presented",on_click:||{{state.frame="frame waiting";await stage.presented(owner:items);state.frame="frame done";}});
            button("jump",on_click:|e|position(items,{n}-20,"center"));
            button("start",on_click:|e|position(items,{n}-20,"start"));
            button("end",on_click:|e|position(items,{n}-20,"end"));
            button("last",on_click:|e|position(items,{n}-1,"end"));
            button("nearest",on_click:|e|position(items,{n}-20,"nearest"));
            button("near",on_click:|e|{{request=items.scroll_to(5,align:"start");}});
            button("cancel",on_click:|e|request.cancel());
            button("reverse",on_click:|e|{{{reverse};}});
            button("unchanged",on_click:|e|{{state.version+=1;}});
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
pub(super) fn status(vm: &SessionVm, label: &str) -> bool {
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

fn feedback_height(vm: &mut SessionVm, session: &mut Session, height: f64) {
    let (list, scroll) = ids(vm);
    let NodeKind::VirtualList(view) = &vm.stage().node(list).unwrap().kind else {
        unreachable!()
    };
    let content = Size::new(320., view.total_height);
    let NodeKind::Scroll { offset, .. } = vm.stage().node(scroll).unwrap().kind else {
        unreachable!()
    };
    let y = offset.y.min((content.height - height).max(0.));
    let mut e = event(vm, y, 320., &[]);
    e.viewport.height = height;
    e.visible_end = y + height;
    vm.interactions(
        session,
        &[
            NodeEvent::Scroll(NodeScroll {
                input: false,
                node: scroll,
                requested: offset,
                offset: e.offset,
                viewport: e.viewport,
                content,
            }),
            NodeEvent::VirtualList(e),
        ],
    )
    .unwrap();
    drive(vm, session);
}

#[test]
fn virtual_list_scroll_to_viewport_height_changes_realign_without_cancelling() {
    for (action, height, clamp) in [("last", 1000., true), ("jump", 80., false)] {
        let (mut session, mut vm) = setup(1000, true);
        click(&mut vm, &mut session, action);
        layout(&mut vm, &mut session);
        let old = vm.presentation_request();
        let (_, scroll) = ids(&vm);
        let NodeKind::Scroll { offset: before, .. } = vm.stage().node(scroll).unwrap().kind else {
            unreachable!()
        };
        feedback_height(&mut vm, &mut session, height);
        if clamp {
            let NodeKind::Scroll { offset, .. } = vm.stage().node(scroll).unwrap().kind else {
                unreachable!()
            };
            assert!(
                offset.y < before.y,
                "the resized viewport clamps the previous projection"
            );
        }
        vm.accept_presentation(&session, old).unwrap();
        drive(&mut vm, &mut session);
        assert!(status(&vm, "waiting"), "{action}");
        for _ in 0..8 {
            feedback_height(&mut vm, &mut session, height);
        }
        assert!(vm.presentation_request() > old, "{action}");
        let (list, scroll) = ids(&vm);
        let NodeKind::VirtualList(view) = &vm.stage().node(list).unwrap().kind else {
            unreachable!()
        };
        let NodeKind::Scroll { offset, .. } = vm.stage().node(scroll).unwrap().kind else {
            unreachable!()
        };
        if clamp {
            assert_eq!(offset.y, view.total_height - height);
        } else {
            assert_eq!(
                offset.y,
                before.y + 20.,
                "center tracks the changed viewport height"
            );
        }
        accept(&mut vm, &mut session);
        assert!(status(&vm, "done"), "{action}");
    }
}

#[derive(Clone, Copy, Debug)]
enum Pause {
    Dirty,
    Scheduled,
    Entry,
    Index,
}

fn pause_source_update(vm: &mut SessionVm, session: &mut Session, action: &str, pause: Pause) {
    let button = vm
        .stage()
        .nodes()
        .find(|n| matches!(&n.kind, NodeKind::Button { label:Some(s), .. } if s==action))
        .unwrap()
        .id;
    vm.click(session, button).unwrap();
    for _ in 0..100_000 {
        match vm
            .step(
                session,
                SliceBudget {
                    instructions: 1,
                    ..Default::default()
                },
                || 0,
            )
            .unwrap()
        {
            SessionStep::Vm(RunOutcome::HostCall(t)) => {
                let p = vm.vm.program().package();
                let list = p.symbols[p.natives[t.native.index()].symbol.index()] == "reel::list";
                if list && matches!(pause, Pause::Entry) {
                    return;
                }
                vm.dispatch(session, t, "en", 1).unwrap();
                if list && matches!(pause, Pause::Index) {
                    return;
                }
            }
            SessionStep::Vm(RunOutcome::Returned(_)) => vm.discard_delivery(session).unwrap(),
            SessionStep::Vm(RunOutcome::Fault(id)) => panic!("{:?}", vm.vm.delivery_fault(id)),
            _ => {}
        }
        if matches!(pause, Pause::Dirty) && vm.vm.heap().derived_pending() {
            return;
        }
        if matches!(pause, Pause::Scheduled) && vm.reactive_ready && vm.vm.heap().derived_pending()
        {
            vm.step(
                session,
                SliceBudget {
                    instructions: 0,
                    work: 0,
                    deadline_ns: Some(0),
                    ..Default::default()
                },
                || 0,
            )
            .unwrap();
            assert!(vm.derived_task.is_some());
            assert!(vm.issued.is_none());
            return;
        }
    }
    panic!("source did not reach {pause:?}");
}

#[test]
fn virtual_list_scroll_to_stale_acceptance_during_source_invalidation_and_indexing() {
    for encoded in [false, true] {
        for upstream in [false, true] {
            for pause in [Pause::Dirty, Pause::Scheduled, Pause::Entry, Pause::Index] {
                let (mut session, mut vm) = setup_source(1000, encoded, upstream);
                click(&mut vm, &mut session, "jump");
                layout(&mut vm, &mut session);
                let old = vm.presentation_request();
                pause_source_update(&mut vm, &mut session, "reverse", pause);
                vm.accept_presentation(&session, old).unwrap();
                drive(&mut vm, &mut session);
                assert!(status(&vm, "waiting"), "upstream={upstream}, {pause:?}");
                layout(&mut vm, &mut session);
                assert!(vm.presentation_request() > old);
                let (list, _) = ids(&vm);
                let NodeKind::VirtualList(view) = &vm.stage().node(list).unwrap().kind else {
                    unreachable!()
                };
                assert!(view.rows.iter().any(|r| r.index == 19));
                accept(&mut vm, &mut session);
                assert!(status(&vm, "done"));
            }
        }
    }
}

#[test]
fn virtual_list_scroll_to_unchanged_upstream_still_rearms_after_invalidation() {
    let (mut session, mut vm) = setup_source(1000, true, true);
    click(&mut vm, &mut session, "jump");
    layout(&mut vm, &mut session);
    let old = vm.presentation_request();
    pause_source_update(&mut vm, &mut session, "unchanged", Pause::Dirty);
    vm.accept_presentation(&session, old).unwrap();
    drive(&mut vm, &mut session);
    assert!(status(&vm, "waiting"));
    layout(&mut vm, &mut session);
    assert!(vm.presentation_request() > old);
    accept(&mut vm, &mut session);
    assert!(status(&vm, "done"));
}

#[test]
fn virtual_list_scroll_to_source_invalidation_does_not_block_an_unrelated_list() {
    let code = r#"
        let state=model({reverse:false, first:"first waiting", second:"second waiting"});
        fn position_first(items) { await items.scroll_to(50); state.first="first done"; }
        fn position_second(items) { await items.scroll_to(50); state.second="second done"; }
        fn opening() {
            let a=[]; let b=[]; let mut i=0; while i<60 { a.append(i);b.append(59-i);i+=1; }
            let first=list(||if state.reverse { b } else { a },key:|x|x,virtual:true,build:|row|column(height:20){text(f"first {row.value}")});
            let second=list(||a,key:|x|x,virtual:true,build:|row|column(height:20){text(f"second {row.value}")});
            stage.replace(column {
                scroll(width:320,height:120){first}; scroll(width:320,height:120){second};
                text(value:derive(||state.first));text(value:derive(||state.second));
                button("first",on_click:||position_first(first));
                button("second",on_click:||position_second(second));
                button("reverse",on_click:||{state.reverse=true;});
            }); narrator("ready");
        }
    "#;
    let (mut session, mut vm) = models::prepared(code, true);
    drive(&mut vm, &mut session);
    fn layout_both(vm: &mut SessionVm, session: &mut Session) {
        for _ in 0..8 {
            let lists: Vec<_> = vm
                .stage()
                .nodes()
                .filter(|n| matches!(n.kind, NodeKind::VirtualList(_)))
                .map(|n| n.id)
                .collect();
            for list in lists {
                let node = vm.stage().node(list).unwrap();
                let scroll = node.parent.unwrap();
                let NodeKind::VirtualList(view) = &node.kind else {
                    unreachable!()
                };
                let NodeKind::Scroll { offset, .. } = vm.stage().node(scroll).unwrap().kind else {
                    unreachable!()
                };
                let viewport = Size::new(320., 120.);
                let content = Size::new(320., view.total_height);
                let e = NodeVirtualList {
                    node: list,
                    scroll,
                    revision: view.revision,
                    requested: offset,
                    offset,
                    viewport,
                    width: 320.,
                    visible_start: offset.y,
                    visible_end: offset.y + 120.,
                    measurements: view
                        .rows
                        .iter()
                        .map(|r| VirtualRowMeasurement {
                            node: r.node,
                            height: 20.,
                        })
                        .collect(),
                    retained: Vec::new().into(),
                    navigation: None,
                };
                vm.interactions(
                    session,
                    &[
                        NodeEvent::Scroll(NodeScroll {
                            input: false,
                            node: scroll,
                            requested: offset,
                            offset,
                            viewport,
                            content,
                        }),
                        NodeEvent::VirtualList(e),
                    ],
                )
                .unwrap();
                drive(vm, session);
            }
        }
    }
    layout_both(&mut vm, &mut session);
    click(&mut vm, &mut session, "first");
    click(&mut vm, &mut session, "second");
    layout_both(&mut vm, &mut session);
    let old = vm.presentation_request();
    pause_source_update(&mut vm, &mut session, "reverse", Pause::Dirty);
    vm.accept_presentation(&session, old).unwrap();
    drive(&mut vm, &mut session);
    assert!(status(&vm, "first waiting"));
    assert!(status(&vm, "second done"));
    layout_both(&mut vm, &mut session);
    accept(&mut vm, &mut session);
    assert!(status(&vm, "first done"));
}

#[test]
fn virtual_list_scroll_to_does_not_disarm_plain_presentation_with_the_same_owner() {
    for positioning in [false, true] {
        let (mut session, mut vm) = setup(1000, true);
        if positioning {
            click(&mut vm, &mut session, "jump");
            layout(&mut vm, &mut session);
        }
        click(&mut vm, &mut session, "presented");
        let old = vm.presentation_request();
        pause_source_update(&mut vm, &mut session, "reverse", Pause::Dirty);
        vm.accept_presentation(&session, old).unwrap();
        drive(&mut vm, &mut session);
        assert!(status(&vm, "frame done"));
        if positioning {
            assert!(status(&vm, "waiting"));
            layout(&mut vm, &mut session);
            accept(&mut vm, &mut session);
            assert!(status(&vm, "done"));
        }
    }
}

#[test]
fn virtual_list_scroll_to_snapshot_during_dirty_source_and_reindexing_rejects_old_frames() {
    for encoded in [false, true] {
        for pause in [Pause::Dirty, Pause::Scheduled, Pause::Entry, Pause::Index] {
            let (mut session, mut vm) = setup_source(1000, encoded, true);
            click(&mut vm, &mut session, "jump");
            layout(&mut vm, &mut session);
            let old = vm.presentation_request();
            pause_source_update(&mut vm, &mut session, "reverse", pause);
            if matches!(pause, Pause::Entry) {
                assert!(
                    vm.snapshot_begin(&session).is_err(),
                    "an issued host call must first be admitted"
                );
                vm.dispatch(&mut session, vm.issued.unwrap(), "en", 1)
                    .unwrap();
            }
            let (mut session, mut vm) = restore(&mut vm, &session);
            vm.accept_presentation(&session, old).unwrap();
            drive(&mut vm, &mut session);
            assert!(status(&vm, "waiting"), "{pause:?}");
            layout(&mut vm, &mut session);
            accept(&mut vm, &mut session);
            assert!(status(&vm, "done"), "{pause:?}");
        }
    }
}

#[test]
#[ignore = "release measurement of model-field source dependencies"]
fn virtual_list_scroll_to_model_key_dependency_cost() {
    for n in std::env::var("REEL_COST_ROWS")
        .unwrap_or_else(|_| "1000,10000,100000".into())
        .split(',')
        .map(|s| s.parse::<usize>().unwrap())
    {
        let code = format!(
            r#"
            let state=model({{items:[],status:"waiting"}});let mut keys=0;
            fn opening(){{
                let mut i=0;while i<{n}{{state.items.append({{id:i}});i+=1;}}
                stage.replace(button("build",on_click:||{{
                let items=list(||state.items,key:|item|{{keys+=1;item.id}},virtual:true,
                    build:|row|column(height:20+(row.value.id%3)*20){{text(f"row {{row.value.id}}")}});
                stage.replace(column{{scroll(width:320,height:120){{items}};text(value:derive(||state.status));
                    button("jump",on_click:||{{await items.scroll_to({n}-20,align:"center");state.status="done";}});
                    button("check",on_click:||{{if keys!={n}{{throw "source was reindexed";}}}});
                }});
                }}));narrator("ready");
            }}"#
        );
        let (mut session, mut vm) = models::prepared(&code, true);
        let source_start = std::time::Instant::now();
        eprintln!("model-key rows={n} phase=source-generation");
        drive(&mut vm, &mut session);
        eprintln!(
            "model-key rows={n} source-generation={:?}",
            source_start.elapsed()
        );
        let index_start = std::time::Instant::now();
        eprintln!("model-key rows={n} phase=index");
        click(&mut vm, &mut session, "build");
        eprintln!("model-key rows={n} index={:?}", index_start.elapsed());
        let window_start = std::time::Instant::now();
        layout(&mut vm, &mut session);
        eprintln!(
            "model-key rows={n} first-window={:?} charge={} peak_charge={}",
            window_start.elapsed(),
            vm.vm.heap().stats().charged_bytes,
            vm.vm.heap().stats().peak_charged_bytes
        );
        let (list, _) = ids(&vm);
        let host = vm.stage.value(vm.vm.heap(), list).unwrap();
        let observer = vm.vm.heap().host_payload(host, 3).unwrap();
        let query_start = std::time::Instant::now();
        assert!(vm.vm.heap_mut().derived_is_current(observer, None).unwrap());
        let first_query = query_start.elapsed();
        let stable_start = std::time::Instant::now();
        for _ in 0..9 {
            assert!(vm.vm.heap_mut().derived_is_current(observer, None).unwrap());
        }
        let stable_query = stable_start.elapsed() / 9;
        let query = query_start.elapsed() / 10;
        let start = std::time::Instant::now();
        click(&mut vm, &mut session, "jump");
        layout(&mut vm, &mut session);
        accept(&mut vm, &mut session);
        let elapsed = start.elapsed();
        assert!(status(&vm, "done"));
        click(&mut vm, &mut session, "check");
        assert!(vm.stage().nodes().count() < 80);
        let peak = vm.vm.heap().stats().peak_charged_bytes;
        let before_gc = vm.vm.heap().stats().charged_bytes;
        let gc_start = std::time::Instant::now();
        collect(&mut vm);
        let gc = gc_start.elapsed();
        assert!(status(&vm, "done"));
        eprintln!(
            "model-key rows={n} freshness={query:?} freshness_first={first_query:?} freshness_stable={stable_query:?} far-position={elapsed:?} nodes={} charge_before_gc={before_gc} charge_after_gc={} peak_charge={peak} gc={gc:?}",
            vm.stage().nodes().count(),
            vm.vm.heap().stats().charged_bytes
        );
    }
}
