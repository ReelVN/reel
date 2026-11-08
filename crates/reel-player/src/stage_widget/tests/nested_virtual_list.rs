use super::*;
use reel_ui::stage::{FlowOptions, InputBindings, VirtualListView, VirtualRow};

fn fixture(total: usize, clip: bool, scale: f64) -> (Fixture, NodeId, NodeId, NodeId) {
    fixture_nested(total, clip, scale, false)
}
fn fixture_nested(
    total: usize,
    clip: bool,
    scale: f64,
    nested: bool,
) -> (Fixture, NodeId, NodeId, NodeId) {
    let mut f = Fixture::new(None);
    let mut stage = Stage::default();
    let row = stage
        .create(
            NodeKind::Stack,
            NodeProps {
                height: Some(40.),
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    let list = stage
        .create(
            NodeKind::VirtualList(VirtualListView {
                revision: 1,
                total_height: total as f64 * 42.,
                total_rows: total,
                rows: Arc::from([VirtualRow {
                    node: row,
                    index: 0,
                    offset: 0.,
                }]),
                navigation: None,
            }),
            NodeProps {
                padding: 4.,
                gap: 2.,
                ..Default::default()
            },
            vec![row],
        )
        .unwrap();
    let input = stage
        .create(
            NodeKind::Input {
                bindings: InputBindings::new(vec![]).unwrap(),
                listening: false,
                keyboard: false,
                pointer: false,
                wheel: false,
            },
            NodeProps::default(),
            vec![list],
        )
        .unwrap();
    let column = stage
        .create(
            NodeKind::Column(FlowOptions::default()),
            NodeProps::default(),
            vec![input],
        )
        .unwrap();
    let wrapper = stage
        .create(
            NodeKind::Stack,
            NodeProps {
                x: 11.,
                y: 30.,
                padding: 10.,
                clip,
                scale_y: scale,
                height: clip.then_some(80.),
                ..Default::default()
            },
            vec![column],
        )
        .unwrap();
    let mut content = vec![wrapper];
    if clip {
        content.push(
            stage
                .create(
                    NodeKind::Stack,
                    NodeProps {
                        height: Some(1_000.),
                        ..Default::default()
                    },
                    vec![],
                )
                .unwrap(),
        );
    }
    let scroll = stage
        .create(
            NodeKind::Scroll {
                axis: ScrollAxis::Vertical,
                overscroll: Default::default(),
                follow_end: false,
                align: reel_ui::widgets::ScrollAlign::Start,
                offset: Vec2::ZERO,
            },
            NodeProps {
                width: Some(200.),
                height: Some(100.),
                ..Default::default()
            },
            content,
        )
        .unwrap();
    let root = if nested {
        stage
            .create(
                NodeKind::Scroll {
                    axis: ScrollAxis::Vertical,
                    overscroll: Default::default(),
                    follow_end: false,
                    align: reel_ui::widgets::ScrollAlign::Start,
                    offset: Vec2::new(0., 25.),
                },
                NodeProps {
                    width: Some(200.),
                    height: Some(60.),
                    ..Default::default()
                },
                vec![scroll],
            )
            .unwrap()
    } else {
        scroll
    };
    stage.replace(root).unwrap();
    f.stage = stage;
    f.root = root;
    (f, list, row, scroll)
}

fn feedback(f: &mut Fixture, list: NodeId) -> NodeVirtualList {
    let frame = f.record();
    f.accept(&frame);
    f.ui.take_events()
        .into_iter()
        .find_map(|event| match event {
            NodeEvent::VirtualList(event) if event.node == list => Some(event),
            _ => None,
        })
        .expect("nested list publishes viewport")
}

fn offset(f: &mut Fixture, scroll: NodeId, y: f64) {
    let node = f.stage.node(scroll).unwrap();
    let props = node.props.clone();
    let mut kind = node.kind.clone();
    let NodeKind::Scroll { offset, .. } = &mut kind else {
        panic!()
    };
    offset.y = y;
    f.stage.update(scroll, kind, props).unwrap();
}

#[test]
fn nested_containers_report_list_local_range_and_reuse_layout() {
    let mut counts = Vec::new();
    for total in [1_000, 10_000, 100_000] {
        let (mut f, list, row, _) = fixture(total, false, 1.);
        let first = feedback(&mut f, list);
        assert_eq!(first.scroll, f.root);
        assert_eq!((first.visible_start, first.visible_end), (-44., 56.));
        assert_eq!(first.scroll_scale, 1.);
        assert_eq!(
            first.measurements.as_ref(),
            &[VirtualRowMeasurement {
                node: row,
                height: 40.
            }]
        );
        let before = f.layouts();
        counts.push(before.len());
        let scroll = f.root;
        offset(&mut f, scroll, 20_000.);
        let moved = feedback(&mut f, list);
        assert_eq!((moved.visible_start, moved.visible_end), (19_956., 20_056.));
        assert_eq!(
            f.layouts(),
            before,
            "a distant nested scroll reuses mounted row layout"
        );
        let work = f.paint_work();
        let idle = f.record();
        f.accept(&idle);
        assert_eq!(
            f.paint_work(),
            work,
            "an unchanged viewport does not traverse compose callbacks"
        );
        assert!(
            !f.ui
                .take_events()
                .iter()
                .any(|e| matches!(e, NodeEvent::VirtualList(_)))
        );
    }
    assert!(counts.iter().all(|count| *count == counts[0]));
}

#[test]
fn nested_clip_and_transforms_share_the_logical_viewport() {
    for scale in [0.5, 2.] {
        let (mut f, list, _, _) = fixture(1_000, false, scale);
        let first = feedback(&mut f, list);
        assert_eq!(first.scroll_scale, 1. / scale);
        assert_eq!(first.visible_start, -30. / scale - 14.);
        assert_eq!(first.visible_end - first.visible_start, 100. / scale);
    }
    let (mut f, list, _, _) = fixture(1_000, true, 1.);
    let first = feedback(&mut f, list);
    assert_eq!((first.visible_start, first.visible_end), (-14., 56.));
    let scroll = f.root;
    offset(&mut f, scroll, 200.);
    let outside = feedback(&mut f, list);
    assert_eq!(
        outside.visible_start, outside.visible_end,
        "a clipped-out list requests an empty window"
    );
    assert!(
        f.ui.virtual_feedback_visible(&mut f.bridge, f.widget)
            .unwrap(),
        "an empty clipped window can be presented without ordinary row coverage"
    );
}

#[test]
fn nearest_scroll_owns_feedback_inside_an_outer_clip() {
    let (mut f, list, _, inner) = fixture_nested(1_000, false, 1., true);
    let first = feedback(&mut f, list);
    assert_eq!(first.scroll, inner);
    assert_eq!(first.viewport, Size::new(200., 100.));
    assert_eq!((first.visible_start, first.visible_end), (-19., 41.));
    offset(&mut f, inner, 200.);
    let moved = feedback(&mut f, list);
    assert_eq!(moved.scroll, inner);
    assert_eq!(moved.offset.y, 200.);
    assert_eq!((moved.visible_start, moved.visible_end), (181., 241.));
    let outer = f.root;
    offset(&mut f, outer, 30.);
    let clipped = feedback(&mut f, list);
    assert_eq!(clipped.scroll, inner);
    assert_eq!(clipped.offset.y, 200.);
    assert_eq!((clipped.visible_start, clipped.visible_end), (186., 246.));
}

#[test]
fn hidden_nested_scroll_preserves_pending_offset_until_layout() {
    let (mut f, list, _, owner) = fixture(1_000, false, 1.);
    offset(&mut f, owner, 200.);
    f.edit(owner, |props| props.visible = false);
    for _ in 0..2 {
        let frame = f.record();
        f.accept(&frame);
        assert!(
            !f.ui
                .take_events()
                .iter()
                .any(|event| matches!(event, NodeEvent::Scroll(_) | NodeEvent::VirtualList(_)))
        );
    }
    f.edit(owner, |props| props.visible = true);
    let restored = feedback(&mut f, list);
    assert_eq!(restored.offset.y, 200.);
    assert_eq!((restored.visible_start, restored.visible_end), (156., 256.));
}

#[test]
fn rotated_and_degenerate_viewports_report_the_owner_axis_explicitly() {
    let (mut f, list, _, _) = fixture(1_000, false, 0.);
    let collapsed = feedback(&mut f, list);
    assert_eq!(collapsed.scroll_scale, 0.);
    assert_eq!(collapsed.visible_start, collapsed.visible_end);
    assert!(collapsed.visible_start.is_finite());
    let (mut f, list, _, _) = fixture(1_000, false, 1.);
    let wrapper = f.stage.node(f.root).unwrap().children[0];
    f.edit(wrapper, |props| props.rotation = 90.);
    let rotated = feedback(&mut f, list);
    assert!(rotated.scroll_scale.abs() < 1e-9);
    assert!(rotated.visible_start.is_finite() && rotated.visible_end.is_finite());
}
