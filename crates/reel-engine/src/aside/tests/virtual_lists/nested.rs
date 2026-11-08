use super::*;

#[test]
fn empty_viewport_retires_normal_rows_and_preserves_retained_rows() {
    for retained in [false, true] {
        let (mut session, mut vm) = prepared(1_000);
        let mut e = event(&vm, 0., 320., &[]);
        let row = e.measurements[0].node;
        e.visible_end = e.visible_start;
        if retained {
            e.retained = [row].into();
        }
        vm.interactions(&mut session, &[NodeEvent::VirtualList(e)])
            .unwrap();
        drive(&mut vm, &mut session);
        let (list, _) = ids(&vm);
        let NodeKind::VirtualList(view) = &vm.stage().node(list).unwrap().kind else {
            panic!()
        };
        assert_eq!(view.rows.len(), usize::from(retained));
        if retained {
            assert_eq!(view.rows[0].node, row);
        }
        for _ in 0..3 {
            feedback(&mut vm, &mut session, 0., 320., &[]);
        }
        let NodeKind::VirtualList(view) = &vm.stage().node(list).unwrap().kind else {
            panic!()
        };
        assert!(
            view.rows.len() > 1,
            "visible rows are rebuilt when the viewport reopens"
        );
    }
}

#[test]
fn empty_clip_retains_target_until_its_viewport_reopens() {
    let (mut session, mut vm) = scroll_to::setup(100, true);
    let mut empty = event(&vm, 0., 320., &[]);
    empty.visible_end = empty.visible_start;
    vm.interactions(&mut session, &[NodeEvent::VirtualList(empty)])
        .unwrap();
    drive(&mut vm, &mut session);
    click(&mut vm, &mut session, "near");
    for _ in 0..4 {
        let (_, owner) = ids(&vm);
        let NodeKind::Scroll { offset, .. } = vm.stage().node(owner).unwrap().kind else {
            panic!()
        };
        let mut empty = event(&vm, offset.y, 320., &[]);
        empty.visible_end = empty.visible_start;
        vm.interactions(&mut session, &[NodeEvent::VirtualList(empty)])
            .unwrap();
        drive(&mut vm, &mut session);
        let (list, _) = ids(&vm);
        let NodeKind::VirtualList(view) = &vm.stage().node(list).unwrap().kind else {
            panic!()
        };
        assert_eq!(
            view.rows.len(),
            1,
            "only the explicit target remains mounted"
        );
        assert_eq!(view.rows[0].index, 5);
        assert_eq!(
            vm.presentation_request(),
            0,
            "an empty clip cannot arm presentation"
        );
    }
    for _ in 0..8 {
        scaled_feedback(&mut vm, &mut session, 1.);
    }
    assert!(
        vm.presentation_request() > 0,
        "the measured target rearms when its clip opens"
    );
}

fn scaled_feedback(vm: &mut SessionVm, session: &mut Session, scale: f64) {
    let (_, owner) = ids(vm);
    let NodeKind::Scroll { offset, .. } = vm.stage().node(owner).unwrap().kind else {
        panic!()
    };
    let mut e = event(vm, offset.y, 320., &[]);
    e.scroll_scale = 1. / scale;
    e.visible_start = offset.y / scale;
    e.visible_end = e.visible_start + 120. / scale;
    vm.interactions(session, &[NodeEvent::VirtualList(e)])
        .unwrap();
    drive(vm, session);
}

