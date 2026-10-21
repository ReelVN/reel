// Copyright 2025 the Xilem Authors
// SPDX-License-Identifier: Apache-2.0

mod range;
pub use range::{InvalidSliderRange, SliderRange};
use std::any::TypeId;

use accesskit::{ActionData, Node, Orientation, Role};
use tracing::{Span, trace_span};

use crate::core::keyboard::{Key, NamedKey};
use crate::core::pointer::PointerButton;
use crate::core::{
    AccessCtx, AccessEvent, ChildrenIds, EventCtx, LayoutCtx, MeasureCtx, PaintCtx,
    PointerButtonEvent, PointerEvent, PointerUpdate, PrePaintProps, PropertiesMut, PropertiesRef,
    Property, RegisterCtx, TextEvent, Update, UpdateCtx, UsesProperty, Widget, WidgetId, WidgetMut,
    paint_background, paint_box_shadow,
};
use crate::imaging::{Composite, GroupRef, Painter};
use crate::kurbo::{Axis, Circle, Rect, Size, Stroke};
use crate::layout::{LenReq, Length};
use crate::peniko;
use crate::properties::{ThumbColor, ThumbRadius, TrackColor, TrackThickness};
use crate::theme;

/// A widget that allows a user to select a value from a continuous range.
///
/// ![Slider](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAMgAAAAgCAMAAABgkcdDAAAAXVBMVEUpKSknJyoqKy46etsxTXgpKirJ2O7y8vLC1O7x8fGQtOrB0+5OiuV+qenl6vErOU8vSnQpKSo6etouPlcoKCo4bL04bb40XJkuP1k7fuM0W5cqLTI3aLRFhOQ7fuSVzae6AAAAgklEQVR42u3WRw6AMAwEQFND7x2y/38mQogjB+CSoJ0fWLK9K0RERPTKNA7NME5iuXWZcZiXVWwWbNBJWqSJxhaIxXzoUB1CDV/s5XqI1CmC54pB2h6PZLE6xRlKxxRdLT2eydUlR+UYo/vNIL9ZrW/HzvfLQGRFuS+NbM9ERETm2QFJkiLhTZDb1gAAAABJRU5ErkJggg==)
pub struct Slider {
    range: SliderRange,
    value: f64,
    decorated: bool,
}
impl Slider {
    /// Creates a slider with a finite, increasing range.
    pub fn new(min: f64, max: f64, value: f64) -> Self {
        let range = SliderRange::new(min, max, None).expect("invalid slider range");
        assert!(value.is_finite(), "slider value must be finite");
        Self {
            range,
            value: value.clamp(min, max),
            decorated: true,
        }
    }
    pub fn with_step(mut self, step: f64) -> Self {
        self.range = SliderRange::new(self.range.min(), self.range.max(), Some(step))
            .expect("invalid slider step");
        self
    }
    /// Keep the control's input and accessibility while its parent paints a skin.
    pub fn with_decorated(mut self, decorated: bool) -> Self {
        self.decorated = decorated;
        self
    }
    pub fn set_decorated(this: &mut WidgetMut<'_, Self>, decorated: bool) {
        if this.widget.decorated != decorated {
            this.widget.decorated = decorated;
            this.ctx.request_render();
        }
    }
    pub fn value(&self) -> f64 {
        self.value
    }
    pub fn range(&self) -> SliderRange {
        self.range
    }
    /// Apply authoritative state without emitting a user action.
    pub fn configure(this: &mut WidgetMut<'_, Self>, range: SliderRange, value: f64) {
        assert!(range.contains(value), "slider value outside range");
        if this.widget.range != range {
            this.widget.range = range;
            this.ctx.invalidate_input_configuration();
        }
        if this.widget.value != value {
            this.widget.value = value;
            this.ctx.request_render();
        }
    }
    pub fn set_value(this: &mut WidgetMut<'_, Self>, value: f64) {
        assert!(value.is_finite(), "slider value must be finite");
        let range = this.widget.range;
        Self::configure(this, range, value.clamp(range.min(), range.max()));
    }
    pub fn set_step(this: &mut WidgetMut<'_, Self>, step: Option<f64>) {
        let range = SliderRange::new(this.widget.range.min(), this.widget.range.max(), step)
            .expect("invalid slider step");
        Self::configure(this, range, this.widget.value);
    }
    pub fn set_range(this: &mut WidgetMut<'_, Self>, min: f64, max: f64) {
        let range =
            SliderRange::new(min, max, this.widget.range.step()).expect("invalid slider range");
        Self::configure(this, range, this.widget.value.clamp(min, max));
    }
    fn change(&mut self, ctx: &mut EventCtx<'_>, value: f64) {
        if value.is_finite() && value != self.value {
            self.value = value;
            ctx.request_render();
            ctx.submit_action::<SliderMoved>(SliderMoved { value });
        }
    }
}

impl UsesProperty<TrackThickness> for Slider {}
impl UsesProperty<TrackColor> for Slider {}
impl UsesProperty<ThumbColor> for Slider {}
impl UsesProperty<ThumbRadius> for Slider {}

