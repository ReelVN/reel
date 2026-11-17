// Copyright 2026 the Reel Authors
// SPDX-License-Identifier: Apache-2.0
//! Scrollbar geometry and input share one continuous logical scroll range.
use super::{ScrollAxis, SliderRange, scroll_offset_by, snap_scroll_offset, wheel_delta_pixels};
use crate::{
    accesskit::{Action, ActionData},
    core::{
        AccessEvent, EventCtx, PointerEvent, TextEvent,
        keyboard::{Key, NamedKey},
        pointer::PointerButton,
    },
    input::{ActionPhase, NavigationAction},
    kurbo::{Affine, Point, Rect, Size},
};

/// Author configuration and measured target data for one scrollbar axis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollBarView {
    pub axis: ScrollAxis,
    pub value: f64,
    pub viewport: f64,
    pub content: f64,
    pub min_thumb_size: f64,
    pub thumb_size: Option<f64>,
    pub step: f64,
    pub snap: Option<f64>,
    pub inset_start: f64,
    pub inset_end: f64,
    pub live: bool,
}
impl Default for ScrollBarView {
    fn default() -> Self {
        Self {
            axis: ScrollAxis::Vertical,
            value: 0.,
            viewport: 0.,
            content: 0.,
            min_thumb_size: 16.,
            thumb_size: None,
            step: 40.,
            snap: None,
            inset_start: 0.,
            inset_end: 0.,
            live: false,
        }
    }
}
impl ScrollBarView {
    pub fn maximum(self) -> f64 {
        (self.content - self.viewport).max(0.)
    }
    pub fn range(self) -> Option<SliderRange> {
        self.live
            .then(|| SliderRange::new(0., self.maximum(), None).ok())
            .flatten()
    }
    pub fn clamp(self, value: f64) -> f64 {
        snap_scroll_offset(value, self.maximum(), self.snap)
    }
    pub fn shifted(self, delta: f64) -> f64 {
        scroll_offset_by(self.value, delta, self.maximum(), self.snap)
    }
    /// Equality of the accepted configuration, excluding the logical offset.
    pub fn same_geometry(self, mut other: Self) -> bool {
        other.value = self.value;
        self == other
    }
    /// Whether the drag may continue across changes to the measured range and offset.
    pub fn same_controls(self, other: Self) -> bool {
        Self {
            viewport: other.viewport,
            content: other.content,
            ..self
        }
        .same_geometry(other)
    }
    pub fn major(self, point: Point) -> f64 {
        if self.axis == ScrollAxis::Horizontal {
            point.x
        } else {
            point.y
        }
    }
    pub fn thumb_rect(self, size: Size) -> Rect {
        let horizontal = self.axis == ScrollAxis::Horizontal;
        let extent = if horizontal { size.width } else { size.height };
        let start = self.inset_start.min(extent);
        let length = (extent - start - self.inset_end).max(0.);
        let proportional = if self.content > 0. {
            length * (self.viewport / self.content).clamp(0., 1.)
        } else {
            length
        };
        let thumb = self
            .thumb_size
            .unwrap_or(proportional.max(self.min_thumb_size))
            .clamp(0., length);
        let position = if self.maximum() > 0. {
            self.value.clamp(0., self.maximum()) / self.maximum() * (length - thumb)
        } else {
            0.
        };
        if horizontal {
            Rect::new(start + position, 0., start + position + thumb, size.height)
        } else {
            Rect::new(0., start + position, size.width, start + position + thumb)
        }
    }
}

