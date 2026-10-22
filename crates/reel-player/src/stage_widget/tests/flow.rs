use super::*;
use reel_ui::stage::{FlowOptions, GridOptions, Justify};

#[test]
#[ignore = "requires REEL_SESSION_FONT for actual paragraph measurement"]
fn flow_remaining_width_wraps_real_text_and_reuses_shaping() {
    let font = std::fs::read(std::env::var_os("REEL_SESSION_FONT").unwrap()).unwrap();
    let mut f = Fixture::new(Some(font.into()));
    let voice = cell(&mut f, Some(40.), 24., 0.);
    let speaker = cell(&mut f, Some(100.), 24., 0.);
    let body = f
        .stage
        .create(
            NodeKind::Text(
                "A paragraph uses the width left after the voice button and the speaker. "
                    .repeat(8)
                    .into(),
            ),
            NodeProps {
                flex: 1.,
                font_size: 20.,
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    let root = f
        .stage
        .create(
            NodeKind::Row(FlowOptions::default()),
            NodeProps {
                width: Some(600.),
                gap: 12.,
                ..Default::default()
            },
            vec![voice, speaker, body],
        )
        .unwrap();
    f.stage.replace(root).unwrap();
    let frame = f.record();
    f.accept(&frame);
    let wide = bounds(&f, &frame, body).0;
    assert_eq!(wide.width, 436.);
    assert!(wide.height > 24.);
    assert_eq!(bounds(&f, &frame, root).0.height, wide.height);
    let stable = f.layouts();
    for _ in 0..16 {
        let frame = f.record();
        f.accept(&frame);
    }
    assert_eq!(f.layouts(), stable);
    f.edit(root, |p| p.width = Some(300.));
    let frame = f.record();
    f.accept(&frame);
    let narrow = bounds(&f, &frame, body).0;
    assert_eq!(narrow.width, 136.);
    assert!(narrow.height > wide.height);
    assert_eq!(bounds(&f, &frame, root).0.height, narrow.height);
}

#[test]
#[ignore = "serial release measurement of actual grid construction, resize, idle and retirement"]
fn flow_grid_runtime_cost() {
    use stats_alloc::{INSTRUMENTED_SYSTEM, Region};
    use std::time::Instant;
    for count in [12, 120] {
        let mut f = Fixture::new(None);
        let cells = (0..count)
            .map(|_| cell(&mut f, Some(120.), 40., 0.))
            .collect::<Vec<_>>();
        let root = f
            .stage
            .create(
                NodeKind::Grid(GridOptions {
                    columns: 4,
                    column_width: Some(120.),
                    row_height: Some(40.),
                    ..Default::default()
                }),
                NodeProps {
                    width: Some(520.),
                    gap: 8.,
                    ..Default::default()
                },
                cells,
            )
            .unwrap();
        f.stage.replace(root).unwrap();
        let region = Region::new(&INSTRUMENTED_SYSTEM);
        let started = Instant::now();
        let frame = f.record();
        f.accept(&frame);
        drop(frame);
        let mount_us = started.elapsed().as_micros();
        let mounted = region.change();
        let layouts = f.layouts();
        let stable_region = Region::new(&INSTRUMENTED_SYSTEM);
        let started = Instant::now();
        for _ in 0..120 {
            let frame = f.record();
            f.accept(&frame);
        }
        let idle_us = started.elapsed().as_micros();
        let idle = stable_region.change();
        assert_eq!(
            layouts,
            f.layouts(),
            "stable presentation does not recalculate layout"
        );
        let changed_region = Region::new(&INSTRUMENTED_SYSTEM);
        let started = Instant::now();
        for index in 0..60 {
            f.edit(root, |p| p.width = Some(520. + f64::from(index % 2)));
            let frame = f.record();
            f.accept(&frame);
        }
        let resize_us = started.elapsed().as_micros();
        let resize = changed_region.change();
        let empty = f
            .stage
            .create(NodeKind::Stack, NodeProps::default(), vec![])
            .unwrap();
        f.stage.replace(empty).unwrap();
        let frame = f.record();
        f.accept(&frame);
        drop(frame);
        assert_eq!(f.layouts().len(), 1);
        let retired = region.change();
        eprintln!(
            "grid cells={count} node_props_bytes={} mounted_us={mount_us} mounted_allocations={} mounted_bytes={} idle_120_us={idle_us} idle_allocations={} idle_bytes={} resize_60_us={resize_us} resize_allocations={} resize_bytes={} retired_net_bytes={}",
            size_of::<NodeProps>(),
            mounted.allocations,
            mounted.bytes_allocated,
            idle.allocations,
            idle.bytes_allocated,
            resize.allocations,
            resize.bytes_allocated,
            retired.bytes_allocated as i128 - retired.bytes_deallocated as i128
        );
    }
}

fn cell(f: &mut Fixture, width: Option<f64>, height: f64, flex: f64) -> NodeId {
    f.stage
        .create(
            NodeKind::Button {
                label: None,
                listening: true,
            },
            NodeProps {
                width,
                height: Some(height),
                flex,
                decorated: false,
                ..Default::default()
            },
            vec![],
        )
        .unwrap()
}
fn bounds(f: &Fixture, frame: &TestFrame, id: NodeId) -> (Size, Point) {
    let widget = f.layouts()[&id].0;
    let geometry = frame.geometry().get(widget).unwrap();
    (geometry.size, geometry.window_transform * Point::ORIGIN)
}

#[test]
fn flow_stretch_preserves_overlay_measurement_and_cross_axis_fill() {
    use reel_ui::stage::Stretch;
    let mut f = Fixture::new(None);
    let ordinary = cell(&mut f, Some(80.), 20., 0.);
    let overlay = cell(&mut f, Some(999.), 60., 0.);
    let stretched = cell(&mut f, Some(40.), 999., 0.);
    f.edit(overlay, |p| {
        p.stretch = Stretch::X;
        p.max_width = Some(10.);
    });
    f.edit(stretched, |p| {
        p.stretch = Stretch::Y;
        p.max_height = Some(10.);
    });
    let root = f
        .stage
        .create(
            NodeKind::Row(FlowOptions::default()),
            NodeProps {
                width: Some(200.),
                gap: 10.,
                padding: 5.,
                ..Default::default()
            },
            vec![ordinary, overlay, stretched],
        )
        .unwrap();
    f.stage.replace(root).unwrap();
    let frame = f.record();
    f.accept(&frame);
    assert_eq!(bounds(&f, &frame, root).0, Size::new(200., 70.));
    assert_eq!(
        bounds(&f, &frame, overlay),
        (Size::new(190., 60.), Point::new(5., 5.))
    );
    assert_eq!(
        bounds(&f, &frame, stretched),
        (Size::new(40., 60.), Point::new(95., 5.))
    );
}

#[test]
fn flow_weights_share_remaining_space_and_idle_paint_reuses_layout() {
    let mut f = Fixture::new(None);
    let fixed = cell(&mut f, Some(80.), 20., 0.);
    let first = cell(&mut f, Some(999.), 20., 1.);
    let second = cell(&mut f, None, 40., 1.);
    let root = f
        .stage
        .create(
            NodeKind::Row(FlowOptions::default()),
            NodeProps {
                width: Some(320.),
                height: Some(80.),
                gap: 10.,
                ..Default::default()
            },
            vec![fixed, first, second],
        )
        .unwrap();
    f.stage.replace(root).unwrap();
    f.edit(first, |p| p.align_y = 0.625);
    let frame = f.record();
    f.accept(&frame);
    assert_eq!(
        bounds(&f, &frame, fixed),
        (Size::new(80., 20.), Point::ORIGIN)
    );
    assert_eq!(
        bounds(&f, &frame, first),
        (Size::new(110., 20.), Point::new(90., 38.))
    );
    assert_eq!(
        bounds(&f, &frame, second),
        (Size::new(110., 40.), Point::new(210., 0.))
    );
    let stable = f.layouts();
    for _ in 0..8 {
        let frame = f.record();
        f.accept(&frame);
    }
    assert_eq!(f.layouts(), stable);
    f.edit(first, |p| p.opacity = 0.4);
    let frame = f.record();
    f.accept(&frame);
    assert_eq!(f.layouts(), stable, "appearance does not run flex layout");
    f.edit(root, |p| p.width = Some(420.));
    let frame = f.record();
    f.accept(&frame);
    assert_eq!(bounds(&f, &frame, first).0.width, 160.);
    assert_eq!(bounds(&f, &frame, second).0.width, 160.);
}

#[test]
fn flow_wrap_and_justify_keep_natural_children_and_line_gap() {
    let mut f = Fixture::new(None);
    let children = (0..3)
        .map(|_| cell(&mut f, Some(50.), 20., 0.))
        .collect::<Vec<_>>();
    let root = f
        .stage
        .create(
            NodeKind::Row(FlowOptions {
                wrap: true,
                justify: Justify::SpaceBetween,
                line_gap: Some(7.),
            }),
            NodeProps {
                width: Some(120.),
                gap: 10.,
                ..Default::default()
            },
            children.clone(),
        )
        .unwrap();
    f.stage.replace(root).unwrap();
    let frame = f.record();
    f.accept(&frame);
    assert_eq!(bounds(&f, &frame, root).0, Size::new(120., 47.));
    assert_eq!(bounds(&f, &frame, children[1]).1, Point::new(70., 0.));
    assert_eq!(bounds(&f, &frame, children[2]).1, Point::new(0., 27.));
}

#[test]
fn flow_grid_tracks_resize_hide_and_reorder_existing_nodes() {
    let mut f = Fixture::new(None);
    let children = (0..6)
        .map(|_| cell(&mut f, None, 20., 0.))
        .collect::<Vec<_>>();
    let root = f
        .stage
        .create(
            NodeKind::Grid(GridOptions {
                columns: 3,
                ..Default::default()
            }),
            NodeProps {
                width: Some(270.),
                padding: 5.,
                gap: 10.,
                ..Default::default()
            },
            children.clone(),
        )
        .unwrap();
    f.stage.replace(root).unwrap();
    let frame = f.record();
    f.accept(&frame);
    assert_eq!(bounds(&f, &frame, root).0, Size::new(270., 60.));
    assert_eq!(
        bounds(&f, &frame, children[5]),
        (Size::new(80., 20.), Point::new(185., 35.))
    );
    let ids = f.layouts();
    f.edit(children[1], |p| p.visible = false);
    let frame = f.record();
    f.accept(&frame);
    assert_eq!(bounds(&f, &frame, children[2]).1, Point::new(95., 5.));
    assert!(
        frame
            .geometry()
            .get(ids[&children[1]].0)
            .unwrap()
            .is_stashed
    );
    let mut order = children.clone();
    let moved = order.pop().unwrap();
    order.insert(0, moved);
    f.stage.reconcile_children(root, order).unwrap();
    let frame = f.record();
    f.accept(&frame);
    assert_eq!(bounds(&f, &frame, children[5]).1, Point::new(5., 5.));
    assert_eq!(f.layouts()[&children[5]].0, ids[&children[5]].0);
    f.edit(root, |p| p.width = Some(330.));
    let frame = f.record();
    f.accept(&frame);
    assert_eq!(bounds(&f, &frame, children[5]).0.width, 100.);
}
