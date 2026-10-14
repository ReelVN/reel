// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0
//! Shared scroll geometry and gesture ownership for native and authored views.
use crate::core::{EventCtx, PointerEvent, PointerInfo, PointerType, ScrollDelta, pointer_info};
use crate::kurbo::{Point, Rect, Size, Vec2};

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

#[derive(Default)]
pub struct ScrollState {
    offset: Vec2,
    viewport: Size,
    content: Size,
    axis: ScrollAxis,
    touch: Option<Touch>,
}
impl ScrollState {
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
        let changed = self.offset != next;
        self.offset = next;
        changed
    }
    /// Content edits retain reading position; a reader at the end can follow appends.
    pub fn layout(&mut self, viewport: Size, content: Size, axis: ScrollAxis, follow_end: bool) {
        let old_max = self.maximum();
        let at_end_x = self.offset.x >= old_max.x;
        let at_end_y = self.offset.y >= old_max.y;
        if self.axis != axis {
            self.cancel();
        }
        self.viewport = viewport;
        self.content = content;
        self.axis = axis;
        let max = self.maximum();
        self.set_offset(Vec2::new(
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
        before - self.offset
    }
    pub fn cancel(&mut self) {
        self.touch = None;
    }
    pub fn preview(&mut self, ctx: &mut EventCtx<'_>, event: &PointerEvent) {
        let info = pointer_info(event);
        if info.pointer_type != PointerType::Touch {
            return;
        }
        match event {
            PointerEvent::Down(e) if self.touch.is_none_or(|touch| !touch.dragging) => {
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
                    if axis_delta > 8. && self.clamped(self.offset + delta) != self.offset {
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
                self.cancel();
            }
            _ => {}
        }
    }
    /// Handles wheel input or an owned touch drag. Only actual movement consumes a wheel.
    pub fn pointer(&mut self, ctx: &mut EventCtx<'_>, event: &PointerEvent) -> bool {
        let info = pointer_info(event);
        match event {
            PointerEvent::Scroll(event) => {
                let delta = match event.delta {
                    ScrollDelta::LineDelta(x, y) => {
                        Vec2::new(f64::from(x) * 40., f64::from(y) * 40.)
                    }
                    ScrollDelta::PageDelta(x, y) => Vec2::new(
                        f64::from(x) * self.viewport.width,
                        f64::from(y) * self.viewport.height,
                    ),
                    ScrollDelta::PixelDelta(p) => Vec2::new(p.x, p.y) / ctx.scale_factor(),
                };
                let delta = if self.axis == ScrollAxis::Horizontal && delta.x == 0. {
                    Vec2::new(delta.y, 0.)
                } else {
                    delta
                };
                if delta.is_finite() && self.set_offset(self.offset + delta) {
                    ctx.set_handled();
                    return true;
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
                    return self.set_offset(self.offset + (touch.point - point));
                }
            }
            PointerEvent::Up(_) | PointerEvent::Cancel(_)
                if self.touch.is_some_and(|touch| touch.info == info) =>
            {
                if self.touch.unwrap().dragging && ctx.is_active() {
                    ctx.release_pointer();
                    ctx.set_handled();
                }
                self.cancel();
            }
            _ => {}
        }
        false
    }
}
