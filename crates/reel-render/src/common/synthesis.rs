// Copyright (c) 2020 Chad Brokaw
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Outline weight synthesis, using the existing Swash outline and Zeno rasterizer.
//!
//! The contour offset below is adapted under the MIT license from Swash 0.2.10,
//! src/scale/outline.rs, commit 7773843df0d63cd468db61a29c152b5e7a99d4ab:
//! https://github.com/dfrg/swash/tree/7773843df0d63cd468db61a29c152b5e7a99d4ab
//! Local changes: close each contour when determining orientation and convert
//! the recorded total stroke increase to the offset algorithm's half-strength.

use std::ops::Range;
use swash::{
    scale::outline::Outline,
    zeno::{Point, Verb},
};

/// Applies total X/Y stroke increases to a monochrome outline, keeping advances.
pub(super) fn embolden(outline: &mut Outline, x: f32, y: f32) {
    let winding = winding(outline.points(), outline.verbs());
    let mut contours = Contours::default();
    while let Some(range) = contours.next(outline.verbs()) {
        embolden_contour(&mut outline.points_mut()[range], winding, x * 0.5, y * 0.5);
    }
}

// A glyph can contain disjoint contours and oppositely wound counters. Connecting
// their end points invents edges whose area can reverse the entire glyph's sign.
fn winding(points: &[Point], verbs: &[Verb]) -> u8 {
    let mut area = 0_f64;
    let mut contours = Contours::default();
    while let Some(range) = contours.next(verbs) {
        let points = &points[range];
        let mut prev = points[points.len() - 1];
        for cur in points {
            area += f64::from(cur.y - prev.y) * f64::from(cur.x + prev.x);
            prev = *cur;
        }
    }
    u8::from(area > 0.)
}

#[derive(Default)]
struct Contours {
    verb: usize,
    point: usize,
    start: usize,
    finished: bool,
}

impl Contours {
    fn next(&mut self, verbs: &[Verb]) -> Option<Range<usize>> {
        while let Some(verb) = verbs.get(self.verb) {
            self.verb += 1;
            match verb {
                Verb::MoveTo | Verb::Close => {
                    let range = self.start..self.point;
                    self.start = self.point;
                    self.point += usize::from(*verb == Verb::MoveTo);
                    if !range.is_empty() {
                        return Some(range);
                    }
                }
                Verb::LineTo => self.point += 1,
                Verb::QuadTo => self.point += 2,
                Verb::CurveTo => self.point += 3,
            }
        }
        if !self.finished {
            self.finished = true;
            let range = self.start..self.point;
            if !range.is_empty() {
                return Some(range);
            }
        }
        None
    }
}

fn embolden_contour(points: &mut [Point], winding: u8, x_strength: f32, y_strength: f32) {
    if points.is_empty() {
        return;
    }
    let last = points.len() - 1;
    let mut i = last;
    let mut j = 0;
    let mut k = !0;
    let mut out_len;
    let mut in_len = 0.;
    let mut anchor_len = 0.;
    let mut anchor = Point::ZERO;
    let mut out;
    let mut in_ = Point::ZERO;
    while j != i && i != k {
        if j != k {
            out = points[j] - points[i];
            out_len = out.length();
            if out_len == 0. {
                j = if j < last { j + 1 } else { 0 };
                continue;
            } else {
                let s = 1. / out_len;
                out.x *= s;
                out.y *= s;
            }
        } else {
            out = anchor;
            out_len = anchor_len;
        }
        if in_len != 0. {
            if k == !0 {
                k = i;
                anchor = in_;
                anchor_len = in_len;
            }
            let mut d = (in_.x * out.x) + (in_.y * out.y);
            let shift = if d > -0.9396 {
                d += 1.;
                let mut sx = in_.y + out.y;
                let mut sy = in_.x + out.x;
                if winding == 0 {
                    sx = -sx;
                } else {
                    sy = -sy;
                }
                let mut q = (out.x * in_.y) - (out.y * in_.x);
                if winding == 0 {
                    q = -q;
                }
                let l = in_len.min(out_len);
                if x_strength * q <= l * d {
                    sx = sx * x_strength / d;
                } else {
                    sx = sx * l / q;
                }
                if y_strength * q <= l * d {
                    sy = sy * y_strength / d;
                } else {
                    sy = sy * l / q;
                }
                Point::new(sx, sy)
            } else {
                Point::ZERO
            };

            while i != j {
                points[i].x += x_strength + shift.x;
                points[i].y += y_strength + shift.y;
                i = if i < last { i + 1 } else { 0 };
            }
        } else {
            i = j;
        }
        in_ = out;
        in_len = out_len;
        j = if j < last { j + 1 } else { 0 };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO_RECTS: [Verb; 10] = [
        Verb::MoveTo,
        Verb::LineTo,
        Verb::LineTo,
        Verb::LineTo,
        Verb::Close,
        Verb::MoveTo,
        Verb::LineTo,
        Verb::LineTo,
        Verb::LineTo,
        Verb::Close,
    ];

    #[test]
    fn disconnected_contours_do_not_invent_edges_when_finding_orientation() {
        let mut points = [
            Point::new(2., 2.),
            Point::new(2., 0.),
            Point::new(0., 0.),
            Point::new(0., 2.),
            Point::new(-8., -8.),
            Point::new(-8., -6.),
            Point::new(-6., -6.),
            Point::new(-6., -8.),
        ];
        // Closing only the concatenated point list gives +24, despite both
        // actual contours having area -8. Their placement cannot change weight.
        let winding = winding(&points, &TWO_RECTS);
        assert_eq!(winding, 0);
        for contour in points.as_chunks_mut::<4>().0 {
            embolden_contour(contour, winding, 0.625, 0.625);
            let bounds = swash::zeno::Bounds::from_points(contour);
            assert_eq!(bounds.width(), 3.25);
            assert_eq!(bounds.height(), 3.25);
        }
    }

    #[test]
    fn total_stroke_increase_expands_outer_edges_and_contracts_counters() {
        for reverse in [false, true] {
            let mut points = [
                Point::new(0., 0.),
                Point::new(10., 0.),
                Point::new(10., 10.),
                Point::new(0., 10.),
                Point::new(3., 3.),
                Point::new(3., 7.),
                Point::new(7., 7.),
                Point::new(7., 3.),
            ];
            if reverse {
                points[..4].reverse();
                points[4..].reverse();
            }
            let winding = winding(&points, &TWO_RECTS);
            for contour in points.as_chunks_mut::<4>().0 {
                embolden_contour(contour, winding, 1.25 * 0.5, 2. * 0.5);
            }
            let outer = swash::zeno::Bounds::from_points(&points[..4]);
            let counter = swash::zeno::Bounds::from_points(&points[4..]);
            assert_eq!((outer.width(), outer.height()), (11.25, 12.));
            assert_eq!((counter.width(), counter.height()), (2.75, 2.));
        }
    }

    #[test]
    fn contour_ranges_include_curve_controls_and_final_open_contour() {
        let verbs = [
            Verb::MoveTo,
            Verb::QuadTo,
            Verb::Close,
            Verb::MoveTo,
            Verb::CurveTo,
        ];
        let mut contours = Contours::default();
        assert_eq!(contours.next(&verbs), Some(0..3));
        assert_eq!(contours.next(&verbs), Some(3..7));
        assert_eq!(contours.next(&verbs), None);
        assert_eq!(contours.next(&verbs), None);
    }
}
