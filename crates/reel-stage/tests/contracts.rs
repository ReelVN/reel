mod support;

#[test]
fn production_input_measurements() {
    support::presentation::measure();
    support::presentation::scaled_measure();
}

use masonry_core::{
    app::{GeometryError, GeometryLimits, InputGeometrySource},
    core::*,
    kurbo::{Affine, Point, Rect, Size},
};
use reel_core::identity::{FrameId, SurfaceEpoch};
use reel_stage::*;
use std::{cell::RefCell, rc::Rc, sync::Arc};
use support::fixture::*;
fn bridge_limits() -> BridgeLimits {
    BridgeLimits {
        geometry: support::limits(),
        max_candidates: 4,
        max_bridge_bytes: 64 * 1024,
        max_snapshot_bytes: 20 * 1024 * 1024,
    }
}
fn accept(c: &RecordedCandidate) -> PresentationNotice {
    notice(c, PresentationOutcome::Accepted(c.key().frame_id()))
}
fn notice(c: &RecordedCandidate, outcome: PresentationOutcome) -> PresentationNotice {
    PresentationNotice {
        key: c.key(),
        surface_epoch: c.key().surface_epoch(),
        outcome,
    }
}

#[test]
fn real_record_gate_first_frame_failures_identity_order_clear_and_epoch() {
    let f = SliderFixture::new();
    let slider = f.slider;
    let group = f.group;
    let trace = f.trace.clone();
    let mut bridge = PresentationBridge::new(f.root, SurfaceEpoch(4), bridge_limits()).unwrap();
    bridge.handle_pointer_event(mouse("down", Point::new(35., 20.)));
    assert_eq!(bridge.root().pointer_capture_target(), None);
    let first = bridge.record(FrameId(1)).unwrap();
    assert!(!first.scene().is_empty());
    first.scene().validate().unwrap();
    assert_eq!(
        bridge.resolve(notice(&first, PresentationOutcome::Accepted(FrameId(99)))),
        NoticeResult::StaleOrForeign
    );
    let mut wrong_epoch = accept(&first);
    wrong_epoch.surface_epoch = SurfaceEpoch(99);
    assert_eq!(bridge.resolve(wrong_epoch), NoticeResult::StaleOrForeign);
    assert_eq!(bridge.resolve(accept(&first)), NoticeResult::Accepted);
    assert_eq!(bridge.resolve(accept(&first)), NoticeResult::StaleOrForeign);
    bridge.handle_pointer_event(mouse("down", Point::new(35., 20.)));
    assert_eq!(bridge.root().pointer_capture_target(), Some(slider));
    bridge.edit_widget(group, |mut w| {
        StageGroup::geometry(
            &mut w.downcast(),
            0,
            Point::new(160., 80.),
            Size::new(200., 40.),
        )
    });
    for (id, outcome) in [
        (2, PresentationOutcome::Skipped),
        (3, PresentationOutcome::SurfaceUnavailable),
        (4, PresentationOutcome::Error),
    ] {
        let c = bridge.record(FrameId(id)).unwrap();
        assert_eq!(
            bridge.resolve(notice(&c, outcome)),
            NoticeResult::NotPresented
        );
        bridge.handle_pointer_event(mouse("move", Point::new(35., 20.)));
        assert_eq!(
            trace
                .borrow()
                .iter()
                .rev()
                .find(|o| o.widget == slider)
                .unwrap()
                .size,
            Size::new(100., 30.)
        );
    }
    let second = bridge.record(FrameId(5)).unwrap();
    let third = bridge.record(FrameId(6)).unwrap();
    assert_eq!(bridge.resolve(accept(&third)), NoticeResult::Accepted);
    assert_eq!(
        bridge.root().pointer_capture_target(),
        Some(slider),
        "surviving capture continues across geometry changes"
    );
    assert_eq!(
        bridge.resolve(accept(&second)),
        NoticeResult::StaleOrForeign
    );
    let other = SliderFixture::new();
    let mut other = PresentationBridge::new(other.root, SurfaceEpoch(4), bridge_limits()).unwrap();
    let foreign = other.record(FrameId(7)).unwrap();
    let own = bridge.record(FrameId(7)).unwrap();
    assert_eq!(
        bridge.resolve(accept(&foreign)),
        NoticeResult::StaleOrForeign
    );
    assert_eq!(bridge.pending_count(), 1);
    trace.borrow_mut().clear();
    assert!(bridge.replace_surface(SurfaceEpoch(5)));
    assert_eq!(
        trace
            .borrow()
            .iter()
            .filter(|o| o.widget == slider && o.kind == "cancel")
            .count(),
        1
    );
    assert_eq!(bridge.root().pointer_capture_target(), None);
    assert_eq!(bridge.resolve(accept(&own)), NoticeResult::StaleOrForeign);
    assert!(!bridge.replace_surface(SurfaceEpoch(5)));
    bridge.handle_pointer_event(mouse("down", Point::new(35., 20.)));
    assert_eq!(bridge.root().pointer_capture_target(), None);
    let new = bridge.record(FrameId(8)).unwrap();
    assert_eq!(new.key().surface_epoch(), SurfaceEpoch(5));
    assert_eq!(bridge.resolve(accept(&new)), NoticeResult::Accepted);
    bridge.handle_pointer_event(mouse("down", Point::new(185., 90.)));
    assert_eq!(bridge.root().pointer_capture_target(), Some(slider));
    bridge.clear();
    assert_eq!(bridge.root().pointer_capture_target(), None);
}

