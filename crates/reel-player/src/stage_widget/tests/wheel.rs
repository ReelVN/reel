use super::*;
use reel_ui::{
    core::ScrollDelta,
    core::{
        PointerEvent, PointerId, PointerInfo, PointerScrollEvent, PointerState, PointerType,
        WindowEvent,
    },
    dpi::PhysicalPosition,
    stage::{InputBindings, NodeEvent},
};

#[test]
fn wheel_routes_at_scroll_boundaries_with_accepted_geometry_dpi_and_modal_scope() {
    for canvas in [None, Some([160, 105])] {
        check_routes(canvas);
    }
}

fn check_routes(canvas: Option<[u32; 2]>) {
    let mut f = Fixture::with_canvas(None, canvas);
    f.stage.remove(f.blue).unwrap();
    f.stage
        .update(
            f.root,
            NodeKind::Input {
                bindings: InputBindings::new(vec![]).unwrap(),
                listening: false,
                keyboard: false,
                pointer: false,
                wheel: true,
            },
            NodeProps {
                x: 10.,
                y: 10.,
                width: Some(200.),
                height: Some(150.),
                scale_x: 2.,
                scale_y: 2.,
                ..Default::default()
            },
        )
        .unwrap();
    f.stage
        .update(
            f.red,
            NodeKind::Scroll {
                overscroll: Default::default(),
                axis: reel_ui::widgets::ScrollAxis::Vertical,
                follow_end: false,
                align: reel_ui::widgets::ScrollAlign::Start,
                offset: Vec2::ZERO,
            },
            NodeProps {
                width: Some(100.),
                height: Some(80.),
                ..Default::default()
            },
        )
        .unwrap();
    let body = f
        .stage
        .create(
            NodeKind::Stack,
            NodeProps {
                width: Some(100.),
                height: Some(240.),
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    f.stage.add(f.red, body, 0).unwrap();
    f.bridge.handle_window_event(WindowEvent::Rescale(2.));
    let frame = f.record();
    f.accept(&frame);
    let wheel = |f: &mut Fixture, delta| {
        f.ui.handle_pointer(
            &mut f.bridge,
            PointerEvent::Scroll(PointerScrollEvent {
                pointer: PointerInfo {
                    pointer_id: Some(PointerId::PRIMARY),
                    persistent_device_id: None,
                    pointer_type: PointerType::Mouse,
                },
                state: PointerState {
                    position: PhysicalPosition::new(120., 100.),
                    ..Default::default()
                },
                delta,
            }),
        )
        .unwrap()
        .events
        .into_iter()
        .filter_map(|event| match event {
            NodeEvent::Wheel(event) => Some(event),
            _ => None,
        })
        .collect::<Vec<_>>()
    };
    let at_top = wheel(&mut f, ScrollDelta::LineDelta(0., -1.));
    assert_eq!(at_top.len(), 1);
    assert_eq!(at_top[0].node, f.root);
    assert_eq!(
        at_top[0].position,
        if canvas.is_some() {
            Point::new(10., 7.5)
        } else {
            Point::new(25., 20.)
        }
    );
    assert_eq!(at_top[0].delta, Vec2::new(0., -40.));
    assert!(
        wheel(
            &mut f,
            ScrollDelta::PixelDelta(PhysicalPosition::new(0., 80.))
        )
        .is_empty(),
        "inner scroll consumes movement before ancestor input"
    );
    let frame = f.record();
    f.accept(&frame);
    assert!(wheel(&mut f, ScrollDelta::LineDelta(0., 100.)).is_empty());
    let frame = f.record();
    f.accept(&frame);
    let at_bottom = wheel(
        &mut f,
        ScrollDelta::PixelDelta(PhysicalPosition::new(0., 80.)),
    );
    assert_eq!(
        at_bottom[0].delta,
        Vec2::new(0., 40.),
        "DPI conversion without node-scale division"
    );
    let page = wheel(&mut f, ScrollDelta::PageDelta(0., 1.));
    assert_eq!(page[0].delta, Vec2::new(0., 150.));
    let modal = f
        .stage
        .create(
            NodeKind::Stack,
            NodeProps {
                modal: true,
                width: Some(200.),
                height: Some(150.),
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    f.stage.add(f.root, modal, 1).unwrap();
    let frame = f.record();
    f.accept(&frame);
    assert!(
        wheel(&mut f, ScrollDelta::LineDelta(0., 1.)).is_empty(),
        "modal scope excludes ancestor input"
    );
    f.stage.remove(modal).unwrap();
    f.edit(f.root, |props| props.enabled = false);
    let frame = f.record();
    f.accept(&frame);
    assert!(wheel(&mut f, ScrollDelta::LineDelta(0., 1.)).is_empty());
}