#[test]
fn scroll_to_and_anchor_use_owner_to_list_scale() {
    for scale in [0.5, 2.] {
        let (mut session, mut vm) = scroll_to::setup(100, true);
        for _ in 0..3 {
            scaled_feedback(&mut vm, &mut session, scale);
        }
        click(&mut vm, &mut session, "near");
        for _ in 0..8 {
            scaled_feedback(&mut vm, &mut session, scale);
        }
        let (list, owner) = ids(&vm);
        let NodeKind::VirtualList(view) = &vm.stage().node(list).unwrap().kind else {
            panic!()
        };
        let row = view.rows.iter().find(|row| row.index == 5).unwrap();
        let NodeKind::Scroll { offset, .. } = vm.stage().node(owner).unwrap().kind else {
            panic!()
        };
        assert!(
            (offset.y - row.offset * scale).abs() < 0.01,
            "scale={scale}: {offset:?}, row={row:?}"
        );
        vm.accept_presentation(&mut session, vm.presentation_request())
            .unwrap();
        drive(&mut vm, &mut session);
        let previous_offset = offset.y;
        let NodeKind::VirtualList(view) = &vm.stage().node(list).unwrap().kind else {
            panic!()
        };
        let above = view.rows.iter().find(|row| row.index == 4).unwrap().node;
        let mut changed = event(&vm, previous_offset, 320., &[]);
        changed.scroll_scale = 1. / scale;
        changed.visible_start = previous_offset / scale;
        changed.visible_end = changed.visible_start + 120. / scale;
        let mut measurements = changed.measurements.to_vec();
        measurements
            .iter_mut()
            .find(|row| row.node == above)
            .unwrap()
            .height += 20.;
        changed.measurements = measurements.into();
        vm.interactions(&mut session, &[NodeEvent::VirtualList(changed)])
            .unwrap();
        drive(&mut vm, &mut session);
        let NodeKind::Scroll { offset, .. } = vm.stage().node(owner).unwrap().kind else {
            panic!()
        };
        assert!((offset.y - previous_offset - 20. * scale).abs() < 0.01);
    }
}

#[test]
fn restored_scaled_target_waits_for_new_geometry_before_rearming() {
    let (mut session, mut vm) = scroll_to::setup(100, true);
    for _ in 0..3 {
        scaled_feedback(&mut vm, &mut session, 2.);
    }
    click(&mut vm, &mut session, "near");
    for _ in 0..8 {
        scaled_feedback(&mut vm, &mut session, 2.);
    }
    let old = vm.presentation_request();
    assert!(old > 0);
    let (mut session, mut vm) = scroll_to::restore(&mut vm, &session);
    let (list, owner) = ids(&vm);
    assert_eq!(
        vm.stage.virtual_scrolls.get(owner).unwrap().as_ref(),
        &[list]
    );
    assert!(vm.stage.virtual_scrolls.geometry(list).is_none());
    let offset = match vm.stage().node(owner).unwrap().kind {
        NodeKind::Scroll { offset, .. } => offset,
        _ => panic!(),
    };
    vm.accept_presentation(&session, old).unwrap();
    drive(&mut vm, &mut session);
    assert!(vm.stage.virtual_scrolls.geometry(list).is_none());
    assert!(matches!(vm.stage().node(owner).unwrap().kind,
        NodeKind::Scroll { offset: restored, .. } if restored == offset));
    for _ in 0..8 {
        scaled_feedback(&mut vm, &mut session, 2.);
    }
    assert!(vm.presentation_request() > old);
}

fn owner_feedback(vm: &mut SessionVm, session: &mut Session) {
    let lists: Vec<_> = vm
        .stage()
        .nodes()
        .filter(|n| matches!(n.kind, NodeKind::VirtualList(_)))
        .map(|n| n.id)
        .collect();
    for list in lists {
        let mut owner = vm.stage().node(list).unwrap().parent.unwrap();
        while !matches!(
            vm.stage().node(owner).unwrap().kind,
            NodeKind::Scroll { .. }
        ) {
            owner = vm.stage().node(owner).unwrap().parent.unwrap();
        }
        let NodeKind::Scroll { offset, .. } = vm.stage().node(owner).unwrap().kind else {
            panic!()
        };
        let NodeKind::VirtualList(view) = &vm.stage().node(list).unwrap().kind else {
            panic!()
        };
        let event = NodeVirtualList {
            node: list,
            scroll: owner,
            revision: view.revision,
            requested: offset,
            offset,
            viewport: Size::new(320., 120.),
            scroll_scale: 1.,
            scroll_limit: 10_000.,
            width: 320.,
            visible_start: offset.y,
            visible_end: offset.y + 120.,
            measurements: view
                .rows
                .iter()
                .map(|row| VirtualRowMeasurement {
                    node: row.node,
                    height: 40.,
                })
                .collect(),
            retained: [].into(),
            navigation: None,
        };
        vm.interactions(session, &[NodeEvent::VirtualList(event)])
            .unwrap();
    }
    drive(vm, session);
}

