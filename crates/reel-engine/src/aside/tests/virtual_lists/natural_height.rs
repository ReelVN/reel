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
fn gallery(n: usize, encoded: bool) -> (Session, SessionVm) {
    let code = format!(
        r#"
        let mut sources=0;let mut keys=0;let mut builds=0;
        let state=model({{items:[],status:"idle"}});
        fn opening() {{
            for i in 0..{n} {{ state.items.append(model({{id:i,height:if i==0 {{217}} else if i%10==0 {{277}} else {{187}}}})); }}
            let items=list(||{{sources+=1;state.items}},key:|item|{{keys+=1;item.id}},virtual:true,
                build:|row|{{builds+=1;column(height:derive(||row.value.height))}});
            stage.replace(column {{
                scroll(width:1313,height:920){{items}};
                text(value:derive(||state.status));
                button("jump",on_click:||{{state.status="waiting";await items.scroll_to({n}-20,align:"center");state.status="done";}});
                button("grow",on_click:||{{state.items[0].height+=80;}});
                button("replace",on_click:||{{state.items[1]=model({{id:1,height:300}});}});
                button("invalid",on_click:||{{state.items[1].height=0;}});
                button("reverse",on_click:||state.items.reverse());
                button("check",on_click:||{{if sources!=1||keys!={n} {{throw "scroll recomputed size source";}}}});
            }});
            narrator("ready");
        }}
    "#,
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
#[ignore = "release measurement: 100k natural-height index, first window and far jump"]
fn virtual_list_natural_height_cost() {
    use std::time::Instant;
    {
        let (mut session, mut vm) = gallery(100_000, true);
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
                            "natural-height rows=100000 generated_cpu={:?} charged={}",
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
            "natural-height index_cpu={index:?} first_cpu={first:?} jump_cpu={jump:?} jump_native={} rows={} nodes={} charged_before_gc={charged} charged_after_gc={}",
            vm.vm.stats().native_entries - entries,
            view(&vm).rows.len(),
            vm.stage().nodes().count(),
            vm.vm.heap().stats().charged_bytes
        );
    }
}

#[test]
fn virtual_list_natural_refills_before_unmeasured_rows_enter_and_uses_all_samples() {
    for encoded in [false, true] {
        let (mut session, mut vm) = gallery(80, encoded);
        drive(&mut vm, &mut session);
        let mut samples = std::collections::BTreeMap::new();
        for _ in 0..4 {
            for row in view(&vm).rows.iter() {
                samples.insert(
                    row.index,
                    vm.stage().node(row.node).unwrap().props.height.unwrap(),
                );
            }
            frame(&mut vm, &mut session, 0., 1313.);
        }
        let initial = view(&vm).total_height;
        assert!(
            (initial - total(80)).abs() < 500.,
            "first-window samples replace the anomalous seed: {initial}"
        );
        let expected = samples.values().sum::<f64>() / samples.len() as f64 * 80.;
        assert!((view(&vm).total_height - expected).abs() < 1e-6);
        let entries = vm.vm.stats().native_entries;
        for step in 0..90 {
            let requested = (offset(&vm) + if step < 45 { 80. } else { -80. }).max(0.);
            for row in view(&vm).rows.iter() {
                samples.insert(
                    row.index,
                    vm.stage().node(row.node).unwrap().props.height.unwrap(),
                );
            }
            frame(&mut vm, &mut session, requested, 1313.);
            for row in view(&vm).rows.iter() {
                let height = vm.stage().node(row.node).unwrap().props.height.unwrap();
                if row.offset < offset(&vm) + 920. && row.offset + height > offset(&vm) {
                    assert!(
                        samples.contains_key(&row.index),
                        "unmeasured row {} entered viewport at step {step}",
                        row.index
                    );
                }
            }
            assert!(view(&vm).rows.len() < 22);
        }
        eprintln!(
            "natural buffer 90 small scrolls: {} native entries, {} measured rows",
            vm.vm.stats().native_entries - entries,
            samples.len()
        );
        click(&mut vm, &mut session, "check");
        // Replace an existing sample's height, then invalidate all old-width
        // samples. Each new mean is computed from exactly the measured rows.
        frame(&mut vm, &mut session, 0., 1313.);
        settle_layout(&mut vm, &mut session);
        click(&mut vm, &mut session, "grow");
        let mut resized = event(&vm, offset(&vm), 1000., &[]);
        resized.viewport.height = 920.;
        resized.visible_end = resized.visible_start + 920.;
        let mean = resized.measurements.iter().map(|m| m.height).sum::<f64>()
            / resized.measurements.len() as f64;
        vm.interactions(&mut session, &[NodeEvent::VirtualList(resized)])
            .unwrap();
        drive(&mut vm, &mut session);
        assert!((view(&vm).total_height - 80. * mean).abs() < 1e-6);
    }
}
