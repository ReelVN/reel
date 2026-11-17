use super::*;
use reel_ui::{
    input::{ActionPhase, NavigationAction, NavigationEvent},
    widgets::ScrollBarView,
};

fn fixture(horizontal: bool) -> (Fixture, NodeId) {
    let mut f = Fixture::new(None);
    let mut stage = Stage::default();
    let track = stage
        .create(
            NodeKind::Stack,
            NodeProps {
                stretch: reel_ui::stage::Stretch::Both,
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    let thumb = stage
        .create(
            NodeKind::Stack,
            NodeProps {
                stretch: reel_ui::stage::Stretch::Both,
                background: Some([120, 130, 140, 255]),
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    let bar = stage
        .create(
            NodeKind::ScrollBar(ScrollBarView {
                axis: if horizontal {
                    ScrollAxis::Horizontal
                } else {
                    ScrollAxis::Vertical
                },
                viewport: 100.,
                content: 500.,
                live: true,
                ..Default::default()
            }),
            NodeProps {
                x: 40.,
                y: 30.,
                width: Some(if horizontal { 200. } else { 20. }),
                height: Some(if horizontal { 20. } else { 200. }),
                ..Default::default()
            },
            vec![track, thumb],
        )
        .unwrap();
    let root = stage
        .create(NodeKind::Stack, NodeProps::default(), vec![bar])
        .unwrap();
    stage.replace(root).unwrap();
    f.stage = stage;
    f.root = root;
    let frame = f.record();
    f.accept(&frame);
    (f, bar)
}
fn pointer(f: &mut Fixture, position: (f64, f64), down: Option<bool>) -> Vec<NodeEvent> {
    f.ui.handle_pointer(
        &mut f.bridge,
        super::cursor::pointer(PointerType::Mouse, position, down),
    )
    .unwrap()
    .events
}
fn changed(events: &[NodeEvent], bar: NodeId) -> Vec<f64> {
    events
        .iter()
        .filter_map(|e| match e {
            NodeEvent::Change(NodeChange {
                node,
                value: ControlValue::Number(v),
                ..
            }) if *node == bar => Some(*v),
            _ => None,
        })
        .collect()
}
#[test]
fn scrollbar_grab_uses_accepted_thumb_when_new_value_is_unpresented() {
    let (mut f, bar) = fixture(false);
    let node = f.stage.node(bar).unwrap();
    let NodeKind::ScrollBar(mut view) = node.kind else {
        panic!()
    };
    view.value = 200.;
    f.stage
        .update(bar, NodeKind::ScrollBar(view), node.props.clone())
        .unwrap();
    let _unaccepted = f.record();
    assert!(changed(&pointer(&mut f, (50., 45.), Some(true)), bar).is_empty());
    assert_eq!(changed(&pointer(&mut f, (50., 85.), None), bar), vec![100.]);
    pointer(&mut f, (50., 85.), Some(false));
}
#[test]
fn scrollbar_capture_continues_across_rotated_accepted_frames_without_relayout() {
    let (mut f, bar) = fixture(false);
    f.edit(bar, |p| {
        p.x = 140.25;
        p.y = 80.375;
        p.rotation = 0.23;
    });
    let frame = f.record();
    f.accept(&frame);
    let widget = f.layouts()[&bar].0;
    let world = f
        .bridge
        .layout_geometry()
        .unwrap()
        .get(widget)
        .unwrap()
        .window_transform;
    let point = |y| {
        let p = world * Point::new(10., y);
        (p.x, p.y)
    };
    pointer(&mut f, point(15.), Some(true));
    let first = changed(&pointer(&mut f, point(55.), None), bar);
    assert_eq!(first.len(), 1);
    assert!((first[0] - 100.).abs() < 1e-6);
    let before = f.layouts()[&bar].1;
    let node = f.stage.node(bar).unwrap();
    let NodeKind::ScrollBar(mut view) = node.kind else {
        panic!()
    };
    view.value = first[0];
    f.stage
        .update(bar, NodeKind::ScrollBar(view), node.props.clone())
        .unwrap();
    let frame = f.record();
    f.accept(&frame);
    assert_eq!(
        f.layouts()[&bar].1,
        before,
        "logical movement only composes the retained slots"
    );
    let second = changed(&pointer(&mut f, point(95.), None), bar);
    assert_eq!(second.len(), 1);
    assert!((second[0] - 200.).abs() < 1e-6);
    pointer(&mut f, point(95.), Some(false));
}
#[test]
fn scrollbar_invalidated_geometry_and_disabled_capture_stop_without_value_writes() {
    for disable in [false, true] {
        let (mut f, bar) = fixture(false);
        pointer(&mut f, (50., 45.), Some(true));
        if disable {
            f.edit(bar, |p| p.enabled = false);
        } else {
            let node = f.stage.node(bar).unwrap();
            let NodeKind::ScrollBar(mut view) = node.kind else {
                panic!()
            };
            view.content = 800.;
            f.stage
                .update(bar, NodeKind::ScrollBar(view), node.props.clone())
                .unwrap();
        }
        let _unaccepted = f.record();
        assert!(changed(&pointer(&mut f, (50., 130.), None), bar).is_empty());
        pointer(&mut f, (50., 130.), Some(false));
    }
}

#[test]
fn scrollbar_continues_from_the_corrected_offset_after_virtual_extent_changes() {
    let (mut f, bar) = fixture(false);
    pointer(&mut f, (50., 45.), Some(true));
    assert_eq!(changed(&pointer(&mut f, (50., 85.), None), bar), vec![100.]);

    let node = f.stage.node(bar).unwrap();
    let NodeKind::ScrollBar(mut view) = node.kind else {
        panic!()
    };
    view.value = 120.;
    view.content = 800.;
    f.stage
        .update(bar, NodeKind::ScrollBar(view), node.props.clone())
        .unwrap();
    let frame = f.record();
    assert!(changed(&pointer(&mut f, (50., 95.), None), bar).is_empty());
    f.accept(&frame);
    assert_eq!(
        changed(&pointer(&mut f, (50., 125.), None), bar),
        vec![280.]
    );
    pointer(&mut f, (50., 125.), Some(false));
}
#[test]
fn horizontal_scrollbar_shares_controller_and_accessibility_range_actions() {
    let (mut f, bar) = fixture(true);
    let widget = f.layouts()[&bar].0;
    assert_eq!(
        f.bridge
            .root()
            .get_widget(widget)
            .unwrap()
            .accessibility_role(),
        Role::ScrollBar
    );
    assert!(f.bridge.focus_on(Some(widget)));
    let events =
        f.ui.handle_text(
            &mut f.bridge,
            TextEvent::Navigation(NavigationEvent {
                action: NavigationAction::Right,
                phase: ActionPhase::Pressed,
                value: 1.,
            }),
        )
        .unwrap()
        .events;
    assert_eq!(changed(&events, bar), vec![40.]);
    let events =
        f.ui.handle_access(
            &mut f.bridge,
            reel_ui::accesskit::ActionRequest {
                action: reel_ui::accesskit::Action::SetValue,
                target_tree: reel_ui::accesskit::TreeId::ROOT,
                target_node: widget.into(),
                data: Some(reel_ui::accesskit::ActionData::NumericValue(250.)),
            },
        )
        .unwrap()
        .events;
    assert_eq!(changed(&events, bar), vec![250.]);
}
