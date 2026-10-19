use super::*;
use reel_ui::stage::VirtualListView;

fn view(vm: &SessionVm) -> &VirtualListView {
    let NodeKind::VirtualList(view) = &vm.stage().node(ids(vm).0).unwrap().kind else {
        unreachable!()
    };
    view
}
fn offset(vm: &SessionVm) -> f64 {
    let NodeKind::Scroll { offset, .. } = vm.stage().node(ids(vm).1).unwrap().kind else {
        unreachable!()
    };
    offset.y
}
fn gallery(n: usize, declared: bool, encoded: bool) -> (Session, SessionVm) {
    let height = if declared {
        "row_height:|item|{heights+=1;item.height},"
    } else {
        ""
    };
    let code = format!(
        r#"
        let mut sources=0;let mut keys=0;let mut heights=0;let mut builds=0;
        let state=model({{items:[],status:"idle"}});
        fn opening() {{
            for i in 0..{n} {{ state.items.append(model({{id:i,height:if i==0 {{217}} else if i%10==0 {{277}} else {{187}}}})); }}
            let items=list(||{{sources+=1;state.items}},key:|item|{{keys+=1;item.id}},virtual:true,{height}
                build:|row|{{builds+=1;column(height:derive(||row.value.height))}});
            stage.replace(column {{
                scroll(width:1313,height:920){{items}};
                text(value:derive(||state.status));
                button("jump",on_click:||{{state.status="waiting";await items.scroll_to({n}-20,align:"center");state.status="done";}});
                button("grow",on_click:||{{state.items[0].height+=80;}});
                button("replace",on_click:||{{state.items[1]=model({{id:1,height:300}});}});
                button("invalid",on_click:||{{state.items[1].height=0;}});
                button("reverse",on_click:||state.items.reverse());
                button("check",on_click:||{{if sources!=1||keys!={n}||heights!={expected_heights} {{throw "scroll recomputed size source";}}}});
            }});
            narrator("ready");
        }}
    "#,
        expected_heights = if declared { n } else { 0 }
    );
    models::prepared(&code, encoded)
}
fn frame(vm: &mut SessionVm, session: &mut Session, y: f64, width: f64) {
    let mut e = event(vm, y, width, &[]);
    e.viewport.height = 920.;
    e.visible_end = y + 920.;
    let scroll = NodeScroll {
        input: false,
        node: e.scroll,
        requested: e.requested,
        offset: e.offset,
        viewport: e.viewport,
        content: Size::new(width, view(vm).total_height),
    };
    vm.interactions(
        session,
        &[NodeEvent::Scroll(scroll), NodeEvent::VirtualList(e)],
    )
    .unwrap();
    drive(vm, session);
}
fn settle_layout(vm: &mut SessionVm, session: &mut Session) {
    for _ in 0..4 {
        frame(vm, session, offset(vm), 1313.);
    }
}
fn total(n: usize) -> f64 {
    (0..n)
        .map(|i| {
            if i == 0 {
                217.
            } else if i % 10 == 0 {
                277.
            } else {
                187.
            }
        })
        .sum()
}

#[test]
fn virtual_list_row_height_keeps_gallery_range_and_neighbor_positions_while_dragging() {
    for encoded in [false, true] {
        let (mut session, mut vm) = gallery(80, true, encoded);
        drive(&mut vm, &mut session);
        assert_eq!(view(&vm).total_height, total(80));
        assert_eq!(
            view(&vm).rows.len(),
            1,
            "only the seed is constructed before layout"
        );
        settle_layout(&mut vm, &mut session);
        for fraction in [0.8, 0.1, 0.55, 0.93] {
            // Same conversion used by an authored thumb: position / track * scroll range.
            let content = view(&vm).total_height;
            let thumb = 920. * 920. / content;
            let pointer = fraction * (920. - thumb) + thumb * 0.3;
            let requested = (pointer - thumb * 0.3) / (920. - thumb) * (content - 920.);
            frame(&mut vm, &mut session, requested, 1313.);
            let initial = view(&vm)
                .rows
                .iter()
                .map(|r| (r.node, r.offset - requested))
                .collect::<Vec<_>>();
            for width in [1313., 1000., 1313.] {
                let y = offset(&vm);
                frame(&mut vm, &mut session, y, width);
                assert_eq!(view(&vm).total_height, content);
                assert_eq!(offset(&vm), requested);
                for &(node, y) in &initial {
                    if let Some(row) = view(&vm).rows.iter().find(|r| r.node == node) {
                        assert_eq!(row.offset - offset(&vm), y);
                    }
                }
            }
        }
        click(&mut vm, &mut session, "check");
        assert!(view(&vm).rows.len() < 20);
    }
}