#[test]
fn missing_children_and_frozen_widgetref_order_include_stashed() {
    let trace = Rc::new(RefCell::new(vec![]));
    let samples = Rc::new(RefCell::new(vec![]));
    let mut inner = StageGroup::new(trace.clone(), 1., Rect::new(0., 0., 150., 100.));
    let a = inner.add(
        image([255, 0, 0, 255]).prepare(),
        Point::ZERO,
        Size::new(10., 10.),
    );
    let b = inner.add(
        image([0, 255, 0, 255]).prepare(),
        Point::new(20., 0.),
        Size::new(10., 10.),
    );
    let stashed = inner.add(
        image([0, 0, 255, 255]).prepare(),
        Point::new(40., 0.),
        Size::new(10., 10.),
    );
    let inner = inner.prepare();
    let inner_id = inner.id();
    let mut parent = StageGroup::new(trace.clone(), 1., Rect::new(0., 0., 400., 300.));
    parent.accepts_pointer = true;
    parent.child_queries = Some(samples.clone());
    parent.query_ids = vec![inner_id];
    parent.add(inner, Point::ZERO, Size::new(150., 100.));
    let parent = parent.prepare();
    let parent_id = parent.id();
    let (mut root, _) = root(parent);
    root.edit_widget(inner_id, |mut w| {
        let mut g = w.downcast::<StageGroup>();
        g.ctx.set_stashed(&mut g.widget.children[2].0, true);
    });
    let (_, view) = root.record_presented_geometry(support::limits()).unwrap();
    assert!(
        view.get(stashed).unwrap().is_stashed,
        "complete registration includes hidden node"
    );
    root.set_input_geometry_source(InputGeometrySource::Presented(Some(Arc::new(view))));
    let fresh = image([255, 255, 0, 255]).prepare();
    let fresh_id = fresh.id();
    root.edit_widget(inner_id, |mut w| {
        let mut g = w.downcast::<StageGroup>();
        StageGroup::remove(&mut g, 1);
        g.widget.children.swap(0, 1);
        g.widget.add(fresh, Point::ZERO, Size::new(10., 10.));
        g.ctx.children_changed();
    });
    let fresh_group = StageGroup::new(trace.clone(), 1., Rect::new(0., 0., 10., 10.)).prepare();
    let fresh_group_id = fresh_group.id();
    root.edit_widget(parent_id, |mut w| {
        let mut g = w.downcast::<StageGroup>();
        g.widget.query_ids.push(fresh_group_id);
        g.widget
            .add(fresh_group, Point::new(200., 0.), Size::new(10., 10.));
        g.ctx.children_changed();
    });
    root.handle_pointer_event(mouse("move", Point::new(300., 200.)));
    root.handle_pointer_event(mouse("down", Point::new(300., 200.)));
    let s = samples.borrow();
    let frozen = s
        .iter()
        .rev()
        .find(|q| q.phase == "query" && q.id == inner_id)
        .unwrap();
    assert_eq!(
        frozen.children,
        vec![a, stashed],
        "old order; destroyed child filtered; fresh node absent"
    );
    assert!(!frozen.children.contains(&b));
    assert!(!frozen.children.contains(&fresh_id));
    assert!(
        s.iter()
            .any(|q| q.phase == "query" && q.id == fresh_group_id && !q.present)
    );
    assert!(
        s.iter()
            .any(|q| q.phase == "raw" && q.id == fresh_group_id && !q.present)
    );
    drop(s);
    root.set_input_geometry_source(InputGeometrySource::Live);
    root.handle_pointer_event(mouse("move", Point::new(300., 200.)));
    let s = samples.borrow();
    assert_eq!(
        s.iter()
            .rev()
            .find(|q| q.phase == "query" && q.id == inner_id)
            .unwrap()
            .children,
        vec![stashed, a, fresh_id]
    );
}

