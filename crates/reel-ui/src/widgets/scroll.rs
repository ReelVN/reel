// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0
//! Shared scroll geometry and gesture ownership for native and authored views.
use crate::core::{EventCtx, PointerEvent, PointerInfo, PointerType, ScrollDelta, pointer_info};
use crate::kurbo::{Point, Rect, Size, Vec2};

/// Convert native wheel units to logical window-axis pixels shared by scroll and input.
pub fn wheel_delta_pixels(delta: ScrollDelta, scale_factor: f64, viewport: Size) -> Vec2 {
    match delta {
        ScrollDelta::LineDelta(x, y) => Vec2::new(f64::from(x) * 40., f64::from(y) * 40.),
        ScrollDelta::PageDelta(x, y) => Vec2::new(
            f64::from(x) * viewport.width,
            f64::from(y) * viewport.height,
        ),
        ScrollDelta::PixelDelta(p) => Vec2::new(p.x, p.y) / scale_factor,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScrollAxis {
    Horizontal,
    #[default]
    Vertical,
    Both,
}
impl ScrollAxis {
    pub fn horizontal(self) -> bool {
        self != Self::Vertical
    }
    pub fn vertical(self) -> bool {
        self != Self::Horizontal
    }
}

#[derive(Clone, Copy)]
struct Touch {
    info: PointerInfo,
    point: Point,
    dragging: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScrollAlign {
    #[default]
    Start,
    Center,
    End,
}

/// Boundary behavior for direct touch panning. Logical offsets stay bounded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScrollOverscroll {
    #[default]
    Clamp,
    Bounce,
}

#[derive(Default)]
pub struct ScrollState {
    offset: Vec2,
    viewport: Size,
    content: Size,
    axis: ScrollAxis,
    touch: Option<Touch>,
    overscroll: ScrollOverscroll,
    pull: Vec2,
    rebound: Option<(Vec2, u64)>,
}
impl ScrollState {
    pub fn viewport(&self) -> Size {
        self.viewport
    }
    pub fn content(&self) -> Size {
        self.content
    }
    pub fn translation(&self, align: ScrollAlign) -> Vec2 {
        self.logical_translation(align) + self.elastic_translation()
    }
    /// Translation for logical viewport feedback, excluding transient elasticity.
    pub fn logical_translation(&self, align: ScrollAlign) -> Vec2 {
        let factor = match align {
            ScrollAlign::Start => 0.,
            ScrollAlign::Center => 0.5,
            ScrollAlign::End => 1.,
        };
        Vec2::new(
            if self.axis.horizontal() {
                (self.viewport.width - self.content.width).max(0.) * factor
            } else {
                0.
            },
            if self.axis.vertical() {
                (self.viewport.height - self.content.height).max(0.) * factor
            } else {
                0.
            },
        ) - self.offset
    }
    pub fn offset(&self) -> Vec2 {
        self.offset
    }
    fn maximum(&self) -> Vec2 {
        Vec2::new(
            if self.axis.horizontal() {
                (self.content.width - self.viewport.width).max(0.)
            } else {
                0.
            },
            if self.axis.vertical() {
                (self.content.height - self.viewport.height).max(0.)
            } else {
                0.
            },
        )
    }
    fn clamped(&self, offset: Vec2) -> Vec2 {
        let max = self.maximum();
        Vec2::new(offset.x.clamp(0., max.x), offset.y.clamp(0., max.y))
    }
    pub fn set_offset(&mut self, offset: Vec2) -> bool {
        let next = self.clamped(offset);
        let changed = self.offset != next || self.pull != Vec2::ZERO;
        self.offset = next;
        self.pull = Vec2::ZERO;
        self.rebound = None;
        changed
    }
    /// Choose direct-touch boundary behavior without changing the logical position.
    pub fn set_overscroll(&mut self, overscroll: ScrollOverscroll) -> bool {
        if self.overscroll == overscroll {
            return false;
        }
        self.overscroll = overscroll;
        let changed = self.pull != Vec2::ZERO;
        self.cancel();
        changed
    }
    fn elastic_translation(&self) -> Vec2 {
        let soften = |pull: f64, extent: f64| {
            let limit = (extent * 0.25).max(1.);
            -pull * 0.5 / (1. + pull.abs() * 0.5 / limit)
        };
        Vec2::new(
            soften(self.pull.x, self.viewport.width),
            soften(self.pull.y, self.viewport.height),
        )
    }
    fn pan(&mut self, delta: Vec2) -> bool {
        if self.overscroll == ScrollOverscroll::Clamp {
            return self.set_offset(self.offset + delta);
        }
        let before = (self.offset, self.pull);
        let raw = self.offset + self.pull + delta;
        self.offset = self.clamped(raw);
        self.pull = raw - self.offset;
        if !self.axis.horizontal() {
            self.pull.x = 0.;
        }
        if !self.axis.vertical() {
            self.pull.y = 0.;
        }
        before != (self.offset, self.pull)
    }
    fn release(&mut self) {
        self.touch = None;
        self.rebound = (self.pull != Vec2::ZERO).then_some((self.pull, 0));
    }
    /// Whether the scroll needs another native animation sample.
    pub fn animating(&self) -> bool {
        self.rebound.is_some()
    }
    /// Advance the native rebound using elapsed nanoseconds since the previous sample.
    pub fn advance(&mut self, elapsed_ns: u64) -> bool {
        let previous = self.pull;
        let Some((initial, elapsed)) = &mut self.rebound else {
            return false;
        };
        *elapsed = elapsed.saturating_add(elapsed_ns);
        let time = *elapsed as f64 / 1_000_000_000.;
        let decay = (1. + 24. * time) * (-24. * time).exp();
        self.pull = *initial * decay;
        if self.elastic_translation().hypot() < 0.05 {
            self.pull = Vec2::ZERO;
            self.rebound = None;
        }
        previous != self.pull
    }
    /// Content edits retain reading position; a reader at the end can follow appends.
    pub fn layout(&mut self, viewport: Size, content: Size, axis: ScrollAxis, follow_end: bool) {
        let old_max = self.maximum();
        let at_end_x = self.offset.x >= old_max.x;
        let at_end_y = self.offset.y >= old_max.y;
        if self.axis != axis || self.viewport != viewport || self.content != content {
            self.cancel();
        }
        self.viewport = viewport;
        self.content = content;
        self.axis = axis;
        let max = self.maximum();
        self.offset = self.clamped(Vec2::new(
            if follow_end && at_end_x {
                max.x
            } else {
                self.offset.x
            },
            if follow_end && at_end_y {
                max.y
            } else {
                self.offset.y
            },
        ));
    }
    /// Returns how far the target rectangle moved in this viewport.
    pub fn reveal(&mut self, rect: Rect) -> Vec2 {
        let elastic = self.elastic_translation();
        let rect = rect - elastic;
        let delta = |start: f64, end: f64, extent: f64| {
            if start < 0. || end - start > extent {
                start
            } else {
                (end - extent).max(0.)
            }
        };
        let before = self.offset;
        self.set_offset(
            before
                + Vec2::new(
                    delta(rect.x0, rect.x1, self.viewport.width),
                    delta(rect.y0, rect.y1, self.viewport.height),
                ),
        );
        before - self.offset - elastic
    }
    pub fn cancel(&mut self) {
        self.touch = None;
        self.pull = Vec2::ZERO;
        self.rebound = None;
    }
    pub fn preview(&mut self, ctx: &mut EventCtx<'_>, event: &PointerEvent) {
        let info = pointer_info(event);
        if info.pointer_type != PointerType::Touch {
            return;
        }
        match event {
            PointerEvent::Down(e) if self.touch.is_none_or(|touch| !touch.dragging) => {
                self.rebound = None;
                self.touch = Some(Touch {
                    info,
                    point: ctx.local_position(e.state.position),
                    dragging: false,
                });
            }
            PointerEvent::Move(e) => {
                if let Some(touch) = &mut self.touch
                    && touch.info == info
                    && !touch.dragging
                {
                    let delta = touch.point - ctx.local_position(e.current.position);
                    let axis_delta = match self.axis {
                        ScrollAxis::Horizontal if delta.x.abs() >= delta.y.abs() => delta.x.abs(),
                        ScrollAxis::Vertical if delta.y.abs() >= delta.x.abs() => delta.y.abs(),
                        ScrollAxis::Both => delta.hypot(),
                        _ => 0.,
                    };
                    if axis_delta > 8.
                        && (self.overscroll == ScrollOverscroll::Bounce
                            || self.clamped(self.offset + delta) != self.offset)
                    {
                        self.touch.as_mut().unwrap().dragging = true;
                        ctx.capture_pointer();
                        ctx.set_handled();
                    }
                }
            }
            PointerEvent::Up(_) | PointerEvent::Cancel(_)
                if self
                    .touch
                    .is_some_and(|touch| touch.info == info && !touch.dragging) =>
            {
                if matches!(event, PointerEvent::Up(_)) {
                    self.release();
                    if self.animating() {
                        ctx.request_anim_frame();
                    }
                } else {
                    self.cancel();
                }
            }
            _ => {}
        }
    }
    /// Handles wheel input or an owned touch drag. Only actual movement consumes a wheel.
    pub fn pointer(&mut self, ctx: &mut EventCtx<'_>, event: &PointerEvent) -> bool {
        let info = pointer_info(event);
        match event {
            PointerEvent::Scroll(event) => {
                let delta = wheel_delta_pixels(event.delta, ctx.scale_factor(), self.viewport);
                let delta = if self.axis == ScrollAxis::Horizontal && delta.x == 0. {
                    Vec2::new(delta.y, 0.)
                } else {
                    delta
                };
                if delta.is_finite() {
                    let before = self.offset;
                    if self.set_offset(self.offset + delta) {
                        if self.offset != before {
                            ctx.set_handled();
                            return true;
                        }
                        ctx.request_compose();
                    }
                }
            }
            PointerEvent::Move(e) if ctx.is_active() => {
                if let Some(touch) = self.touch
                    && touch.info == info
                    && touch.dragging
                {
                    let point = ctx.local_position(e.current.position);
                    self.touch.as_mut().unwrap().point = point;
                    ctx.set_handled();
                    return self.pan(touch.point - point);
                }
            }
            PointerEvent::Up(_) | PointerEvent::Cancel(_)
                if self.touch.is_some_and(|touch| touch.info == info) =>
            {
                if self.touch.unwrap().dragging && ctx.is_active() {
                    ctx.release_pointer();
                    ctx.set_handled();
                }
                if matches!(event, PointerEvent::Up(_)) {
                    self.release();
                    if self.animating() {
                        ctx.request_anim_frame();
                    }
                } else {
                    let changed = self.pull != Vec2::ZERO;
                    self.cancel();
                    return changed;
                }
            }
            _ => {}
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> ScrollState {
        let mut state = ScrollState::default();
        state.layout(
            Size::new(200., 300.),
            Size::new(200., 900.),
            ScrollAxis::Vertical,
            false,
        );
        state
    }

    #[test]
    fn touch_elasticity_is_opt_in_and_keeps_logical_edges_bounded() {
        let mut state = state();
        assert!(!state.pan(Vec2::new(0., -40.)));
        assert_eq!(state.translation(ScrollAlign::Start), Vec2::ZERO);
        state.set_overscroll(ScrollOverscroll::Bounce);
        assert!(state.pan(Vec2::new(0., -40.)));
        assert_eq!(state.offset(), Vec2::ZERO);
        assert_eq!(state.logical_translation(ScrollAlign::Start), Vec2::ZERO);
        let visual = state.translation(ScrollAlign::Start).y;
        assert!(visual > 0. && visual < 40.);
        state.pan(Vec2::new(0., 40.));
        assert_eq!(state.translation(ScrollAlign::Start), Vec2::ZERO);
        state.pan(Vec2::new(0., 30.));
        assert_eq!(state.offset().y, 30.);
        state.set_offset(Vec2::new(0., 600.));
        state.pan(Vec2::new(400., 100_000.));
        assert_eq!(state.offset(), Vec2::new(0., 600.));
        let translation = state.translation(ScrollAlign::Start);
        assert_eq!(translation.x, 0.);
        assert!(translation.y < -600. && translation.y > -675.);
    }

    #[test]
    fn rebound_uses_elapsed_time_and_stops_requesting_frames() {
        let mut a = state();
        let mut b = state();
        for state in [&mut a, &mut b] {
            state.set_overscroll(ScrollOverscroll::Bounce);
            state.pan(Vec2::new(0., -120.));
            state.release();
        }
        for _ in 0..6 {
            a.advance(20_000_000);
        }
        for _ in 0..12 {
            b.advance(10_000_000);
        }
        assert_eq!(
            a.translation(ScrollAlign::Start),
            b.translation(ScrollAlign::Start)
        );
        for _ in 0..60 {
            a.advance(16_666_667);
        }
        assert!(!a.animating());
        assert!(!a.advance(1_000_000_000));
        assert_eq!(a.translation(ScrollAlign::Start), Vec2::ZERO);
        assert_eq!(a.offset(), Vec2::ZERO);
    }

    #[test]
    fn programmatic_positioning_and_geometry_changes_clear_elastic_state() {
        let mut state = state();
        state.set_overscroll(ScrollOverscroll::Bounce);
        state.pan(Vec2::new(0., -80.));
        state.layout(
            state.viewport(),
            state.content(),
            ScrollAxis::Vertical,
            false,
        );
        assert!(state.translation(ScrollAlign::Start).y > 0.);
        assert!(state.set_offset(Vec2::ZERO));
        assert_eq!(state.translation(ScrollAlign::Start), Vec2::ZERO);
        state.pan(Vec2::new(0., -80.));
        state.release();
        state.layout(
            Size::new(200., 250.),
            state.content(),
            ScrollAxis::Vertical,
            false,
        );
        assert!(!state.animating());
        assert_eq!(state.translation(ScrollAlign::Start), Vec2::ZERO);
        state.pan(Vec2::new(0., -80.));
        assert!(state.set_overscroll(ScrollOverscroll::Clamp));
        assert_eq!(state.translation(ScrollAlign::Start), Vec2::ZERO);
    }
}