#[test]
fn virtual_list_row_height_changes_and_replacements_keep_key_anchor() {
    let (mut session, mut vm) = gallery(80, true, true);
    drive(&mut vm, &mut session);
    settle_layout(&mut vm, &mut session);
    frame(&mut vm, &mut session, 5000., 1313.);
    let anchor = view(&vm)
        .rows
        .iter()
        .rev()
        .find(|r| r.offset <= offset(&vm))
        .unwrap()
        .clone();
    let screen = anchor.offset - offset(&vm);
    click(&mut vm, &mut session, "grow");
    assert_eq!(view(&vm).total_height, total(80) + 80.);
    assert_eq!(offset(&vm), 5080.);
    click(&mut vm, &mut session, "replace");
    assert_eq!(view(&vm).total_height, total(80) + 80. + 113.);
    let current = view(&vm)
        .rows
        .iter()
        .find(|r| r.node == anchor.node)
        .unwrap();
    assert_eq!(current.offset - offset(&vm), screen);
    click(&mut vm, &mut session, "reverse");
    let current = view(&vm)
        .rows
        .iter()
        .find(|r| r.node == anchor.node)
        .unwrap();
    assert_eq!(current.offset - offset(&vm), screen);
    assert_eq!(current.index, 79 - anchor.index);
}

#[test]
fn virtual_list_row_height_still_waits_for_real_layout_and_presentation() {
    let (mut session, mut vm) = gallery(80, true, true);
    drive(&mut vm, &mut session);
    settle_layout(&mut vm, &mut session);
    click(&mut vm, &mut session, "jump");
    assert!(scroll_to::status(&vm, "waiting"));
    vm.accept_presentation(&session, vm.presentation_request())
        .unwrap();
    drive(&mut vm, &mut session);
    assert!(
        scroll_to::status(&vm, "waiting"),
        "exact height is not an accepted frame"
    );
    settle_layout(&mut vm, &mut session);
    vm.accept_presentation(&session, vm.presentation_request())
        .unwrap();
    drive(&mut vm, &mut session);
    assert!(scroll_to::status(&vm, "done"));
    let row = view(&vm).rows.iter().find(|r| r.index == 60).unwrap();
    assert_eq!(row.offset - offset(&vm), (920. - 277.) / 2.);
}

