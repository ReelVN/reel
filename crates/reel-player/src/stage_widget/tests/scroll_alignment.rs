use super::*;
use reel_ui::{
    core::{
        PointerButton, PointerButtonEvent, PointerEvent, PointerId, PointerInfo, PointerState,
        PointerType,
    },
    dpi::PhysicalPosition,
    stage::Stretch,
};

fn click(f: &mut Fixture, x: f64, y: f64) -> Vec<NodeId> {
    let mut clicked = Vec::new();
    for pressed in [true, false] {
        let event = PointerButtonEvent {
            pointer: PointerInfo {
                pointer_id: Some(PointerId::PRIMARY),
                persistent_device_id: None,
                pointer_type: PointerType::Mouse,
            },
            state: PointerState {
                position: PhysicalPosition::new(x, y),
                ..Default::default()
            },
            button: Some(PointerButton::Primary),
        };
        for event in
            f.ui.handle_pointer(
                &mut f.bridge,
                if pressed {
                    PointerEvent::Down(event)
                } else {
                    PointerEvent::Up(event)
                },
            )
            .unwrap()
            .events
        {
            if let NodeEvent::Click(event) = event {
                clicked.push(event.node);
            }
        }
    }
    clicked
}

#[test]
fn scroll_cross_axis_keeps_explicit_caps_alignment_and_hit_geometry() {
    for (axis, capped) in [
        (ScrollAxis::Vertical, false),
        (ScrollAxis::Vertical, true),
        (ScrollAxis::Horizontal, false),
        (ScrollAxis::Horizontal, true),
    ] {
        let mut f = Fixture::new(None);
        f.stage.clear_children(f.root).unwrap();
        let vertical = axis == ScrollAxis::Vertical;
        let child = f
            .stage
            .create(
                NodeKind::Button {
                    label: None,
                    listening: true,
                    focus_on_pointer: true,
                    focusable: true,
                },
                NodeProps {
                    width: Some(if vertical {
                        if capped { 160. } else { 80. }
                    } else {
                        400.
                    }),
                    height: Some(if vertical {
                        400.
                    } else if capped {
                        160.
                    } else {
                        80.
                    }),
                    max_width: (vertical && capped).then_some(80.),
                    max_height: (!vertical && capped).then_some(80.),
                    align_x: 0.5,
                    align_y: 0.5,
                    ..Default::default()
                },
                vec![],
            )
            .unwrap();
        f.stage.add(f.root, child, 0).unwrap();
        f.stage
            .update(
                f.root,
                NodeKind::Scroll {
                    overscroll: Default::default(),
                    axis,
                    follow_end: false,
                    snap_y: None,
                    align: reel_ui::widgets::ScrollAlign::Start,
                    offset: Vec2::ZERO,
                },
                NodeProps {
                    width: Some(220.),
                    height: Some(220.),
                    padding: 10.,
                    ..Default::default()
                },
            )
            .unwrap();
        let first = f.record();
        f.accept(&first);
        assert!(
            f.ui.take_events()
                .iter()
                .any(|event| matches!(event, NodeEvent::Scroll(scroll) if scroll.node == f.root))
        );
        let ids = f.layouts();
        let geometry = first.geometry().get(ids[&child].0).unwrap();
        assert_eq!(
            geometry.size,
            if vertical {
                Size::new(80., 400.)
            } else {
                Size::new(400., 80.)
            }
        );
        assert_eq!(
            geometry.window_transform.translation(),
            if vertical {
                Vec2::new(70., 10.)
            } else {
                Vec2::new(10., 70.)
            }
        );
        assert!(
            click(&mut f, 20., 20.).is_empty(),
            "the unused cross-axis margin must not hit the child"
        );
        assert_eq!(click(&mut f, 80., 80.), [child]);

        let props = f.stage.node(f.root).unwrap().props.clone();
        f.stage
            .update(
                f.root,
                NodeKind::Scroll {
                    overscroll: Default::default(),
                    axis,
                    follow_end: false,
                    snap_y: None,
                    align: reel_ui::widgets::ScrollAlign::Start,
                    offset: if vertical {
                        Vec2::new(0., 30.)
                    } else {
                        Vec2::new(30., 0.)
                    },
                },
                props,
            )
            .unwrap();
        let moved = f.record();
        f.accept(&moved);
        assert!(
            !f.ui
                .take_events()
                .iter()
                .any(|event| matches!(event, NodeEvent::Scroll(scroll) if scroll.node == f.root))
        );
        assert_eq!(
            f.layouts(),
            ids,
            "ordinary scroll motion retains layout caches"
        );

        f.edit(child, |props| {
            props.stretch = if vertical { Stretch::X } else { Stretch::Y }
        });
        let filled = f.record();
        f.accept(&filled);
        let geometry = filled.geometry().get(ids[&child].0).unwrap();
        assert_eq!(
            geometry.size,
            if vertical {
                Size::new(200., 400.)
            } else {
                Size::new(400., 200.)
            }
        );
        assert_eq!(
            geometry.window_transform.translation(),
            if vertical {
                Vec2::new(10., -20.)
            } else {
                Vec2::new(-20., 10.)
            }
        );
    }
}

#[test]
fn scroll_cross_axis_virtual_list_uses_its_own_width_for_rows_and_feedback() {
    let (mut f, list, row) = virtual_fixture(1000);
    f.edit(list, |props| {
        props.width = Some(80.);
        props.padding = 0.;
        props.align_x = 0.5;
    });
    let frame = f.record();
    f.accept(&frame);
    let ids = f.layouts();
    for node in [list, row] {
        let geometry = frame.geometry().get(ids[&node].0).unwrap();
        assert_eq!(geometry.size.width, 80.);
        assert_eq!(geometry.window_transform.translation().x, 60.);
    }
    let measured =
        f.ui.take_events()
            .into_iter()
            .find_map(|event| match event {
                NodeEvent::VirtualList(event) => Some(event),
                _ => None,
            })
            .unwrap();
    assert_eq!(measured.width, 80.);
    assert_eq!(measured.viewport, Size::new(200., 100.));
    assert!(click(&mut f, 20., 20.).is_empty());
    assert_eq!(click(&mut f, 70., 20.), [row]);
}
