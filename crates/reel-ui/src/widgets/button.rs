// Copyright 2018 the Xilem Authors and the Druid Authors
// SPDX-License-Identifier: Apache-2.0

use std::any::TypeId;
use std::sync::Arc;

use accesskit::{Node, Role};
use tracing::{Span, trace, trace_span};

use crate::core::MeasureCtx;
use crate::core::keyboard::{Key, NamedKey};
use crate::core::pointer::PointerButton;
use crate::core::{
    AccessCtx, AccessEvent, ChildrenIds, EventCtx, LayoutCtx, NewWidget, PaintCtx,
    PointerButtonEvent, PointerEvent, PropertiesMut, PropertiesRef, RegisterCtx, TextEvent, Update,
    UpdateCtx, Widget, WidgetId, WidgetMut, WidgetPod,
};
use crate::imaging::Painter;
use crate::kurbo::{Axis, Size};
use crate::layout::{LayoutSize, LenReq, Length, SizeDef};
use crate::theme;
use crate::widgets::Label;

/// A button with a child widget.
///
/// Emits [`ButtonPress`] when pressed.
///
#[doc = include_str!("../doc/widgets/button.md")]
pub struct Button {
    child: WidgetPod<dyn Widget>,
    behavior: ButtonBehavior,
}

// --- MARK: BUILDERS
impl Button {
    /// Creates a new button with a child widget.
    ///
    /// The child widget probably shouldn't be interactive,
    /// to avoid behaviour which might be confusing to the user.
    ///
    /// # Examples
    ///
    /// ```
    /// use reel_ui::widgets::{Button, Label};
    /// use reel_ui::core::Widget;
    ///
    /// let button = Button::new(Label::new("Increment").prepare());
    /// ```
    pub fn new(child: NewWidget<impl Widget + ?Sized>) -> Self {
        Self {
            child: child.erased().to_pod(),
            behavior: ButtonBehavior::default(),
        }
    }

    /// Creates a new button with a label widget.
    ///
    /// # Examples
    ///
    /// ```
    /// use reel_ui::widgets::Button;
    /// use reel_ui::core::Widget;
    ///
    /// let button = Button::with_text("Increment");
    /// ```
    pub fn with_text(text: impl Into<Arc<str>>) -> Self {
        Self::new(Label::new(text).prepare())
    }
}

// --- MARK: WIDGETMUT
impl Button {
    /// Replaces the child widget with a new one.
    pub fn set_child(this: &mut WidgetMut<'_, Self>, child: NewWidget<impl Widget + ?Sized>) {
        this.ctx.remove_child(std::mem::replace(
            &mut this.widget.child,
            child.erased().to_pod(),
        ));
    }

    /// Returns a mutable reference to the child.
    pub fn child_mut<'t>(this: &'t mut WidgetMut<'_, Self>) -> Option<WidgetMut<'t, dyn Widget>> {
        this.ctx.get_mut(&mut this.widget.child)
    }
}

/// A button was pressed.
#[derive(PartialEq, Debug)]
pub struct ButtonPress {
    /// The pointer button that has been pressed.
    ///
    /// Can be `None` when using for example the keyboard or a touch screen.
    pub button: Option<PointerButton>,
}

/// Shared activation behavior for buttons with independently owned content.
/// Capture, focus and disabled state stay in the ordinary widget/input system.
#[derive(Default)]
pub struct ButtonBehavior {
    pressed_key: Option<ActivationKey>,
    pressed_navigation: bool,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum ActivationKey {
    Enter,
    Space,
}
impl ButtonBehavior {
    /// Keyboard/controller activation remains held until release or cancellation.
    pub fn is_pressed(&self) -> bool {
        self.pressed_key.is_some() || self.pressed_navigation
    }
    pub fn pointer(ctx: &mut EventCtx<'_>, event: &PointerEvent) -> Option<ButtonPress> {
        match event {
            PointerEvent::Down(e) if e.button.is_none_or(|b| b == PointerButton::Primary) => {
                ctx.request_focus();
                ctx.capture_pointer();
                ctx.request_paint_only();
                trace!("Button {:?} pressed", ctx.widget_id());
            }
            PointerEvent::Up(PointerButtonEvent { button, .. })
                if button.is_none_or(|b| b == PointerButton::Primary) =>
            {
                let press = (ctx.is_active() && ctx.is_hovered())
                    .then_some(ButtonPress { button: *button });
                ctx.request_paint_only();
                // The shared event pass releases capture after terminal bubbling.
                // Ancestors may still need the real terminal event for cleanup.
                return press;
            }
            _ => {}
        }
        None
    }
    pub fn text(&mut self, ctx: &mut EventCtx<'_>, event: &TextEvent) -> Option<ButtonPress> {
        if let TextEvent::Navigation(event) = event
            && event.action == crate::input::NavigationAction::Confirm
        {
            use crate::input::ActionPhase;
            ctx.set_handled();
            match event.phase {
                ActionPhase::Pressed => self.pressed_navigation = true,
                ActionPhase::Released => {
                    return std::mem::take(&mut self.pressed_navigation)
                        .then_some(ButtonPress { button: None });
                }
                ActionPhase::Cancelled => self.pressed_navigation = false,
                _ => {}
            }
        }
        if matches!(event, TextEvent::WindowFocusChange(false)) {
            self.pressed_key = None;
            self.pressed_navigation = false;
        }
        if let TextEvent::Keyboard(event) = event {
            let key = match &event.key {
                Key::Named(NamedKey::Enter) => ActivationKey::Enter,
                Key::Character(c) if c == " " => ActivationKey::Space,
                _ => return None,
            };
            ctx.set_handled();
            if event.state.is_up() {
                return (self.pressed_key.take() == Some(key))
                    .then_some(ButtonPress { button: None });
            }
            if !event.repeat {
                self.pressed_key = Some(key);
            }
        }
        None
    }
    pub fn update(&mut self, event: &Update) {
        if matches!(
            event,
            Update::FocusChanged(false)
                | Update::WindowFocusChanged(false)
                | Update::DisabledChanged(true)
                | Update::StashedChanged(true)
        ) {
            self.pressed_key = None;
            self.pressed_navigation = false;
        }
    }
    pub fn access(event: &AccessEvent) -> Option<ButtonPress> {
        (event.action == accesskit::Action::Click).then_some(ButtonPress { button: None })
    }
}

// --- MARK: IMPL WIDGET
impl Widget for Button {
    type Action = ButtonPress;

