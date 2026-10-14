use super::*;
use reel_ui::app::{RenderRoot, RenderRootSignal};

struct Fixture {
    root: RenderRoot,
    container: WidgetId,
    outside: WidgetId,
    panel: WidgetId,
    first: WidgetId,
    second: WidgetId,
    nested: WidgetId,
    trace: Trace,
    signals: Rc<RefCell<Vec<RenderRootSignal>>>,
}
fn fixture() -> Fixture {
    let trace = Trace::default();
    let control = || image_button(trace.clone(), false).0;
    let mut group = StageGroup::new(trace.clone(), 1., Rect::new(0., 0., 400., 300.));
    let outside = group.add(control(), Point::new(10., 10.), Size::new(80., 40.));
    let mut panel = StageGroup::new(trace.clone(), 1., Rect::new(0., 0., 300., 180.));
    let first = panel.add(control(), Point::new(10., 0.), Size::new(80., 40.));
    let second = panel.add(control(), Point::new(110., 0.), Size::new(80., 40.));
    let nested = panel.add(control(), Point::new(10., 60.), Size::new(80., 40.));
    let panel = group.add(panel.prepare(), Point::new(0., 80.), Size::new(300., 180.));
    let group = group.prepare();
    let container = group.id();
    let (root, signals) = root(group);
    Fixture {
        root,
        container,
        outside,
        panel,
        first,
        second,
        nested,
        trace,
        signals,
    }
}
fn accept_view(root: &mut RenderRoot, presented: bool) {
    if presented {
        let (_, view) = root.record_presented_geometry(support::limits()).unwrap();
        root.set_input_geometry_source(InputGeometrySource::Presented(Some(Arc::new(view))));
    }
}
fn tab(backward: bool) -> TextEvent {
    let mut event = TextEvent::key_down(keyboard::Key::Named(keyboard::NamedKey::Tab));
    if let TextEvent::Keyboard(key) = &mut event {
        key.modifiers.set(Modifiers::SHIFT, backward);
    }
    event
}

#[test]
fn modal_traps_input_restores_nested_focus_and_releases_capture_in_both_geometry_modes() {
    for presented in [false, true] {
        let mut f = fixture();
        accept_view(&mut f.root, presented);
        assert!(f.root.focus_on(Some(f.outside)));
        let enter = keyboard::Key::Named(keyboard::NamedKey::Enter);
        f.root.handle_text_event(TextEvent::key_down(enter.clone()));
        f.root
            .handle_pointer_event(mouse("down", Point::new(30., 30.)));
        assert_eq!(f.root.pointer_capture_target(), Some(f.outside));
        f.trace.borrow_mut().clear();
        f.root.edit_widget(f.panel, |mut w| w.ctx.set_modal(true));
        accept_view(&mut f.root, presented);
        assert_eq!(f.root.pointer_capture_target(), None);
        assert_eq!(f.root.focused_widget(), Some(f.first));
        f.signals.borrow_mut().clear();
        f.root.handle_text_event(TextEvent::key_up(enter.clone()));
        assert!(
            !f.signals
                .borrow()
                .iter()
                .any(|s| matches!(s, RenderRootSignal::Action(..))),
            "a key released after entering a modal cannot activate the new focus"
        );
        let mut repeated = TextEvent::key_down(enter.clone());
        if let TextEvent::Keyboard(key) = &mut repeated {
            key.repeat = true;
        }
        f.root.handle_text_event(repeated);
        f.root.handle_text_event(TextEvent::key_up(enter.clone()));
        assert!(
            !f.signals
                .borrow()
                .iter()
                .any(|s| matches!(s, RenderRootSignal::Action(..)))
        );
        f.root
            .handle_access_event(reel_ui::accesskit::ActionRequest {
                action: reel_ui::accesskit::Action::Click,
                target_tree: reel_ui::accesskit::TreeId::ROOT,
                target_node: reel_ui::accesskit::NodeId(f.outside.to_raw()),
                data: None,
            });
        assert!(
            !f.signals
                .borrow()
                .iter()
                .any(|s| matches!(s, RenderRootSignal::Action(..)))
        );
        f.root.handle_text_event(TextEvent::key_down(enter.clone()));
        f.root.handle_text_event(TextEvent::key_up(enter.clone()));
        assert_eq!(
            f.signals
                .borrow()
                .iter()
                .filter(|s| matches!(s, RenderRootSignal::Action(_, id) if *id == f.first))
                .count(),
            1
        );
        f.signals.borrow_mut().clear();
        f.root.handle_text_event(TextEvent::key_down(enter.clone()));
        f.root
            .handle_text_event(TextEvent::WindowFocusChange(false));
        f.root.handle_text_event(TextEvent::WindowFocusChange(true));
        f.root.handle_text_event(TextEvent::key_up(enter));
        assert!(
            !f.signals
                .borrow()
                .iter()
                .any(|s| matches!(s, RenderRootSignal::Action(..)))
        );
        assert_eq!(
            f.trace
                .borrow()
                .iter()
                .filter(|e| e.widget == f.outside && e.kind == "cancel")
                .count(),
            1
        );
        assert!(!f.root.focus_on(Some(f.outside)));
        f.trace.borrow_mut().clear();
        assert_eq!(
            f.root
                .handle_pointer_event(mouse("up", Point::new(30., 30.))),
            Handled::Yes
        );
        assert!(!f.trace.borrow().iter().any(|e| e.widget == f.outside));
        assert_eq!(f.root.handle_text_event(tab(true)), Handled::Yes);
        assert_eq!(f.root.focused_widget(), Some(f.nested));
        f.root.handle_text_event(tab(false));
        assert_eq!(f.root.focused_widget(), Some(f.first));
        f.root.handle_text_event(tab(false));
        assert_eq!(f.root.focused_widget(), Some(f.second));

        f.root.edit_widget(f.nested, |mut w| w.ctx.set_modal(true));
        accept_view(&mut f.root, presented);
        assert_eq!(f.root.focused_widget(), Some(f.nested));
        f.root.handle_text_event(tab(false));
        assert_eq!(f.root.focused_widget(), Some(f.nested));
        f.root.edit_widget(f.nested, |mut w| w.ctx.set_modal(false));
        accept_view(&mut f.root, presented);
        assert_eq!(f.root.focused_widget(), Some(f.second));

        // A disabled modal remains a barrier, including bubbling to its parent.
        f.root
            .edit_widget(f.panel, |mut w| w.ctx.set_disabled(true));
        accept_view(&mut f.root, presented);
        f.trace.borrow_mut().clear();
        assert_eq!(
            f.root
                .handle_pointer_event(mouse("down", Point::new(30., 100.))),
            Handled::Yes
        );
        assert!(!f.trace.borrow().iter().any(|e| e.widget == f.container));
        assert_eq!(
            f.root
                .handle_text_event(TextEvent::key_down(keyboard::Key::Named(
                    keyboard::NamedKey::Enter
                ))),
            Handled::Yes
        );
        f.root
            .edit_widget(f.panel, |mut w| w.ctx.set_disabled(false));
        accept_view(&mut f.root, presented);

        // Hiding the container releases its scope, then showing it restores it.
        f.root.edit_widget(f.container, |mut w| {
            let mut g = w.downcast::<StageGroup>();
            g.ctx.set_stashed(&mut g.widget.children[1].0, true);
        });
        accept_view(&mut f.root, presented);
        assert_eq!(f.root.focused_widget(), Some(f.outside));
        f.root.edit_widget(f.container, |mut w| {
            let mut g = w.downcast::<StageGroup>();
            g.ctx.set_stashed(&mut g.widget.children[1].0, false);
        });
        accept_view(&mut f.root, presented);
        assert!(f.root.focused_widget().is_some());
        // Retired focus identities never keep widgets alive or become focus targets.
        f.root.edit_widget(f.container, |mut w| {
            StageGroup::remove(&mut w.downcast::<StageGroup>(), 0)
        });
        f.root.edit_widget(f.container, |mut w| {
            StageGroup::remove(&mut w.downcast::<StageGroup>(), 0)
        });
        assert_eq!(f.root.handle_text_event(tab(false)), Handled::Yes);
        accept_view(&mut f.root, presented);
        assert_eq!(f.root.focused_widget(), None);
    }
}