#[test]
fn manual_scroll_cancels_every_bound_nested_list_for_that_owner() {
    let code = r#"
    let state=model({a:"idle",b:"idle",c:"idle"});
    fn position(items,field){
        state[field]="waiting";
        try { await items.scroll_to(50); state[field]="done"; }
        catch error { state[field]="cancelled"; }
    }
    fn opening(){
        let values=[];let mut i=0;while i<100{values.append(i);i+=1;}
        let a=list(||values,key:|x|x,virtual:true,build:|row|column(height:40));
        let b=list(||values,key:|x|x,virtual:true,build:|row|column(height:40));
        let c=list(||values,key:|x|x,virtual:true,build:|row|column(height:40));
        stage.replace(column{
            scroll(width:320,height:120){column{input{a};column{b}}};
            scroll(width:320,height:120){input{c}};
            text(value:derive(||state.a+"/"+state.b+"/"+state.c));
            button("all",on_click:||{
                spawn(||position(a,"a"));spawn(||position(b,"b"));spawn(||position(c,"c"));
            });
        });
        narrator("ready");
    }"#;
    let (mut session, mut vm) = models::prepared(code, true);
    drive(&mut vm, &mut session);
    for _ in 0..3 {
        owner_feedback(&mut vm, &mut session);
    }
    let owner = vm
        .stage()
        .nodes()
        .find(|node| matches!(node.kind, NodeKind::Scroll { .. }))
        .unwrap()
        .id;
    click(&mut vm, &mut session, "all");
    assert!(scroll_to::status(&vm, "waiting/waiting/waiting"));
    let NodeKind::Scroll {
        offset: requested, ..
    } = vm.stage().node(owner).unwrap().kind
    else {
        panic!()
    };
    let clamped = (requested.y - 20.).max(0.);
    vm.interactions(
        &mut session,
        &[NodeEvent::Scroll(NodeScroll {
            input: false,
            node: owner,
            requested,
            offset: Vec2::new(0., clamped),
            viewport: Size::new(320., 120.),
            content: Size::new(320., clamped + 120.),
        })],
    )
    .unwrap();
    drive(&mut vm, &mut session);
    assert!(
        scroll_to::status(&vm, "waiting/waiting/waiting"),
        "layout clamps preserve targets for every list sharing this owner"
    );
    vm.virtual_scroll_input(owner).unwrap();
    drive(&mut vm, &mut session);
    assert!(scroll_to::status(&vm, "cancelled/cancelled/waiting"));
    assert_eq!(vm.stage.virtual_scrolls.get(owner).unwrap().len(), 2);
}

#[test]
fn unpositionable_scroll_axis_reports_a_native_source_fault() {
    let (mut session, mut vm) = scroll_to::setup(100, true);
    let mut e = event(&vm, 0., 320., &[]);
    e.scroll_scale = 0.;
    vm.interactions(&mut session, &[NodeEvent::VirtualList(e)])
        .unwrap();
    drive(&mut vm, &mut session);
    let near = vm.stage().nodes().find(|node|
        matches!(&node.kind, NodeKind::Button { label: Some(label), .. } if label == "near")
    ).unwrap().id;
    vm.click(&mut session, near).unwrap();
    let mut rejected = false;
    for _ in 0..10_000 {
        match vm
            .step(
                &mut session,
                SessionBudget::Exact(SliceBudget::default()),
                || 0,
            )
            .unwrap()
        {
            SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                if let Err(error) = vm.dispatch(&mut session, ticket, "en", NativeBudget::Exact(32))
                {
                    assert!(format!("{error:?}").contains("vertical axis"));
                    assert!(ticket.location.source.is_some());
                    vm.reject_native(&session, ticket, error).unwrap();
                    rejected = true;
                }
            }
            SessionStep::Vm(RunOutcome::Returned(_)) => vm.discard_delivery(&mut session).unwrap(),
            SessionStep::Vm(RunOutcome::Fault(id)) => {
                assert!(rejected);
                assert!(
                    vm.vm
                        .delivery_fault(id)
                        .unwrap()
                        .unwrap()
                        .location
                        .source
                        .is_some()
                );
                return;
            }
            _ => {}
        }
    }
    panic!("unpositionable target did not produce a source fault");
}