    fn on_pointer_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        event: &PointerEvent,
    ) {
        if let Some(press) = ButtonBehavior::pointer(ctx, event) {
            ctx.submit_action::<Self::Action>(press);
        }
    }

    fn on_text_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        event: &TextEvent,
    ) {
        if let Some(press) = self.behavior.text(ctx, event) {
            ctx.submit_action::<Self::Action>(press);
        }
    }

    fn on_access_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        event: &AccessEvent,
    ) {
        if let Some(press) = ButtonBehavior::access(event) {
            ctx.submit_action::<Self::Action>(press);
        }
    }

    fn update(&mut self, _ctx: &mut UpdateCtx<'_>, _props: &mut PropertiesMut<'_>, event: &Update) {
        self.behavior.update(event);
    }

    fn register_children(&mut self, ctx: &mut RegisterCtx<'_>) {
        ctx.register_child(&mut self.child);
    }

    fn property_changed(&mut self, _ctx: &mut UpdateCtx<'_>, _property_type: TypeId) {}

    fn measure(
        &mut self,
        ctx: &mut MeasureCtx<'_>,
        _props: &PropertiesRef<'_>,
        axis: Axis,
        len_req: LenReq,
        cross_length: Option<Length>,
    ) -> Length {
        let auto_length = len_req.into();
        let context_size = LayoutSize::maybe(axis.cross(), cross_length);

        let child_length = ctx.compute_length(
            &mut self.child,
            auto_length,
            context_size,
            axis,
            cross_length,
        );

        let length = child_length;

        // TODO - Add MinimumSize property.
        // HACK: to make sure we look okay at default sizes when beside a text input,
        // we make sure we will have at least the same height as the default text input.
        match axis {
            Axis::Horizontal => length,
            Axis::Vertical => length.max(theme::BASIC_WIDGET_HEIGHT),
        }
    }

    fn layout(&mut self, ctx: &mut LayoutCtx<'_>, _props: &PropertiesRef<'_>, size: Size) {
        let child_size = ctx.compute_size(&mut self.child, SizeDef::fit(size), size.into());
        ctx.run_layout(&mut self.child, child_size);

        let child_origin = ((size - child_size).to_vec2() * 0.5).to_point();
        ctx.place_child(&mut self.child, child_origin);

        ctx.derive_baselines(&self.child);
    }

    fn paint(
        &mut self,
        _ctx: &mut PaintCtx<'_>,
        _props: &PropertiesRef<'_>,
        _painter: &mut Painter<'_>,
    ) {
    }

    fn accessibility_role(&self) -> Role {
        Role::Button
    }

    fn accessibility(
        &mut self,
        _ctx: &mut AccessCtx<'_>,
        _props: &PropertiesRef<'_>,
        node: &mut Node,
    ) {
        node.add_action(accesskit::Action::Click);
    }

    fn children_ids(&self) -> ChildrenIds {
        ChildrenIds::from_slice(&[self.child.id()])
    }

    fn propagates_pointer_interaction(&self) -> bool {
        false
    }

    fn accepts_focus(&self) -> bool {
        // Buttons can be tab-focused...
        true
    }

    fn accepts_text_input(&self) -> bool {
        // But they still aren't text areas.
        false
    }

    fn make_trace_span(&self, id: WidgetId) -> Span {
        trace_span!("Button", id = id.trace())
    }
}

// --- MARK: TESTS