/// Transient capture stays in the native input tree and is rebuilt after restore.
#[derive(Default)]
pub struct ScrollBarState {
    view: ScrollBarView,
    grab: Option<ScrollBarGrab>,
    size: Size,
    origin: Point,
}
#[derive(Clone, Copy)]
struct ScrollBarGrab {
    offset: f64,
    last_coordinate: f64,
    track: Rect,
    thumb_size: f64,
    window: Affine,
    rebased: bool,
}
impl ScrollBarState {
    pub fn new(view: ScrollBarView) -> Self {
        Self {
            view,
            ..Default::default()
        }
    }
    pub fn view(&self) -> ScrollBarView {
        self.view
    }
    pub fn configure(&mut self, view: ScrollBarView) -> bool {
        let changed = !self.view.same_geometry(view);
        if !self.view.same_controls(view) {
            self.cancel();
        } else if self.view != view
            && let Some(grab) = &mut self.grab
        {
            grab.rebased = true;
        }
        self.view = view;
        changed
    }
    pub fn layout(&mut self, size: Size) {
        self.layout_rect(size.to_rect());
    }
    pub fn layout_rect(&mut self, rect: Rect) {
        let size = rect.size();
        if self.origin != rect.origin() {
            self.cancel();
            self.origin = rect.origin();
        }
        if self.size != size {
            self.cancel();
            self.size = size;
        }
    }
    pub fn cancel(&mut self) {
        self.grab = None;
    }
    pub fn thumb_rect(&self) -> Rect {
        self.view.thumb_rect(self.size) + self.origin.to_vec2()
    }
    fn change(&mut self, value: f64) -> Option<f64> {
        if !value.is_finite() || self.view.range().is_none() {
            return None;
        }
        let value = self.view.clamp(value);
        if value == self.view.value {
            return None;
        }
        self.view.value = value;
        Some(value)
    }
    #[cfg(test)]
    fn at_pointer(&mut self, coordinate: f64, grab: f64) -> Option<f64> {
        let rect = self.thumb_rect();
        self.map_pointer(
            coordinate,
            grab,
            Rect::from_origin_size(self.origin, self.size),
            if self.view.axis == ScrollAxis::Horizontal {
                rect.width()
            } else {
                rect.height()
            },
        )
    }
    fn map_pointer(&mut self, coordinate: f64, grab: f64, track: Rect, thumb: f64) -> Option<f64> {
        let (start, travel) = self.travel(track, thumb);
        if travel == 0. {
            return None;
        }
        self.change(((coordinate - start - grab) / travel).clamp(0., 1.) * self.view.maximum())
    }
    fn travel(&self, track: Rect, thumb: f64) -> (f64, f64) {
        let extent = if self.view.axis == ScrollAxis::Horizontal {
            track.width()
        } else {
            track.height()
        };
        let start = self.view.inset_start.min(extent);
        (
            self.view.major(track.origin()) + start,
            (extent - start - self.view.inset_end - thumb).max(0.),
        )
    }
    fn pointer_geometry(
        &self,
        ctx: &EventCtx<'_>,
        parts: [crate::core::WidgetId; 2],
    ) -> Option<(Rect, Rect, Affine)> {
        match &ctx.global_state.input_geometry_source {
            crate::app::InputGeometrySource::Live => Some((
                Rect::from_origin_size(self.origin, self.size),
                self.thumb_rect(),
                Affine::IDENTITY,
            )),
            crate::app::InputGeometrySource::Presented(Some(view)) => {
                let parent = view.get(ctx.widget_id())?;
                let local = parent.window_transform.inverse();
                let rect = |id| {
                    let child = view.get(id)?;
                    Some((local * child.window_transform).transform_rect_bbox(child.size.to_rect()))
                };
                Some((rect(parts[0])?, rect(parts[1])?, parent.window_transform))
            }
            crate::app::InputGeometrySource::Presented(None) => None,
        }
    }
    fn shift(&mut self, delta: f64) -> Option<f64> {
        if !delta.is_finite() {
            return None;
        }
        self.change(self.view.shifted(delta))
    }
    pub fn pointer(
        &mut self,
        ctx: &mut EventCtx<'_>,
        event: &PointerEvent,
        parts: [crate::core::WidgetId; 2],
    ) -> Option<f64> {
        if ctx.is_disabled() || self.view.range().is_none() {
            self.cancel();
            if ctx.is_active() {
                ctx.release_pointer();
            }
            return None;
        }
        match event {
            PointerEvent::Down(event)
                if event.button.is_none_or(|b| b == PointerButton::Primary) =>
            {
                if !ctx.input_configuration_is_current() {
                    return None;
                }
                let point = ctx.local_position(event.state.position);
                let (track, rect, window) = self.pointer_geometry(ctx, parts)?;
                let coordinate = self.view.major(point);
                let (start, length) = if self.view.axis == ScrollAxis::Horizontal {
                    (rect.x0, rect.width())
                } else {
                    (rect.y0, rect.height())
                };
                let on_thumb = rect.contains(point);
                let grab = if on_thumb {
                    coordinate - start
                } else {
                    length * 0.5
                };
                self.grab = Some(ScrollBarGrab {
                    offset: grab,
                    last_coordinate: coordinate,
                    track,
                    thumb_size: length,
                    window,
                    rebased: false,
                });
                ctx.request_focus();
                ctx.capture_pointer();
                ctx.set_handled();
                if on_thumb {
                    None
                } else {
                    self.map_pointer(coordinate, grab, track, length)
                }
            }
            PointerEvent::Move(event) if ctx.is_active() => {
                if !ctx.input_configuration_is_current() {
                    return None;
                }
                let Some(mut grab) = self.grab else {
                    ctx.release_pointer();
                    return None;
                };
                let (track, thumb, window) = self.pointer_geometry(ctx, parts)?;
                let length = if self.view.axis == ScrollAxis::Horizontal {
                    thumb.width()
                } else {
                    thumb.height()
                };
                // Affine round trips can change the low bits of a translated
                // rectangle's extent without changing its presented geometry.
                let near = |a: f64, b: f64| (a - b).abs() <= 1e-6;
                if ![track.x0, track.y0, track.x1, track.y1]
                    .into_iter()
                    .zip([grab.track.x0, grab.track.y0, grab.track.x1, grab.track.y1])
                    .all(|(a, b)| near(a, b))
                    || window != grab.window
                {
                    self.cancel();
                    ctx.release_pointer();
                    return None;
                }
                ctx.set_handled();
                let coordinate = self.view.major(ctx.local_position(event.current.position));
                let delta = coordinate - grab.last_coordinate;
                grab.last_coordinate = coordinate;
                grab.rebased |= !near(length, grab.thumb_size);
                self.grab = Some(grab);
                if !grab.rebased {
                    return self.map_pointer(coordinate, grab.offset, track, length);
                }
                let (start, travel) = self.travel(track, length);
                if delta == 0. || travel == 0. {
                    return None;
                }
                let offset = if grab.thumb_size > 0. {
                    length * grab.offset / grab.thumb_size
                } else {
                    0.
                };
                let position = coordinate - start - offset;
                if position <= 0. {
                    self.change(0.)
                } else if position >= travel {
                    self.change(self.view.maximum())
                } else {
                    self.shift(delta * self.view.maximum() / travel)
                }
            }
            PointerEvent::Up(_) | PointerEvent::Cancel(_) => {
                if self.grab.take().is_some() {
                    ctx.set_handled();
                }
                None
            }
            PointerEvent::Scroll(event) if ctx.input_configuration_is_current() => {
                let viewport = if self.view.axis == ScrollAxis::Horizontal {
                    Size::new(self.view.viewport, 0.)
                } else {
                    Size::new(0., self.view.viewport)
                };
                let delta = wheel_delta_pixels(event.delta, ctx.scale_factor(), viewport);
                let amount = if self.view.axis == ScrollAxis::Horizontal {
                    if delta.x == 0. { delta.y } else { delta.x }
                } else {
                    delta.y
                };
                let changed = self.shift(amount);
                if changed.is_some() {
                    self.cancel();
                    ctx.set_handled();
                }
                changed
            }
            _ => None,
        }
    }
    pub fn text(&mut self, ctx: &mut EventCtx<'_>, event: &TextEvent) -> Option<f64> {
        if ctx.is_disabled() || !ctx.is_focus_target() || !ctx.input_configuration_is_current() {
            return None;
        }
        let horizontal = self.view.axis == ScrollAxis::Horizontal;
        let delta = match event {
            TextEvent::Navigation(e)
                if matches!(e.phase, ActionPhase::Pressed | ActionPhase::Repeated) =>
            {
                match e.action {
                    NavigationAction::Left if horizontal => Some(-self.view.step),
                    NavigationAction::Right if horizontal => Some(self.view.step),
                    NavigationAction::Up if !horizontal => Some(-self.view.step),
                    NavigationAction::Down if !horizontal => Some(self.view.step),
                    _ => None,
                }
            }
            TextEvent::Keyboard(e) if !e.state.is_up() => match &e.key {
                Key::Named(NamedKey::ArrowLeft) if horizontal => Some(-self.view.step),
                Key::Named(NamedKey::ArrowRight) if horizontal => Some(self.view.step),
                Key::Named(NamedKey::ArrowUp) if !horizontal => Some(-self.view.step),
                Key::Named(NamedKey::ArrowDown) if !horizontal => Some(self.view.step),
                Key::Named(NamedKey::PageUp) => Some(-self.view.viewport),
                Key::Named(NamedKey::PageDown) => Some(self.view.viewport),
                Key::Named(NamedKey::Home) => Some(-self.view.value),
                Key::Named(NamedKey::End) => Some(self.view.maximum() - self.view.value),
                _ => None,
            },
            _ => None,
        }?;
        ctx.set_handled();
        self.cancel();
        self.shift(delta)
    }
    pub fn access(&mut self, ctx: &mut EventCtx<'_>, event: &AccessEvent) -> Option<f64> {
        if ctx.is_disabled() || !ctx.input_configuration_is_current() {
            return None;
        }
        let value = match event.action {
            Action::Increment => self.view.shifted(self.view.step),
            Action::Decrement => self.view.shifted(-self.view.step),
            Action::SetValue => match &event.data {
                Some(ActionData::NumericValue(v)) => *v,
                Some(ActionData::Value(v)) => v.parse().ok()?,
                _ => return None,
            },
            _ => return None,
        };
        ctx.set_handled();
        self.cancel();
        self.change(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn proportional_and_fixed_thumbs_share_inverse_mapping_and_empty_ranges() {
        let mut state = ScrollBarState::new(ScrollBarView {
            live: true,
            viewport: 100.,
            content: 500.,
            ..Default::default()
        });
        state.layout(Size::new(20., 200.));
        assert_eq!(state.thumb_rect(), Rect::new(0., 0., 20., 40.));
        assert_eq!(state.at_pointer(180., 20.), Some(400.));
        assert_eq!(state.thumb_rect(), Rect::new(0., 160., 20., 200.));
        let fixed = ScrollBarView {
            thumb_size: Some(52.),
            inset_start: 13.,
            inset_end: 5.,
            ..state.view()
        };
        assert_eq!(
            fixed.thumb_rect(Size::new(35., 600.)),
            Rect::new(0., 543., 35., 595.)
        );
        let empty = ScrollBarView {
            content: 100.,
            viewport: 100.,
            ..fixed
        };
        assert!(empty.range().is_none());
        assert_eq!(
            empty.thumb_rect(Size::new(35., 600.)),
            Rect::new(0., 13., 35., 65.)
        );
        assert_eq!(fixed.thumb_rect(Size::new(35., 10.)).height(), 0.);
    }
}