#[test]
fn virtual_list_row_height_constant_validation_and_failed_update_are_transactional() {
    for encoded in [false, true] {
        let code = r#"fn opening(){
            let mut errors=0;
            try {list(||[],key:|x|x,virtual:true,row_height:0,build:|row|column());} catch e {errors+=1;}
            try {list(||[],key:|x|x,virtual:true,row_height:-1,build:|row|column());} catch e {errors+=1;}
            try {list(||[1],key:|x|x,virtual:true,row_height:|item|0,build:|row|column());} catch e {errors+=1;}
            try {list(||[1],key:|x|x,row_height:40,build:|row|column());} catch e {errors+=1;}
            try {list(||[],key:|x|x,virtual:true,row_height:"bad",build:|row|column());} catch e {errors+=1;}
            try {list(||[1],key:|x|x,virtual:true,row_height:|item|{await delay(0);40},build:|row|column());} catch e {errors+=1;}
            if errors!=6 {throw "height validation";}
            stage.replace(scroll(width:320,height:120){list(||[1,2,3],key:|x|x,virtual:true,row_height:40,build:|row|column(height:999));});
            narrator("ready");
        }"#;
        let (mut session, mut vm) = models::prepared(code, encoded);
        drive(&mut vm, &mut session);
        assert_eq!(view(&vm).total_height, 120.);
        feedback(&mut vm, &mut session, 0., 320., &[]);
        assert!(view(&vm).rows.iter().all(|r| r.height == Some(40.)));
    }
    let (mut session, mut vm) = gallery(80, true, true);
    drive(&mut vm, &mut session);
    settle_layout(&mut vm, &mut session);
    let before = vm.stage().node(ids(&vm).0).unwrap().kind.clone();
    let id = vm
        .stage()
        .nodes()
        .find(|n| matches!(&n.kind,NodeKind::Button{label:Some(s),..} if s=="invalid"))
        .unwrap()
        .id;
    vm.click(&mut session, id).unwrap();
    let mut failed = false;
    for _ in 0..10000 {
        match vm.step(&mut session, SliceBudget::default(), || 0).unwrap() {
            SessionStep::Vm(RunOutcome::HostCall(t)) => {
                if let Err(e) = vm.dispatch(&mut session, t, "en", 32) {
                    vm.reject_native(&mut session, t, e).unwrap();
                }
            }
            SessionStep::Vm(RunOutcome::Fault(_)) => {
                failed = true;
                break;
            }
            SessionStep::Vm(RunOutcome::Returned(_)) => vm.discard_delivery(&mut session).unwrap(),
            _ => {}
        }
    }
    assert!(failed);
    assert_eq!(vm.stage().node(ids(&vm).0).unwrap().kind, before);
}

#[test]
fn virtual_list_row_height_snapshot_resumes_the_size_callback_and_target_once() {
    use aside_vm::snapshot::SnapshotBudget;
    let code = r#"
        let mut calls=0;
        fn height(item){calls+=1;let mut i=0;while i<40{i+=1;}item.height}
        fn opening(){
            let values=[];for i in 0..40 {values.append({id:i,height:40+i});}
            let items=list(||values,key:|item|item.id,virtual:true,row_height:height,build:|row|column(height:999));
            stage.replace(scroll(width:320,height:120){items});
            let row=await items.scroll_to(25,align:"start");
            if calls!=40 {throw "height callback replayed";}
            row.opacity=0.7;narrator("restored");
        }
    "#;
    let (mut session, mut vm) = models::prepared(code, true);
    let mut interrupted = false;
    for _ in 0..10000 {
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
            let p = vm.vm.program().package();
            let sizing = p.symbols[p.natives[t.native.index()].symbol.index()] == "reel::list"
                && t.continuation.is_some_and(|c| c.stage == 7)
                && t.input == NativeInput::CallbackReturned;
            // Save after the size callback's first return, before the next item.
            vm.dispatch(&mut session, t, "en", 1).unwrap();
            if sizing {
                interrupted = true;
                break;
            }
        }
    }
    assert!(interrupted);
    assert!(vm.vm.heap().reactive_evaluating());
    collect(&mut vm);
    let mut capture = vm.snapshot_begin(&session).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let mut saved =
        snapshot::SessionSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
    let mut restore = SessionVm::restore_begin(
        &mut saved,
        vm.vm.program().clone(),
        VmLimits::default(),
        SessionIdentity {
            session_id: SessionId(79),
            epoch: Epoch(21),
        },
        Default::default(),
        None,
        None,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let (mut session, mut vm) = restore.finish().unwrap();
    drive(&mut vm, &mut session);
    assert_eq!(view(&vm).total_height, (40..80).sum::<usize>() as f64);
    for _ in 0..4 {
        let y = offset(&vm);
        feedback(&mut vm, &mut session, y, 320., &[]);
    }
    vm.accept_presentation(&session, vm.presentation_request())
        .unwrap();
    drive(&mut vm, &mut session);
    assert!(vm.stage().nodes().any(|node| node.props.opacity == 0.7));
    assert_eq!(offset(&vm), (40..65).sum::<usize>() as f64);
}

