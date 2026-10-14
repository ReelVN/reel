use super::*;
use reel_ui::app::{RenderRoot, RenderRootSignal};

struct Contacts {
    root: RenderRoot,
    group: WidgetId,
    a: WidgetId,
    b: WidgetId,
    trace: Trace,
    signals: Rc<RefCell<Vec<RenderRootSignal>>>,
}
fn contacts(presented: bool) -> Contacts {
    let trace = Trace::default();
    let mut group = StageGroup::new(trace.clone(), 1., Rect::new(0., 0., 400., 300.));
    let a = group.add(
        image_button(trace.clone(), false).0,
        Point::new(10., 10.),
        Size::new(80., 40.),
    );
    let b = group.add(
        image_button(trace.clone(), false).0,
        Point::new(110., 10.),
        Size::new(80., 40.),
    );
    let group = group.prepare();
    let group_id = group.id();
    let (mut root, signals) = root(group);
    root.redraw();
    if presented {
        let (_, view) = root
            .record_presented_geometry(bridge_limits().geometry)
            .unwrap();
        root.set_input_geometry_source(InputGeometrySource::Presented(Some(Arc::new(view))));
    }
    Contacts {
        root,
        group: group_id,
        a,
        b,
        trace,
        signals,
    }
}
fn presses(f: &Contacts, id: WidgetId) -> usize {
    f.signals
        .borrow()
        .iter()
        .filter(|s| matches!(s, RenderRootSignal::Action(_, target) if *target == id))
        .count()
}

#[test]
fn concurrent_contacts_keep_capture_and_aggregate_visual_state() {
    for presented in [false, true] {
        let mut f = contacts(presented);
        let a = Point::new(30., 30.);
        let b = Point::new(130., 30.);
        f.root.handle_pointer_event(touch("down", 2, a));
        f.root.handle_pointer_event(touch("down", 3, b));
        assert_eq!(
            f.root
                .pointer_capture_for(pointer_info(&touch("move", 2, a))),
            Some(f.a)
        );
        assert_eq!(
            f.root
                .pointer_capture_for(pointer_info(&touch("move", 3, b))),
            Some(f.b)
        );
        assert!(f.root.get_widget(f.a).unwrap().ctx().is_active());
        assert!(f.root.get_widget(f.b).unwrap().ctx().is_active());
        f.root.handle_pointer_event(touch("cancel", 3, b));
        assert_eq!(presses(&f, f.b), 0);
        assert!(f.root.get_widget(f.a).unwrap().ctx().is_active());
        assert!(!f.root.get_widget(f.b).unwrap().ctx().is_active());
        f.root.handle_pointer_event(touch("up", 2, a));
        assert_eq!(presses(&f, f.a), 1);
        assert!(!f.root.get_widget(f.a).unwrap().ctx().is_hovered());
        assert!(!f.root.get_widget(f.group).unwrap().ctx().has_active());
        // Orphaned terminals and a second finger over the same control cannot click.
        f.root.handle_pointer_event(touch("up", 3, a));
        f.root.handle_pointer_event(touch("down", 4, a));
        f.root.handle_pointer_event(touch("down", 5, a));
        f.root.handle_pointer_event(touch("up", 5, a));
        assert_eq!(presses(&f, f.a), 1);
        f.root
            .handle_pointer_event(touch("up", 4, Point::new(350., 250.)));
        assert_eq!(
            presses(&f, f.a),
            1,
            "release outside without an intervening move"
        );
    }
}