#[test]
fn modal_activation_follows_acceptance_and_clear_preserves_restoration() {
    let f = fixture();
    let mut bridge = PresentationBridge::new(f.root, SurfaceEpoch(1), bridge_limits()).unwrap();
    assert_eq!(bridge.handle_text_event(tab(false)), Handled::Yes);
    assert_eq!(bridge.root().focused_widget(), None);
    let initial = bridge.record(FrameId(1)).unwrap();
    bridge.resolve(accept(&initial));
    bridge.handle_text_event(tab(false));
    assert_eq!(bridge.root().focused_widget(), Some(f.outside));
    bridge.handle_pointer_event(mouse("down", Point::new(30., 30.)));
    bridge.edit_widget(f.panel, |mut w| w.ctx.set_modal(true));
    let rejected = bridge.record(FrameId(2)).unwrap();
    assert_eq!(rejected.geometry().modal_scope(), Some(f.panel));
    assert_eq!(bridge.root().focused_widget(), Some(f.outside));
    assert_eq!(bridge.root().pointer_capture_target(), Some(f.outside));
    bridge.resolve(notice(&rejected, PresentationOutcome::Skipped));
    assert_eq!(bridge.root().focused_widget(), Some(f.outside));
    let accepted = bridge.record(FrameId(3)).unwrap();
    bridge.resolve(accept(&accepted));
    assert_eq!(bridge.root().focused_widget(), Some(f.first));
    assert_eq!(bridge.root().pointer_capture_target(), None);
    bridge.clear();
    assert_eq!(
        bridge.handle_pointer_event(mouse("down", Point::new(30., 30.))),
        Handled::Yes
    );
    assert_eq!(bridge.handle_text_event(tab(false)), Handled::Yes);
    assert_eq!(bridge.root().focused_widget(), Some(f.first));
    bridge.edit_widget(f.panel, |mut w| w.ctx.set_modal(false));
    let closed = bridge.record(FrameId(4)).unwrap();
    bridge.resolve(accept(&closed));
    assert_eq!(bridge.root().focused_widget(), Some(f.outside));
}
