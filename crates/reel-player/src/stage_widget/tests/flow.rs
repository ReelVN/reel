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
    use std::time::Instant;
    for (count, automatic) in [(12, false), (12, true), (120, false), (120, true)] {
        let mut f = Fixture::new(None);
        let cells = (0..count)
            .map(|_| cell(&mut f, Some(120.), 40., 0.))
            .collect::<Vec<_>>();
        let kind = if automatic {
            NodeKind::Grid(GridOptions {
                columns: 4,
                column_width: Some(120.),
                row_height: Some(40.),
                ..Default::default()
            })
        } else {
            for (index, &cell) in cells.iter().enumerate() {
                f.edit(cell, |p| {
                    p.x = (index % 4) as f64 * 128.;
                    p.y = (index / 4) as f64 * 48.;
                });
            }
            NodeKind::Stack
        };
        let root = f
            .stage
            .create(
                kind,
                NodeProps {
                    width: Some(520.),
                    height: (!automatic).then_some(f64::from(count / 4) * 48. - 8.),
                    gap: 8.,
                    ..Default::default()
                },
                cells,
            )
            .unwrap();
        f.stage.replace(root).unwrap();
        let started = Instant::now();
        let mounted = allocation_counter::measure(|| {
            let frame = f.record();
            f.accept(&frame);
            drop(frame);
        });
        let mount_us = started.elapsed().as_micros();
        let layouts = f.layouts();
        let started = Instant::now();
        let idle = allocation_counter::measure(|| {
            for _ in 0..120 {
                let frame = f.record();
                f.accept(&frame);
            }
        });
        let idle_us = started.elapsed().as_micros();
        assert_eq!(
            layouts,
            f.layouts(),
            "stable presentation does not recalculate layout"
        );
        let started = Instant::now();
        let resize = allocation_counter::measure(|| {
            for index in 0..60 {
                f.edit(root, |p| p.width = Some(520. + f64::from(index % 2)));
                let frame = f.record();
                f.accept(&frame);
            }
        });
        let resize_us = started.elapsed().as_micros();
        let retirement = allocation_counter::measure(|| {
            let empty = f
                .stage
                .create(NodeKind::Stack, NodeProps::default(), vec![])
                .unwrap();
            f.stage.replace(empty).unwrap();
            let frame = f.record();
            f.accept(&frame);
            drop(frame);
            assert_eq!(f.layouts().len(), 1);
        });
        let retired = mounted.bytes_current
            + idle.bytes_current
            + resize.bytes_current
            + retirement.bytes_current;
        eprintln!(
            "grid automatic={automatic} cells={count} node_props_bytes={} mounted_us={mount_us} mounted_allocations={} mounted_bytes={} idle_120_us={idle_us} idle_allocations={} idle_bytes={} resize_60_us={resize_us} resize_allocations={} resize_bytes={} retired_net_bytes={}",
            size_of::<NodeProps>(),
            mounted.count_total,
            mounted.bytes_total,
            idle.count_total,
            idle.bytes_total,
            resize.count_total,
            resize.bytes_total,
            retired
        );
    }
}

fn cell(f: &mut Fixture, width: Option<f64>, height: f64, flex: f64) -> NodeId {
    f.stage
        .create(
            NodeKind::Button {
                label: None,
                listening: true,
                focus_on_pointer: true,
                focusable: true,
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
fn flow_intrinsic_size_and_grid_columns_follow_the_authored_container() {
    let mut f = Fixture::new(None);
    let a = cell(&mut f, Some(80.), 20., 0.);
    let b = cell(&mut f, Some(40.), 30., 0.);
    let row = f
        .stage
        .create(
            NodeKind::Row(FlowOptions::default()),
            NodeProps {
                gap: 10.,
                padding: 5.,
                ..Default::default()
            },
            vec![a, b],
        )
        .unwrap();
    f.stage.replace(row).unwrap();
    let frame = f.record();
    f.accept(&frame);
    assert_eq!(bounds(&f, &frame, row).0, Size::new(140., 40.));
    let wide = cell(&mut f, Some(300.), 20., 0.);
    let wrapper = f
        .stage
        .create(NodeKind::Stack, NodeProps::default(), vec![wide])
        .unwrap();
    let second = cell(&mut f, None, 20., 0.);
    let grid = f
        .stage
        .create(
            NodeKind::Grid(GridOptions {
                columns: 2,
                ..Default::default()
            }),
            NodeProps {
                width: Some(200.),
                ..Default::default()
            },
            vec![wrapper, second],
        )
        .unwrap();
    f.stage.replace(grid).unwrap();
    let frame = f.record();
    f.accept(&frame);
    assert_eq!(bounds(&f, &frame, wrapper).0.width, 100.);
    assert_eq!(bounds(&f, &frame, second).1.x, 100.);
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
    f.edit(stretched, |p| p.max_height = Some(10.));
    let frame = f.record();
    f.accept(&frame);
    assert_eq!(bounds(&f, &frame, stretched).0.height, 60.);
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
    let target = children[5];
    let before = bounds(&f, &frame, target).1;
    input_focus::press_pointer(&mut f, true, before.x + 20., before.y + 10.);
    let events = input_focus::press_pointer(&mut f, false, before.x + 20., before.y + 10.);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, NodeEvent::Click(click) if click.node == target))
    );
}