#[test]
fn snapshot_node_path_capacity_and_lease_recovery() {
    use std::time::Instant;
    let f = SliderFixture::new();
    let other = SliderFixture::new();
    assert!(matches!(
        PresentationBridge::new(
            other.root,
            SurfaceEpoch(1),
            BridgeLimits {
                max_bridge_bytes: 0,
                ..bridge_limits()
            }
        ),
        Err(RecordError::BridgeBudget)
    ));
    let mut limits = support::limits();
    assert!(matches!(
        f.root.capture_presented_geometry(GeometryLimits {
            max_nodes: 2,
            ..limits
        }),
        Err(GeometryError::Nodes)
    ));
    assert!(matches!(
        f.root.capture_presented_geometry(GeometryLimits {
            max_path: 2,
            ..limits
        }),
        Err(GeometryError::Path)
    ));
    assert!(matches!(
        f.root.capture_presented_geometry(GeometryLimits {
            max_bytes: 1,
            ..limits
        }),
        Err(GeometryError::Bytes)
    ));
    let v = f.root.capture_presented_geometry(limits).unwrap();
    let mut path = Vec::with_capacity(2);
    assert_eq!(
        v.path_into(f.slider, &mut path, 2),
        Err(GeometryError::Path)
    );
    assert!(path.is_empty());
    path = Vec::with_capacity(3);
    v.path_into(f.slider, &mut path, 3).unwrap();
    assert_eq!(path.len(), 3);
    limits.max_bytes = v.retained_bytes();
    let bytes = limits.max_bytes as u64 + 256;
    let mut bridge = PresentationBridge::new(
        f.root,
        SurfaceEpoch(1),
        BridgeLimits {
            geometry: limits,
            max_candidates: 1,
            max_bridge_bytes: 64 * 1024,
            max_snapshot_bytes: bytes,
        },
    )
    .unwrap();
    let c = bridge.record(FrameId(1)).unwrap();
    let start = Instant::now();
    assert!(matches!(
        bridge.record(FrameId(2)),
        Err(RecordError::CandidateCapacity)
    ));
    let full_ns = start.elapsed().as_nanos();
    assert_eq!(bridge.resolve(accept(&c)), NoticeResult::Accepted);
    let lease = match bridge.root().input_geometry_source() {
        InputGeometrySource::Presented(Some(v)) => v.clone(),
        _ => panic!("accepted view"),
    };
    drop(c);
    bridge.clear();
    assert_eq!(
        bridge.snapshot_usage().tickets,
        1,
        "raw Arc lease keeps budget charged"
    );
    let start = Instant::now();
    assert!(matches!(
        bridge.record(FrameId(2)),
        Err(RecordError::SnapshotBudget)
    ));
    let exhausted_ns = start.elapsed().as_nanos();
    drop(lease);
    assert_eq!(bridge.snapshot_usage().bytes, 0);
    let start = Instant::now();
    let c = bridge.record(FrameId(2)).unwrap();
    let recovery_ns = start.elapsed().as_nanos();
    bridge.resolve(notice(&c, PresentationOutcome::Skipped));
    drop(c);
    assert_eq!(bridge.snapshot_usage().bytes, 0);
    println!(
        "budget actual_nodes=3 payload_bytes={} snapshot_limit_bytes={bytes}: full_candidate_reject={full_ns}ns retained_lease_reject={exhausted_ns}ns release_then_record={recovery_ns}ns; final_usage={:?}",
        v.retained_bytes(),
        bridge.snapshot_usage()
    );
}

