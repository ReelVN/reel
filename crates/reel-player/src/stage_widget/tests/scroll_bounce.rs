use super::*;
use reel_ui::{core::WindowEvent, widgets::ScrollOverscroll};
use std::time::Duration;

fn touch(f: &mut Fixture, point: (f64, f64), pressed: Option<bool>) -> Vec<NodeEvent> {
    f.ui.handle_pointer(
        &mut f.bridge,
        super::cursor::pointer(PointerType::Touch, point, pressed),
    )
    .unwrap()
    .events
}

#[test]
fn touch_bounce_preserves_virtual_window_and_returns_to_idle() {
    let (mut f, list, _) = virtual_fixture(1000);
    let node = f.stage.node(f.root).unwrap();
    let mut kind = node.kind.clone();
    if let NodeKind::Scroll { overscroll, .. } = &mut kind {
        *overscroll = ScrollOverscroll::Bounce;
    }
    f.stage.update(f.root, kind, node.props.clone()).unwrap();
    let initial = f.record();
    f.accept(&initial);
    f.bridge
        .handle_window_event(WindowEvent::AnimFrame(Duration::ZERO));
    let base = f.record();
    f.accept(&base);
    f.ui.take_events();
    let list_widget = f.layouts()[&list].0;
    let original = base.geometry().get(list_widget).unwrap().window_transform;

    let mut events = touch(&mut f, (20., 20.), Some(true));
    events.extend(touch(&mut f, (20., 60.), None));
    let pulled = f.record();
    let translated = pulled.geometry().get(list_widget).unwrap().window_transform;
    assert!(translated.translation().y > original.translation().y);
    assert!(translated.translation().y < original.translation().y + 40.);
    let mut input = false;
    events.extend(f.ui.take_events());
    for event in events {
        match event {
            NodeEvent::Scroll(event) => {
                input |= event.input;
                assert_eq!(event.offset, Vec2::ZERO);
            }
            NodeEvent::VirtualList(event) => {
                assert_eq!(event.offset, Vec2::ZERO);
                assert_eq!(event.visible_start, -4.);
                assert_eq!(event.visible_end, 96.);
            }
            _ => {}
        }
    }
    assert!(input, "edge drag must interrupt an existing scroll_to");
    f.accept(&pulled);
    touch(&mut f, (20., 60.), Some(false));
    assert!(f.bridge.root().needs_anim());
    for _ in 0..80 {
        f.bridge
            .handle_window_event(WindowEvent::AnimFrame(Duration::from_nanos(16_666_667)));
        let frame = f.record();
        f.accept(&frame);
        for event in f.ui.take_events() {
            if let NodeEvent::Scroll(event) = event {
                assert!(!event.input);
                assert_eq!(event.offset, Vec2::ZERO);
            }
        }
        if !f.bridge.root().needs_anim() {
            break;
        }
    }
    assert!(!f.bridge.root().needs_anim());
    let returned = f.record();
    assert_eq!(
        returned
            .geometry()
            .get(list_widget)
            .unwrap()
            .window_transform,
        original
    );
}

#[test]
fn clamp_mode_does_not_capture_or_animate_an_outward_edge_drag() {
    let (mut f, list, _) = virtual_fixture(1000);
    let initial = f.record();
    f.accept(&initial);
    f.bridge
        .handle_window_event(WindowEvent::AnimFrame(Duration::ZERO));
    let base = f.record();
    f.accept(&base);
    assert!(!f.bridge.root().needs_anim());
    let widget = f.layouts()[&list].0;
    let original = base.geometry().get(widget).unwrap().window_transform;
    touch(&mut f, (20., 20.), Some(true));
    touch(&mut f, (20., 60.), None);
    touch(&mut f, (20., 60.), Some(false));
    let frame = f.record();
    assert_eq!(
        frame.geometry().get(widget).unwrap().window_transform,
        original
    );
    assert!(!f.bridge.root().needs_anim());
}

#[test]
fn changing_boundary_policy_clears_an_active_pull_without_changing_position() {
    let (mut f, list, _) = virtual_fixture(1000);
    let node = f.stage.node(f.root).unwrap();
    let mut kind = node.kind.clone();
    if let NodeKind::Scroll { overscroll, .. } = &mut kind {
        *overscroll = ScrollOverscroll::Bounce;
    }
    f.stage.update(f.root, kind, node.props.clone()).unwrap();
    let base = f.record();
    f.accept(&base);
    let widget = f.layouts()[&list].0;
    let original = base.geometry().get(widget).unwrap().window_transform;
    touch(&mut f, (20., 20.), Some(true));
    touch(&mut f, (20., 60.), None);
    let pulled = f.record();
    f.accept(&pulled);
    assert_ne!(
        pulled.geometry().get(widget).unwrap().window_transform,
        original
    );
    let node = f.stage.node(f.root).unwrap();
    let mut kind = node.kind.clone();
    if let NodeKind::Scroll { overscroll, .. } = &mut kind {
        *overscroll = ScrollOverscroll::Clamp;
    }
    f.stage.update(f.root, kind, node.props.clone()).unwrap();
    let reset = f.record();
    f.accept(&reset);
    assert_eq!(
        reset.geometry().get(widget).unwrap().window_transform,
        original
    );
    touch(&mut f, (20., 60.), Some(false));
    f.bridge
        .handle_window_event(WindowEvent::AnimFrame(Duration::ZERO));
    assert!(!f.bridge.root().needs_anim());
}
