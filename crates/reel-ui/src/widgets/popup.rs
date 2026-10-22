// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0

//! Modal layer behavior for ordinary authored child widgets.
use crate::{
    accesskit::{Node, Role},
    core::{
        AccessCtx, ChildrenIds, EventCtx, Layer, LayoutCtx, MeasureCtx, NewWidget, PaintCtx,
        PointerEvent, PropertiesMut, PropertiesRef, RegisterCtx, TextEvent, Update, UpdateCtx,
        Widget, WidgetId, WidgetPod,
        keyboard::{Key, NamedKey},
        pointer::PointerButton,
    },
    imaging::Painter,
    input::{ActionPhase, NavigationAction},
    kurbo::{Axis, Point, Rect, Size},
    layout::{LenReq, Length, SizeDef},
};
use tracing::{Span, trace_span};

/// Requested side and alignment of a floating child in window coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PopupPlacement {
    #[default]
    BottomStart,
    BottomEnd,
    TopStart,
    TopEnd,
    LeftStart,
    LeftEnd,
    RightStart,
    RightEnd,
}

impl PopupPlacement {
    /// Prefer the requested side, flip when the opposite side fits, then keep
    /// the origin in the viewport. Oversized content starts at the near edge.
    pub fn position(self, anchor: Rect, size: Size, viewport: Rect, gap: f64) -> Point {
        let (horizontal, after, end) = match self {
            Self::BottomStart => (false, true, false),
            Self::BottomEnd => (false, true, true),
            Self::TopStart => (false, false, false),
            Self::TopEnd => (false, false, true),
            Self::LeftStart => (true, false, false),
            Self::LeftEnd => (true, false, true),
            Self::RightStart => (true, true, false),
            Self::RightEnd => (true, true, true),
        };
        let (near, far, length, lower, upper) = if horizontal {
            (anchor.x0, anchor.x1, size.width, viewport.x0, viewport.x1)
        } else {
            (anchor.y0, anchor.y1, size.height, viewport.y0, viewport.y1)
        };
        let before = near - gap - length;
        let following = far + gap;
        let fits = |origin: f64| origin >= lower && origin + length <= upper;
        let (preferred, opposite) = if after {
            (following, before)
        } else {
            (before, following)
        };
        let major = if !fits(preferred) && fits(opposite) {
            opposite
        } else {
            preferred
        };
        let (x, y) = if horizontal {
            (
                major,
                if end {
                    anchor.y1 - size.height
                } else {
                    anchor.y0
                },
            )
        } else {
            (
                if end {
                    anchor.x1 - size.width
                } else {
                    anchor.x0
                },
                major,
            )
        };
        Point::new(
            x.clamp(viewport.x0, (viewport.x1 - size.width).max(viewport.x0)),
            y.clamp(viewport.y0, (viewport.y1 - size.height).max(viewport.y0)),
        )
    }
}

/// A user requested dismissal; the owner decides when to remove its layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PopupDismiss {
    Outside,
    Back,
}

/// A layer with ordinary child content and the shared modal focus lifecycle.
/// It emits dismissal actions without allocating or mutating the child tree.
pub struct Popup {
    child: WidgetPod<dyn Widget>,
    modal: bool,
    back_pressed: bool,
    escape_pressed: bool,
}

impl Popup {
    pub fn new(child: NewWidget<impl Widget + ?Sized>) -> Self {
        Self {
            child: child.erased().to_pod(),
            modal: true,
            back_pressed: false,
            escape_pressed: false,
        }
    }

    /// Nonmodal popups retain normal input routing to the underlying scene.
    pub fn with_modal(mut self, modal: bool) -> Self {
        self.modal = modal;
        self
    }
}

impl Layer for Popup {
    fn capture_pointer_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _: &mut PropertiesMut<'_>,
        event: &PointerEvent,
    ) {
        if ctx.input_configuration_is_current()
            && let PointerEvent::Down(event) = event
            && event
                .button
                .is_none_or(|button| button == PointerButton::Primary)
            && !ctx
                .border_box()
                .contains(ctx.local_position(event.state.position))
        {
            ctx.submit_action::<PopupDismiss>(PopupDismiss::Outside);
        }
    }
}

impl Widget for Popup {
    type Action = PopupDismiss;

    fn register_children(&mut self, ctx: &mut RegisterCtx<'_>) {
        ctx.register_child(&mut self.child);
    }

    fn update(&mut self, ctx: &mut UpdateCtx<'_>, _: &mut PropertiesMut<'_>, event: &Update) {
        if matches!(event, Update::WidgetAdded) {
            ctx.set_modal(self.modal);
        }
        if matches!(
            event,
            Update::WindowFocusChanged(false)
                | Update::DisabledChanged(true)
                | Update::StashedChanged(true)
                | Update::ChildFocusChanged(false)
        ) {
            self.back_pressed = false;
            self.escape_pressed = false;
        }
    }