#[test]
fn virtual_list_natural_height_keeps_anchor_on_measurement_resize_and_rejects_old_scroll_feedback()
{
    let (mut session, mut vm) = prepared(1000);
    for _ in 0..4 {
        feedback(&mut vm, &mut session, 500., 320., &[]);
    }
    let old_offset = offset(&vm);
    let anchor = view(&vm)
        .rows
        .iter()
        .rev()
        .find(|r| r.offset <= old_offset)
        .unwrap()
        .clone();
    let within = anchor.offset - old_offset;
    let mut measured = event(&vm, old_offset, 280., &[]);
    for item in std::sync::Arc::make_mut(&mut measured.measurements) {
        item.height += 35.;
    }
    vm.interactions(&mut session, &[NodeEvent::VirtualList(measured.clone())])
        .unwrap();
    drive(&mut vm, &mut session);
    let next = view(&vm)
        .rows
        .iter()
        .find(|r| r.node == anchor.node)
        .unwrap();
    assert_eq!(next.offset - offset(&vm), within);
    let scroll = ids(&vm).1;
    let current = offset(&vm);
    vm.interactions(
        &mut session,
        &[NodeEvent::Scroll(NodeScroll {
            input: true,
            node: scroll,
            requested: Vec2::new(0., current),
            offset: Vec2::new(0., 800.),
            viewport: Size::new(280., 120.),
            content: Size::new(280., view(&vm).total_height),
        })],
    )
    .unwrap();
    vm.interactions(&mut session, &[NodeEvent::VirtualList(measured)])
        .unwrap();
    drive(&mut vm, &mut session);
    assert_eq!(
        offset(&vm),
        800.,
        "old measurement cannot move a newer user scroll"
    );
}

#[test]
#[ignore = "release measurement: 100k exact-height index, first window and far jump"]
fn virtual_list_row_height_cost() {
    use std::time::Instant;
    for declared in [false, true] {
        let (mut session, mut vm) = gallery(100_000, declared, true);
        let generation = Instant::now();
        let mut index_start = None;
        loop {
            match vm
                .step(
                    &mut session,
                    SliceBudget {
                        instructions: 1000,
                        ..Default::default()
                    },
                    || 0,
                )
                .unwrap()
            {
                SessionStep::Vm(RunOutcome::HostCall(t)) => {
                    let p = vm.vm.program().package();
                    if index_start.is_none()
                        && p.symbols[p.natives[t.native.index()].symbol.index()] == "reel::list"
                    {
                        eprintln!(
                            "row-height exact={declared} rows=100000 generated_cpu={:?} charged={}",
                            generation.elapsed(),
                            vm.vm.heap().stats().charged_bytes
                        );
                        index_start = Some(Instant::now());
                    }
                    vm.dispatch(&mut session, t, "en", 32).unwrap();
                }
                SessionStep::Vm(RunOutcome::Returned(_)) => {
                    vm.discard_delivery(&mut session).unwrap()
                }
                SessionStep::Idle | SessionStep::Vm(RunOutcome::Waiting(_))
                    if !vm.has_runnable_tasks() =>
                {
                    vm.sync_stage(&mut session).unwrap();
                    break;
                }
                SessionStep::Vm(RunOutcome::Fault(id)) => {
                    panic!("{:?}", vm.vm.delivery_fault(id).unwrap())
                }
                _ => {}
            }
        }
        let index = index_start.unwrap().elapsed();
        let start = Instant::now();
        settle_layout(&mut vm, &mut session);
        let first = start.elapsed();
        let entries = vm.vm.stats().native_entries;
        let start = Instant::now();
        click(&mut vm, &mut session, "jump");
        settle_layout(&mut vm, &mut session);
        vm.accept_presentation(&session, vm.presentation_request())
            .unwrap();
        drive(&mut vm, &mut session);
        let jump = start.elapsed();
        assert!(scroll_to::status(&vm, "done"));
        click(&mut vm, &mut session, "check");
        assert!(view(&vm).rows.len() < 25);
        let charged = vm.vm.heap().stats().charged_bytes;
        collect(&mut vm);
        eprintln!(
            "row-height exact={declared} index_cpu={index:?} first_cpu={first:?} jump_cpu={jump:?} jump_native={} rows={} nodes={} charged_before_gc={charged} charged_after_gc={}",
            vm.vm.stats().native_entries - entries,
            view(&vm).rows.len(),
            vm.stage().nodes().count(),
            vm.vm.heap().stats().charged_bytes
        );
    }
}
