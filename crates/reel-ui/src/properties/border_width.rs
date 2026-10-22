// Copyright 2025 the Xilem Authors
// SPDX-License-Identifier: Apache-2.0

use crate::core::{Property, UsesProperty, Widget};
use crate::kurbo::{Axis, BezPath, Insets, Point, Rect, Shape as _, Size, Vec2};
use crate::layout::Length;
use crate::properties::CornerRadius;

// Every widget has a border width.
impl<W: Widget> UsesProperty<BorderWidth> for W {}

/// The width of a widget's border.
#[expect(missing_docs, reason = "field names are self-descriptive")]
#[derive(Default, Clone, Copy, Debug, PartialEq)]
pub struct BorderWidth {
    pub width: Length,
}

// TODO - To match CSS, we should use a non-zero default width
// and a "border style" of "None".

impl Property for BorderWidth {
    fn static_default() -> &'static Self {
        static DEFAULT: BorderWidth = BorderWidth {
            width: Length::ZERO,
        };
        &DEFAULT
    }
}

impl BorderWidth {
    /// Creates new `BorderWidth` with given value.
    pub const fn all(width: Length) -> Self {
        Self { width }
    }

    /// Returns the total [`Length`] of this border on the given `axis`.
    ///
    /// For [`Axis::Horizontal`] it will return the sum of the left and right border width.
    /// For [`Axis::Vertical`] it will return the sum of the top and bottom border height.
    pub fn length(&self, _axis: Axis) -> Length {
        self.width.saturating_add(self.width)
    }

    /// Expands the `size` by the border width.
    ///
    /// The returned [`Size`] will be non-negative and in logical pixels.
    ///
    /// The provided `size` must be in logical pixels.
    ///
    /// Helper function to be called in [`Widget::layout`].
    pub fn size_up(&self, size: Size) -> Size {
        let border_width = self.width.get();
        let width = size.width + border_width * 2.;
        let height = size.height + border_width * 2.;
        Size::new(width, height)
    }

    /// Shrinks the `size` by the border width.
    ///
    /// The returned [`Size`] will be non-negative and in logical pixels.
    ///
    /// The provided `size` must be in logical pixels.
    ///
    /// Helper function to be called in [`Widget::layout`].
    pub fn size_down(&self, size: Size) -> Size {
        let border_width = self.width.get();
        let width = (size.width - border_width * 2.).max(0.);
        let height = (size.height - border_width * 2.).max(0.);
        Size::new(width, height)
    }

    /// Returns the [`Insets`] for deriving an area with this border.
    ///
    /// The returned [`Insets`] will be in logical pixels.
    ///
    /// The provided `insets` must be in logical pixels.
    pub fn insets_up(&self, insets: Insets) -> Insets {
        let border_width = self.width.get();
        Insets {
            x0: insets.x0 + border_width,
            y0: insets.y0 + border_width,
            x1: insets.x1 + border_width,
            y1: insets.y1 + border_width,
        }
    }

    /// Raises the `baseline` by the border width.
    ///
    /// The returned baseline will be in logical pixels.
    ///
    /// The provided `baseline` must be in logical pixels.
    ///
    /// Helper function to be called in [`Widget::layout`].
    pub fn baseline_up(&self, baseline: f64) -> f64 {
        baseline + self.width.get()
    }

    /// Lowers the `baseline` by the border width.
    ///
    /// The returned baseline will be in logical pixels.
    ///
    /// The provided `baseline` must be in logical pixels.
    ///
    /// Helper function to be called in [`Widget::layout`].
    pub fn baseline_down(&self, baseline: f64) -> f64 {
        baseline - self.width.get()
    }

    /// Lowers the position by the border width.
    ///
    /// The returned [`Point`] will be in logical pixels.
    ///
    /// The provided `origin` must be in logical pixels.
    ///
    /// Helper function to be called in [`Widget::layout`].
    pub fn origin_down(&self, origin: Point) -> Point {
        let border_width = self.width.get();
        origin + Vec2::new(border_width, border_width)
    }

    /// Creates an inside border with the requested outer corner radius.
    /// Fill this path using the even-odd rule; its inner contour excludes the content.
    pub fn border_path(&self, border_box: Rect, border_radius: &CornerRadius) -> BezPath {
        let outer = border_box.to_rounded_rect(border_radius.radius.get());
        let width = self.width.get();
        let inner = border_box.inset(-width);
        let mut path = BezPath::with_capacity(20);
        path.extend(outer.path_elements(0.1));
        if inner.width() > 0. && inner.height() > 0. {
            let radius = (outer.radii().top_left - width).max(0.);
            path.extend(inner.to_rounded_rect(radius).path_elements(0.1));
        }
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inside_border_keeps_outer_rounding_and_excludes_only_the_inner_box() {
        let bounds = Rect::new(0., 0., 60., 30.);
        for radius in [0., 8., 1000.] {
            let corners = CornerRadius::all(Length::px(radius));
            let outer = bounds.to_rounded_rect(radius);
            for width in [2., 15.] {
                let path = BorderWidth::all(Length::px(width)).border_path(bounds, &corners);
                let actual = path.bounding_box();
                for (actual, expected) in [actual.x0, actual.y0, actual.x1, actual.y1]
                    .into_iter()
                    .zip([bounds.x0, bounds.y0, bounds.x1, bounds.y1])
                {
                    assert!((actual - expected).abs() < 1e-9);
                }
                let inner = bounds
                    .inset(-width)
                    .to_rounded_rect((outer.radii().top_left - width).max(0.));
                for point in [
                    Point::new(0.1, 0.1),
                    Point::new(2.05, 9.),
                    Point::new(30., 1.),
                    Point::new(30., 15.),
                    Point::new(60.1, 15.),
                ] {
                    let in_outer = outer.winding(point) != 0;
                    let in_inner = width < 15. && inner.winding(point) != 0;
                    assert_eq!(
                        path.winding(point).abs() % 2 == 1,
                        in_outer && !in_inner,
                        "radius={radius}, width={width}, point={point:?}"
                    );
                }
            }
        }
    }
}