#[test]
fn view_change_notifies_only_differences_and_uses_old_exit_geometry() {
    let queries = Rc::new(RefCell::new(vec![]));
    let trace = Rc::new(RefCell::new(vec![]));
    let mut group = StageGroup::new(trace.clone(), 1., Rect::new(0., 0., 400., 300.));
    group.queries = Some(queries.clone());
    let make = |q| {
        Observed {
            inner: masonry::widgets::Slider::new(0., 1., 0.),
            trace: trace.clone(),
            isolated: false,
            queries: Some(q),
        }
        .prepare()
    };
    let a = group.add(
        make(queries.clone()),
        Point::new(10., 10.),
        Size::new(100., 30.),
    );
    let b = group.add(
        make(queries.clone()),
        Point::new(200., 10.),
        Size::new(100., 30.),
    );
    let group = group.prepare();
    let group_id = group.id();
    let (root, _) = root(group);
    let mut bridge = PresentationBridge::new(root, SurfaceEpoch(1), bridge_limits()).unwrap();
    let first = bridge.record(FrameId(1)).unwrap();
    bridge.resolve(accept(&first));
    bridge.handle_pointer_event(mouse("move", Point::new(35., 20.)));
    bridge.edit_widget(group_id, |mut w| {
        let mut g = w.downcast::<StageGroup>();
        StageGroup::geometry(&mut g, 0, Point::new(200., 10.), Size::new(100., 30.));
        StageGroup::geometry(&mut g, 1, Point::new(10., 10.), Size::new(100., 30.));
    });
    let second = bridge.record(FrameId(2)).unwrap();
    queries.borrow_mut().clear();
    bridge.resolve(accept(&second));
    let q = queries.borrow();
    let exit = q
        .iter()
        .find(|q| q.widget == a && q.kind == "hover_exit")
        .unwrap();
    let enter = q
        .iter()
        .find(|q| q.widget == b && q.kind == "hover_enter")
        .unwrap();
    assert_eq!(exit.transform, Affine::translate((10., 10.)));
    assert_eq!(enter.transform, Affine::translate((10., 10.)));
    assert!(
        !q.iter().any(|q| q.widget == group_id
            && (q.kind == "ancestor_hover_exit" || q.kind == "ancestor_hover_enter")),
        "shared parent stays hovered"
    );
    drop(q);
    queries.borrow_mut().clear();
    let third = bridge.record(FrameId(3)).unwrap();
    bridge.resolve(accept(&third));
    assert!(
        !queries
            .borrow()
            .iter()
            .any(|q| q.kind.contains("enter") || q.kind.contains("exit")),
        "identical paths have no notifications"
    );
    bridge.clear();
    assert!(
        queries
            .borrow()
            .iter()
            .any(|q| q.widget == b && q.kind == "hover_exit")
    );
}