    fn measure(
        &mut self,
        ctx: &mut MeasureCtx<'_>,
        _: &PropertiesRef<'_>,
        axis: Axis,
        _: LenReq,
        cross: Option<Length>,
    ) -> Length {
        ctx.redirect_measurement(&mut self.child, axis, cross)
    }

    fn layout(&mut self, ctx: &mut LayoutCtx<'_>, _: &PropertiesRef<'_>, size: Size) {
        let child_size = ctx.compute_size(&mut self.child, SizeDef::fit(size), size.into());
        ctx.run_layout(&mut self.child, child_size);
        ctx.place_child(&mut self.child, Point::ORIGIN);
        ctx.derive_baselines(&self.child);
    }

    fn on_text_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _: &mut PropertiesMut<'_>,
        event: &TextEvent,
    ) {
        if matches!(event, TextEvent::WindowFocusChange(false))
            || !ctx.input_configuration_is_current()
        {
            self.back_pressed = false;
            self.escape_pressed = false;
            return;
        }
        let (pressed, phase) = match event {
            TextEvent::Navigation(event) if event.action == NavigationAction::Back => {
                (&mut self.back_pressed, event.phase)
            }
            TextEvent::Keyboard(key) if key.key == Key::Named(NamedKey::Escape) => {
                let phase = if key.state.is_up() {
                    ActionPhase::Released
                } else if key.repeat {
                    ActionPhase::Repeated
                } else {
                    ActionPhase::Pressed
                };
                (&mut self.escape_pressed, phase)
            }
            _ => return,
        };
        match phase {
            ActionPhase::Pressed => *pressed = true,
            ActionPhase::Cancelled => *pressed = false,
            ActionPhase::Released if std::mem::take(pressed) => {
                ctx.submit_action::<PopupDismiss>(PopupDismiss::Back);
            }
            _ => {}
        }
        ctx.set_handled();
    }

    fn paint(&mut self, _: &mut PaintCtx<'_>, _: &PropertiesRef<'_>, _: &mut Painter<'_>) {}
    fn accessibility_role(&self) -> Role {
        Role::GenericContainer
    }
    fn accessibility(&mut self, _: &mut AccessCtx<'_>, _: &PropertiesRef<'_>, _: &mut Node) {}
    fn children_ids(&self) -> ChildrenIds {
        ChildrenIds::from_slice(&[self.child.id()])
    }
    fn as_layer(&mut self) -> Option<&mut dyn Layer> {
        Some(self)
    }
    fn make_trace_span(&self, id: WidgetId) -> Span {
        trace_span!("Popup", id = id.trace())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::{
            GeometryLimits, InputGeometrySource, RenderRoot, RenderRootOptions, RenderRootSignal,
            WindowSizePolicy,
        },
        core::{
            DefaultProperties, PointerButtonEvent, PointerId, PointerInfo, PointerState,
            PointerType,
        },
        input::NavigationEvent,
    };
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
        sync::Arc,
    };

    struct Content(Rc<Cell<usize>>);
    impl Widget for Content {
        type Action = ();
        fn register_children(&mut self, _: &mut RegisterCtx<'_>) {}
        fn measure(
            &mut self,
            _: &mut MeasureCtx<'_>,
            _: &PropertiesRef<'_>,
            _: Axis,
            _: LenReq,
            _: Option<Length>,
        ) -> Length {
            Length::px(40.)
        }
        fn layout(&mut self, _: &mut LayoutCtx<'_>, _: &PropertiesRef<'_>, _: Size) {}
        fn paint(&mut self, _: &mut PaintCtx<'_>, _: &PropertiesRef<'_>, _: &mut Painter<'_>) {}
        fn accessibility_role(&self) -> Role {
            Role::Button
        }
        fn accessibility(&mut self, _: &mut AccessCtx<'_>, _: &PropertiesRef<'_>, _: &mut Node) {}
        fn children_ids(&self) -> ChildrenIds {
            ChildrenIds::new()
        }
        fn accepts_focus(&self) -> bool {
            true
        }
        fn on_pointer_event(
            &mut self,
            _: &mut EventCtx<'_>,
            _: &mut PropertiesMut<'_>,
            event: &PointerEvent,
        ) {
            if matches!(event, PointerEvent::Down(_)) {
                self.0.set(self.0.get() + 1);
            }
        }
    }

    fn down(x: f64, y: f64) -> PointerEvent {
        PointerEvent::Down(PointerButtonEvent {
            pointer: PointerInfo {
                pointer_id: Some(PointerId::PRIMARY),
                persistent_device_id: None,
                pointer_type: PointerType::Mouse,
            },
            state: PointerState {
                position: dpi::PhysicalPosition::new(x, y),
                ..Default::default()
            },
            button: Some(PointerButton::Primary),
        })
    }
    fn accept(root: &mut RenderRoot) {
        let (_, geometry) = root
            .record_presented_geometry(GeometryLimits { max_bytes: 65536 })
            .unwrap();
        root.set_input_geometry_source(InputGeometrySource::Presented(Some(Arc::new(geometry))));
    }
    struct Harness {
        root: RenderRoot,
        base: WidgetId,
        clicks: Rc<Cell<usize>>,
        actions: Rc<RefCell<Vec<PopupDismiss>>>,
        redraws: Rc<Cell<usize>>,
    }
    fn setup() -> Harness {
        let clicks = Rc::new(Cell::new(0));
        let base = Content(clicks.clone()).prepare();
        let id = base.id();
        let actions = Rc::new(RefCell::new(Vec::new()));
        let sink = actions.clone();
        let redraws = Rc::new(Cell::new(0));
        let redraw_sink = redraws.clone();
        let root = RenderRoot::new(
            base,
            move |signal| {
                if matches!(signal, RenderRootSignal::RequestRedraw) {
                    redraw_sink.set(redraw_sink.get() + 1);
                }
                if let RenderRootSignal::Action(action, _) = signal
                    && let Some(action) = action.downcast_ref::<PopupDismiss>()
                {
                    sink.borrow_mut().push(*action);
                }
            },
            RenderRootOptions {
                default_properties: Arc::new(DefaultProperties::new()),
                use_system_fonts: false,
                size_policy: WindowSizePolicy::User,
                size: dpi::PhysicalSize::new(200, 150),
                scale_factor: 1.,
                default_font: None,
            },
        );
        Harness {
            root,
            base: id,
            clicks,
            actions,
            redraws,
        }
    }

    #[test]
    fn popup_uses_accepted_geometry_and_restores_focus_after_removal() {
        let Harness {
            mut root,
            base,
            clicks,
            actions,
            ..
        } = setup();
        accept(&mut root);
        assert!(root.focus_on(Some(base)));
        let content = Content(Rc::new(Cell::new(0))).prepare();
        let content_id = content.id();
        let popup = Popup::new(content).prepare();
        let popup_id = popup.id();
        root.add_layer(popup, Point::new(80., 60.));
        accept(&mut root);
        assert!(root.focus_on(Some(content_id)));
        root.handle_pointer_event(down(90., 70.));
        assert!(actions.borrow().is_empty());
        root.reposition_layer(popup_id, Point::new(130., 100.));
        root.handle_pointer_event(down(90., 70.));
        assert!(
            actions.borrow().is_empty(),
            "unaccepted movement must not change outside detection"
        );
        root.handle_pointer_event(down(10., 10.));
        assert_eq!(&*actions.borrow(), &[PopupDismiss::Outside]);
        assert_eq!(
            clicks.get(),
            0,
            "outside press must not activate the scene behind a modal layer"
        );
        root.remove_layer(popup_id);
        accept(&mut root);
        assert_eq!(root.focused_widget(), Some(base));
        assert!(root.get_widget(popup_id).is_none());
        assert!(root.get_widget(content_id).is_none());
        root.handle_pointer_event(down(10., 10.));
        assert_eq!(clicks.get(), 1);
    }

    #[test]
    fn popup_stable_position_does_not_invalidate_layout_or_request_redraw() {
        let Harness {
            mut root, redraws, ..
        } = setup();
        let popup = Popup::new(Content(Rc::new(Cell::new(0))).prepare()).prepare();
        let id = popup.id();
        let position = Point::new(80., 60.);
        root.add_layer(popup, position);
        accept(&mut root);
        redraws.set(0);
        for _ in 0..120 {
            root.reposition_layer(id, position);
        }
        assert_eq!(redraws.get(), 0);
        root.reposition_layer(id, Point::new(81., 60.));
        assert!(redraws.get() > 0);
    }

    #[test]
    fn popup_back_requires_press_release_and_clears_on_focus_loss() {
        let Harness {
            mut root, actions, ..
        } = setup();
        let content = Content(Rc::new(Cell::new(0))).prepare();
        let content_id = content.id();
        root.add_layer(Popup::new(content).prepare(), Point::ORIGIN);
        accept(&mut root);
        root.focus_on(Some(content_id));
        root.handle_text_event(TextEvent::key_up(Key::Named(NamedKey::Escape)));
        assert!(actions.borrow().is_empty());
        root.handle_text_event(TextEvent::key_down(Key::Named(NamedKey::Escape)));
        assert!(actions.borrow().is_empty());
        root.handle_text_event(TextEvent::WindowFocusChange(false));
        root.handle_text_event(TextEvent::WindowFocusChange(true));
        root.handle_text_event(TextEvent::key_up(Key::Named(NamedKey::Escape)));
        assert!(actions.borrow().is_empty());
        root.handle_text_event(TextEvent::key_down(Key::Named(NamedKey::Escape)));
        root.handle_text_event(TextEvent::key_up(Key::Named(NamedKey::Escape)));
        assert_eq!(&*actions.borrow(), &[PopupDismiss::Back]);
        for phase in [
            ActionPhase::Pressed,
            ActionPhase::Cancelled,
            ActionPhase::Released,
        ] {
            root.handle_text_event(TextEvent::Navigation(NavigationEvent {
                action: NavigationAction::Back,
                value: 1.,
                phase,
            }));
        }
        assert_eq!(actions.borrow().len(), 1);
        for phase in [ActionPhase::Pressed, ActionPhase::Released] {
            root.handle_text_event(TextEvent::Navigation(NavigationEvent {
                action: NavigationAction::Back,
                value: 1.,
                phase,
            }));
        }
        assert_eq!(
            &*actions.borrow(),
            &[PopupDismiss::Back, PopupDismiss::Back]
        );
    }

    #[test]
    fn popup_back_pairs_each_input_channel_without_cross_cancellation() {
        let Harness {
            mut root, actions, ..
        } = setup();
        let content = Content(Rc::new(Cell::new(0))).prepare();
        let content_id = content.id();
        root.add_layer(Popup::new(content).prepare(), Point::ORIGIN);
        accept(&mut root);
        root.focus_on(Some(content_id));
        let back = |phase| {
            TextEvent::Navigation(NavigationEvent {
                action: NavigationAction::Back,
                value: 1.,
                phase,
            })
        };
        root.handle_text_event(back(ActionPhase::Pressed));
        root.handle_text_event(TextEvent::key_up(Key::Named(NamedKey::Escape)));
        assert!(actions.borrow().is_empty(), "Escape has no matching press");
        root.handle_text_event(back(ActionPhase::Released));
        assert_eq!(&*actions.borrow(), &[PopupDismiss::Back]);

        root.handle_text_event(TextEvent::key_down(Key::Named(NamedKey::Escape)));
        root.handle_text_event(back(ActionPhase::Released));
        assert_eq!(actions.borrow().len(), 1, "Back has no matching press");
        root.handle_text_event(back(ActionPhase::Cancelled));
        root.handle_text_event(TextEvent::key_up(Key::Named(NamedKey::Escape)));
        assert_eq!(
            actions.borrow().len(),
            2,
            "Back cancellation preserves Escape"
        );

        root.handle_text_event(back(ActionPhase::Pressed));
        root.handle_text_event(TextEvent::key_down(Key::Named(NamedKey::Escape)));
        root.handle_text_event(TextEvent::WindowFocusChange(false));
        root.handle_text_event(TextEvent::WindowFocusChange(true));
        root.handle_text_event(back(ActionPhase::Released));
        root.handle_text_event(TextEvent::key_up(Key::Named(NamedKey::Escape)));
        assert_eq!(actions.borrow().len(), 2, "focus loss clears both channels");
    }

    #[test]
    fn popup_placement_flips_and_clamps_instead_of_resizing_content() {
        let viewport = Rect::new(0., 0., 200., 150.);
        let size = Size::new(70., 40.);
        let anchor = Rect::new(160., 120., 180., 140.);
        assert_eq!(
            PopupPlacement::BottomStart.position(anchor, size, viewport, 4.),
            Point::new(130., 76.)
        );
        assert_eq!(
            PopupPlacement::BottomEnd.position(anchor, size, viewport, 4.),
            Point::new(110., 76.)
        );
        assert_eq!(
            PopupPlacement::RightStart.position(anchor, size, viewport, 4.),
            Point::new(86., 110.)
        );
        assert_eq!(
            PopupPlacement::LeftEnd.position(anchor, size, viewport, 4.),
            Point::new(86., 100.)
        );
        assert_eq!(
            PopupPlacement::TopStart.position(Rect::new(10., 5., 30., 25.), size, viewport, 4.),
            Point::new(10., 29.)
        );
        assert_eq!(
            PopupPlacement::BottomStart.position(anchor, Size::new(300., 200.), viewport, 4.),
            Point::ORIGIN
        );
    }
}