#[test]
fn contact_cancellation_preserves_identity_across_lifecycle_and_retirement() {
    for presented in [false, true] {
        for cause in ["blur", "clear", "disable", "remove", "modal"] {
            let mut f = contacts(presented);
            f.root
                .handle_pointer_event(touch("down", 2, Point::new(30., 30.)));
            f.root
                .handle_pointer_event(touch("down", 3, Point::new(130., 30.)));
            f.trace.borrow_mut().clear();
            match cause {
                "blur" => {
                    f.root
                        .handle_text_event(TextEvent::WindowFocusChange(false));
                }
                "clear" => f
                    .root
                    .set_input_geometry_source(InputGeometrySource::Presented(None)),
                "disable" => f
                    .root
                    .edit_widget(f.group, |mut w| w.ctx.set_disabled(true)),
                "remove" => f.root.edit_widget(f.group, |mut w| {
                    let mut group = w.downcast::<StageGroup>();
                    StageGroup::remove(&mut group, 1);
                    StageGroup::remove(&mut group, 0);
                }),
                "modal" => {
                    f.root.edit_widget(f.b, |mut w| w.ctx.set_modal(true));
                    if presented {
                        let (_, view) = f
                            .root
                            .record_presented_geometry(bridge_limits().geometry)
                            .unwrap();
                        f.root
                            .set_input_geometry_source(InputGeometrySource::Presented(Some(
                                Arc::new(view),
                            )));
                    }
                }
                _ => unreachable!(),
            }
            for (id, pointer) in [(f.a, 2), (f.b, 3)] {
                let expected = usize::from(cause != "modal" || id == f.a);
                let trace = f.trace.borrow();
                let cancels: Vec<_> = trace
                    .iter()
                    .filter(|e| e.widget == id && e.kind == "cancel")
                    .collect();
                assert_eq!(
                    cancels.len(),
                    expected,
                    "{cause}, presented={presented}, widget={id:?}"
                );
                if expected == 1 {
                    assert_eq!(cancels[0].pointer.pointer_id, PointerId::new(pointer));
                    assert!(
                        cancels[0].active,
                        "terminal retains capture during callback"
                    );
                }
            }
            f.root.cancel_pointer_inputs();
            assert_eq!(presses(&f, f.a) + presses(&f, f.b), 0);
        }
    }
}

#[test]
fn contacts_are_charged_before_dispatch_and_release_their_paths() {
    let f = contacts(false);
    let mut bridge = PresentationBridge::new(f.root, SurfaceEpoch(1), bridge_limits()).unwrap();
    let frame = bridge.record(FrameId(1)).unwrap();
    bridge.resolve(accept(&frame));
    let initial = bridge.routing_storage_bytes();
    bridge
        .handle_pointer_event(touch("down", 2, Point::new(30., 30.)))
        .unwrap();
    let active = bridge.routing_storage_bytes();
    let admission_limit = active - 1;
    assert!(active > initial);
    bridge
        .handle_pointer_event(touch("up", 2, Point::new(30., 30.)))
        .unwrap();
    let released = bridge.routing_storage_bytes();
    assert!(released < active);
    for id in 3..100 {
        bridge
            .handle_pointer_event(touch("down", id, Point::new(30., 30.)))
            .unwrap();
        bridge
            .handle_pointer_event(touch("up", id, Point::new(30., 30.)))
            .unwrap();
        assert_eq!(
            bridge.routing_storage_bytes(),
            released,
            "no historic contact accumulation"
        );
    }
    let f = contacts(false);
    let mut limits = bridge_limits();
    limits.max_bridge_bytes = admission_limit;
    let mut limited = PresentationBridge::new(f.root, SurfaceEpoch(1), limits).unwrap();
    let frame = limited.record(FrameId(1)).unwrap();
    limited.resolve(accept(&frame));
    let down = mouse("down", Point::new(30., 30.));
    let pointer = pointer_info(&down);
    limited.handle_pointer_event(down).unwrap();
    assert_eq!(
        limited.handle_pointer_event(touch("down", 3, Point::new(130., 30.))),
        Err(InputBudgetExceeded)
    );
    assert_eq!(limited.root().pointer_capture_for(pointer), Some(f.a));
    assert_eq!(
        limited
            .root()
            .pointer_capture_for(pointer_info(&touch("down", 3, Point::ZERO))),
        None
    );
}