/// A slider was moved.
#[derive(PartialEq, Debug)]
pub struct SliderMoved {
    /// The new value of the slider.
    pub value: f64,
}

// --- MARK: IMPL WIDGET
impl Widget for Slider {
    type Action = SliderMoved;

    fn accepts_focus(&self) -> bool {
        true
    }

    fn on_pointer_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        props: &mut PropertiesMut<'_>,
        event: &PointerEvent,
    ) {
        if ctx.is_disabled() {
            return;
        }
        let position = match event {
            PointerEvent::Down(PointerButtonEvent {
                button: Some(PointerButton::Primary) | None,
                state,
                ..
            }) => {
                if !ctx.input_configuration_is_current() {
                    return;
                }
                ctx.request_focus();
                ctx.capture_pointer();
                Some(state.position)
            }
            PointerEvent::Move(PointerUpdate { current, .. }) if ctx.is_active() => {
                Some(current.position)
            }
            PointerEvent::Up(_) if ctx.is_active() => None,
            _ => None,
        };
        if let Some(position) = position
            && ctx.input_configuration_is_current()
        {
            let x = ctx.local_position(position).x;
            let radius = props.get::<ThumbRadius>(ctx.property_cache()).0.get();
            let (a, b) = range::endpoints(ctx.content_box().width(), radius);
            if b > a {
                self.change(ctx, self.range.at_progress((x - a) / (b - a)));
            }
        }
    }
    fn on_text_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        props: &mut PropertiesMut<'_>,
        event: &TextEvent,
    ) {
        if ctx.is_disabled() || !ctx.is_focus_target() {
            return;
        }
        if let TextEvent::Navigation(event) = event {
            use crate::input::{ActionPhase, NavigationAction};
            let action = match event.action {
                NavigationAction::Left => accesskit::Action::Decrement,
                NavigationAction::Right => accesskit::Action::Increment,
                _ => return,
            };
            if matches!(event.phase, ActionPhase::Pressed | ActionPhase::Repeated) {
                self.on_access_event(ctx, props, &AccessEvent { action, data: None });
            }
            ctx.set_handled();
        }
        if let TextEvent::Keyboard(event) = event {
            if event.state.is_up() {
                return;
            }
            let step = if event.modifiers.shift() { 10. } else { 1. };
            let value = match &event.key {
                Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowDown) => {
                    self.range.increment(self.value, -step)
                }
                Key::Named(NamedKey::ArrowRight | NamedKey::ArrowUp) => {
                    self.range.increment(self.value, step)
                }
                Key::Named(NamedKey::Home) => self.range.min(),
                Key::Named(NamedKey::End) => self.range.max(),
                _ => return,
            };
            ctx.set_handled();
            if ctx.input_configuration_is_current() {
                self.change(ctx, value);
            }
        }
    }
    fn update(&mut self, ctx: &mut UpdateCtx<'_>, _props: &mut PropertiesMut<'_>, event: &Update) {
        match event {
            Update::FocusChanged(_) | Update::HoveredChanged(_) | Update::ActiveChanged(_) => {
                ctx.request_render();
            }
            _ => {}
        }
    }

    fn on_access_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        event: &AccessEvent,
    ) {
        if ctx.is_disabled() {
            return;
        }
        let value = match event.action {
            accesskit::Action::Increment => self.range.increment(self.value, 1.),
            accesskit::Action::Decrement => self.range.increment(self.value, -1.),
            accesskit::Action::SetValue => match &event.data {
                Some(ActionData::NumericValue(value)) => *value,
                Some(ActionData::Value(value)) => match value.parse::<f64>() {
                    Ok(value) => value,
                    Err(_) => return,
                },
                _ => return,
            },
            _ => return,
        };
        ctx.set_handled();
        if value.is_finite() && ctx.input_configuration_is_current() {
            self.change(ctx, self.range.snap(value));
        }
    }

    fn register_children(&mut self, _ctx: &mut RegisterCtx<'_>) {}

    fn property_changed(&mut self, ctx: &mut UpdateCtx<'_>, property_type: TypeId) {
        TrackThickness::prop_changed(ctx, property_type);
        ThumbColor::prop_changed(ctx, property_type);
        ThumbRadius::prop_changed(ctx, property_type);
        if ThumbRadius::matches(property_type) {
            ctx.invalidate_input_configuration();
        }
        if TrackColor::matches(property_type) {
            ctx.request_paint_only();
        }
    }

    fn measure(
        &mut self,
        ctx: &mut MeasureCtx<'_>,
        props: &PropertiesRef<'_>,
        axis: Axis,
        len_req: LenReq,
        _cross_length: Option<Length>,
    ) -> Length {
        match axis {
            Axis::Horizontal => match len_req {
                // TODO: Move this 100. to theme?
                LenReq::MinContent | LenReq::MaxContent => Length::const_px(100.),
                LenReq::FitContent(space) => space,
            },
            Axis::Vertical => {
                let cache = ctx.property_cache();
                let thumb_radius = props.get::<ThumbRadius>(cache);
                let track_thickness = props.get::<TrackThickness>(cache);

                let thumb_length = thumb_radius.0.saturating_add(thumb_radius.0);
                let track_length = track_thickness.0;
                let padding_length = theme::WIDGET_CONTROL_COMPONENT_PADDING;

                thumb_length
                    .max(track_length)
                    .saturating_add(padding_length)
            }
        }
    }

    fn layout(&mut self, _ctx: &mut LayoutCtx<'_>, _props: &PropertiesRef<'_>, _size: Size) {}

    fn pre_paint(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        props: &PropertiesRef<'_>,
        painter: &mut Painter<'_>,
    ) {
        if !self.decorated {
            return;
        }
        let bbox = ctx.border_box();
        let cache = ctx.property_cache();
        let p = PrePaintProps::fetch(props, cache);

        paint_box_shadow(painter, bbox, p.box_shadow, p.corner_radius);
        paint_background(painter, bbox, p.background, p.border_width, p.corner_radius);

        if ctx.is_focus_target() || ctx.is_hovered() {
            // TODO: Replace this custom implementation with the general paint_border()

            let focus_rect = bbox.inset(2.);

            let focus_color = p.border_color.color;
            let focus_width = 2.;
            let focus_radius = 4.;

            let focus_path = focus_rect.to_rounded_rect(focus_radius);
            let focus_stroke = Stroke::new(focus_width).with_miter_limit(10.);

            painter
                .stroke(focus_path, &focus_stroke, focus_color)
                .draw();
        }
    }

    fn paint(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        props: &PropertiesRef<'_>,
        painter: &mut Painter<'_>,
    ) {
        if !self.decorated {
            return;
        }
        // Get parameters and resolve colors

        let cache = ctx.property_cache();
        let track_color = props.get::<TrackColor>(cache);
        let thumb_color = props.get::<ThumbColor>(cache).0;
        let track_thickness = props.get::<TrackThickness>(cache).0.get();
        let thumb_radius = props.get::<ThumbRadius>(cache).0.get();
        let thumb_border_width = if ctx.is_active() {
            2.5
        } else if ctx.is_hovered() {
            1.5
        } else {
            2.
        };

        // Calculate geometry based on state
        let size = ctx.content_box().size();
        let track_y = (size.height - track_thickness) / 2.0;
        let border_box = ctx.border_box();

        // TODO: replace with proper disabled colors
        // Push semitransparent layer if disabled
        if ctx.is_disabled() {
            const DISABLED_ALPHA: f32 = 0.4;
            // Paint through a semitransparent isolated group when disabled.
            painter.push_fill_clip(border_box);
            painter.push_group(
                GroupRef::new()
                    .with_composite(Composite::new(peniko::BlendMode::default(), DISABLED_ALPHA)),
            );
        }

        let progress = self.range.progress(self.value);
        let (a, b) = range::endpoints(size.width, thumb_radius);
        let thumb_x = a + progress * (b - a);
        let track_rect = Rect::new(0., track_y, size.width, track_y + track_thickness)
            .to_rounded_rect(track_thickness / 2.);
        // Two clipped solids express the hard color boundary directly, including
        // translucent colors, without requiring a gradient-capable renderer.
        for (left, right, color) in [
            (0., thumb_x, track_color.active),
            (thumb_x, size.width, track_color.inactive),
        ] {
            painter.push_fill_clip(Rect::new(left, 0., right, size.height));
            painter.fill(track_rect, color).draw();
            painter.pop_clip();
        }

        // Paint thumb
        let thumb_y = size.height / 2.;
        let thumb_circle = Circle::new((thumb_x, thumb_y), thumb_radius - thumb_border_width / 2.);

        painter.fill(thumb_circle, thumb_color).draw();
        painter
            .stroke(
                thumb_circle,
                &Stroke::new(thumb_border_width),
                track_color.active,
            )
            .draw();

        // Pop the semitransparent layer
        if ctx.is_disabled() {
            painter.pop_group();
            painter.pop_clip();
        }
    }

    fn accessibility_role(&self) -> Role {
        Role::Slider
    }

    fn accessibility(
        &mut self,
        _ctx: &mut AccessCtx<'_>,
        _props: &PropertiesRef<'_>,
        node: &mut Node,
    ) {
        node.set_orientation(Orientation::Horizontal);
        node.set_value(self.value.to_string());
        node.set_numeric_value(self.value);
        node.set_min_numeric_value(self.range.min());
        node.set_max_numeric_value(self.range.max());
        if let Some(step) = self.range.step() {
            node.set_numeric_value_step(step);
        }
        node.add_action(accesskit::Action::SetValue);
        node.add_action(accesskit::Action::Increment);
        node.add_action(accesskit::Action::Decrement);
    }

    fn children_ids(&self) -> ChildrenIds {
        ChildrenIds::new()
    }

    fn make_trace_span(&self, id: WidgetId) -> Span {
        trace_span!("Slider", id = id.trace())
    }
}

// --- MARK: TESTS