#[test]
fn raw_mut_missing_and_animation_live_with_complete_geometry_getters() {
    use masonry_core::{
        layout::Length,
        properties::{BorderWidth, Padding},
    };
    use support::raw_probe::*;
    let samples = Rc::new(RefCell::new(vec![]));
    let trace = Rc::new(RefCell::new(vec![]));
    let probe = RawProbe::new(trace.clone(), samples.clone())
        .prepare()
        .with_props(
            PropertySet::new()
                .with(Padding::all(Length::px(5.)))
                .with(BorderWidth {
                    width: Length::px(2.),
                }),
        );
    let id = probe.id();
    let (mut root, _) = root(probe);
    let own_sample = root
        .get_widget(id)
        .unwrap()
        .downcast::<RawProbe>()
        .unwrap()
        .own_sample
        .clone();
    let own = root.get_widget(id).unwrap();
    let q = own.ctx();
    let expected = (
        q.content_box(),
        q.border_box(),
        q.paint_box(),
        q.bounding_box(),
        q.first_baseline(),
        q.last_baseline(),
        q.clip_path(),
        q.border_box_translation(),
        q.window_transform(),
        q.scale_factor(),
    );
    assert_eq!(expected.4, 7.);
    assert_eq!(expected.5, 11.);
    let (_, view) = root.record_presented_geometry(support::limits()).unwrap();
    let record = view.get(id).unwrap();
    assert_eq!(record.first_baseline, 7.);
    assert_eq!(record.last_baseline, 11.);
    assert_eq!(record.size.to_rect() - expected.7, expected.1);
    assert_eq!(
        record.clip_path.map(|r| r - expected.7),
        expected.6,
        "clip converts to content coordinates once"
    );
    assert_eq!(
        record.paint_box_insets,
        masonry_core::kurbo::Insets::uniform(10.)
    );
    root.set_input_geometry_source(InputGeometrySource::Presented(Some(Arc::new(view))));
    root.handle_pointer_event(mouse("down", Point::new(100., 100.)));
    assert!(samples.borrow().iter().all(|(_, v)| v.is_some()));
    root.edit_widget(id, |mut w| {
        let mut p = w.downcast::<RawProbe>();
        let fresh = StageGroup::new(trace.clone(), 1., Rect::new(0., 0., 20., 20.))
            .prepare()
            .with_transform(Affine::translate((7., 3.)))
            .to_pod();
        let old = std::mem::replace(&mut p.widget.child, fresh);
        p.ctx.remove_child(old);
        p.ctx.children_changed();
        p.ctx.request_anim_frame();
    });
    root.edit_widget(id, |mut w| {
        w.insert_prop(Padding::all(Length::px(9.)));
    });
    root.handle_window_event(WindowEvent::Rescale(2.));
    samples.borrow_mut().clear();
    root.handle_pointer_event(mouse("down", Point::new(100., 100.)));
    assert_eq!(
        *own_sample.borrow(),
        Some(expected),
        "all own geometric getters use the validated old record"
    );
    assert_eq!(
        samples.borrow().as_slice(),
        &[("pointer_mut", None), ("pointer_shared", None)]
    );
    root.edit_widget(id, |mut w| {
        let mut p = w.downcast::<RawProbe>();
        let (_, raw) = p.ctx.get_raw_mut(&mut p.widget.child).unwrap();
        assert_eq!(raw.transform(), Affine::translate((7., 3.)));
    });
    root.handle_window_event(WindowEvent::AnimFrame(std::time::Duration::from_nanos(1)));
    let s = samples.borrow();
    assert_eq!(
        s.iter().find(|(k, _)| *k == "animation_mut").unwrap().1,
        Some(Affine::translate((7., 3.)))
    );
}

#[test]
fn live_stashing_and_missing_new_view_cancel_alive_capture_once() {
    for missing_view in [false, true] {
        let mut f = SliderFixture::new();
        let trace = f.trace.clone();
        let first = f
            .root
            .capture_presented_geometry(support::limits())
            .unwrap();
        f.root
            .set_input_geometry_source(InputGeometrySource::Presented(Some(Arc::new(first))));
        f.root
            .handle_pointer_event(mouse("down", Point::new(35., 20.)));
        trace.borrow_mut().clear();
        if missing_view {
            let other = SliderFixture::new();
            let view = other
                .root
                .capture_presented_geometry(support::limits())
                .unwrap();
            f.root
                .set_input_geometry_source(InputGeometrySource::Presented(Some(Arc::new(view))));
        } else {
            f.root.edit_widget(f.group, |mut w| {
                let mut g = w.downcast::<StageGroup>();
                g.ctx.set_stashed(&mut g.widget.children[0].0, true);
            });
        }
        assert_eq!(f.root.pointer_capture_target(), None);
        let t = trace.borrow();
        let terminal = t
            .iter()
            .filter(|s| s.widget == f.slider && s.kind == "cancel")
            .collect::<Vec<_>>();
        assert_eq!(terminal.len(), 1);
        assert_eq!(terminal[0].size, Size::new(100., 30.));
        drop(t);
        f.root
            .handle_pointer_event(mouse("down", Point::new(35., 20.)));
        assert_eq!(f.root.pointer_capture_target(), None);
    }
}

#[test]
fn actual_bridge_record_and_input_measurements() {
    use std::time::Instant;
    for count in [32, 256, 1024] {
        let trace = Rc::new(RefCell::new(vec![]));
        let mut group = StageGroup::new(trace.clone(), 1., Rect::new(0., 0., 1920., 1080.));
        for i in 0..count {
            group.add(
                Observed {
                    inner: masonry::widgets::Slider::new(0., 1., 0.),
                    trace: trace.clone(),
                    isolated: false,
                    queries: None,
                }
                .prepare(),
                Point::new((i % 32) as f64 * 60., (i / 32) as f64 * 30.),
                Size::new(60., 30.),
            );
        }
        let (mut root, signals) = root(group.prepare());
        root.handle_window_event(WindowEvent::Resize(masonry_core::dpi::PhysicalSize::new(
            1920, 1080,
        )));
        let nodes = count + 2;
        let max_bytes = masonry_core::app::PresentedGeometryView::bytes_for_nodes(nodes).unwrap();
        let limits = BridgeLimits {
            geometry: GeometryLimits {
                max_nodes: nodes,
                max_bytes: max_bytes + 2 * nodes * size_of::<WidgetId>(),
                max_path: 128,
            },
            max_candidates: 2,
            max_bridge_bytes: 64 * 1024,
            max_snapshot_bytes: (max_bytes * 3 + 1024) as u64,
        };
        let scratch = root
            .capture_presented_geometry(limits.geometry)
            .unwrap()
            .scratch_peak_bytes();
        let mut bridge = PresentationBridge::new(root, SurfaceEpoch(1), limits).unwrap();
        let mut records = vec![];
        let mut exports = vec![];
        let mut inputs = vec![];
        for frame in 1..=110 {
            let start = Instant::now();
            let candidate = bridge.record(FrameId(frame)).unwrap();
            let elapsed = start.elapsed().as_nanos();
            if frame > 10 {
                records.push(elapsed);
            }
            assert_eq!(candidate.geometry().len(), nodes);
            bridge.resolve(accept(&candidate));
            let start = Instant::now();
            std::hint::black_box(
                bridge
                    .root()
                    .capture_presented_geometry(limits.geometry)
                    .unwrap(),
            );
            if frame > 10 {
                exports.push(start.elapsed().as_nanos());
            }
        }
        let p = Point::new(
            ((count - 1) % 32) as f64 * 60. + 15.,
            ((count - 1) / 32) as f64 * 30. + 10.,
        );
        bridge.handle_pointer_event(mouse("down", p));
        for i in 0..1100 {
            trace.borrow_mut().clear();
            signals.borrow_mut().clear();
            let start = Instant::now();
            bridge.handle_pointer_event(mouse("move", p + ((i % 20) as f64, 0.)));
            if i >= 100 {
                inputs.push(start.elapsed().as_nanos());
            }
        }
        records.sort_unstable();
        exports.sort_unstable();
        inputs.sort_unstable();
        println!(
            "production bridge 1080p controls={count} actual_nodes={nodes} geometry_bytes={max_bytes} export_scratch_peak_bytes={scratch} fixed_route_registry_bytes={} snapshot_usage={:?}; record+replay p95={}ns max={}ns export p95={}ns max={}ns pointer p95={}ns max={}ns",
            bridge.fixed_storage_bytes(),
            bridge.snapshot_usage(),
            records[94],
            records.last().unwrap(),
            exports[94],
            exports.last().unwrap(),
            inputs[949],
            inputs.last().unwrap()
        );
    }
}
