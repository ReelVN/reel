use reel_ui::{
    frame::{BlendMode, FrameGroup, InkBounds},
    imaging::record::{Clip, Geometry},
    kurbo::{Affine, Rect},
    peniko::{Compose, Mix},
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Scissor {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
impl Scissor {
    pub fn full(size: [u32; 2]) -> Self {
        Self {
            x: 0,
            y: 0,
            width: size[0],
            height: size[1],
        }
    }
    pub fn empty(self) -> bool {
        self.width == 0 || self.height == 0
    }
    pub fn intersect(self, other: Self) -> Self {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        Self {
            x,
            y,
            width: (self.x + self.width)
                .min(other.x + other.width)
                .saturating_sub(x),
            height: (self.y + self.height)
                .min(other.y + other.height)
                .saturating_sub(y),
        }
    }
    /// Single-sample rectangle coverage is determined at pixel centers. Fractional
    /// edges therefore need no coverage target unless they approach a sample,
    /// where the backend's triangle edge ownership must remain authoritative.
    /// Multisampled targets retain geometry coverage for fractional edges.
    pub fn from_clip(clip: &Clip, outer: Affine, size: [u32; 2], samples: u32) -> Option<Self> {
        let Clip::Fill {
            transform,
            shape: Geometry::Rect(rect),
            ..
        } = clip
        else {
            return None;
        };
        let transform = outer * *transform;
        let [_, b, c, _, _, _] = transform.as_coeffs();
        if b != 0. || c != 0. {
            return None;
        }
        let Rect { x0, y0, x1, y1 } = transform.transform_rect_bbox(*rect);
        let edge = |value: f64, extent: u32| {
            if !value.is_finite() {
                return None;
            }
            let value = value.clamp(0., f64::from(extent));
            if samples == 1 {
                // GLES guarantees at least four fractional rasterization bits.
                // Keep one subpixel step around the center on the geometry path
                // so vertex quantization cannot change the accepted pixels.
                if (value.fract() - 0.5).abs() <= 1. / 16. {
                    return None;
                }
                Some(value.round() as u32)
            } else {
                (value.fract() == 0.).then_some(value as u32)
            }
        };
        let x0 = edge(x0, size[0])?;
        let x1 = edge(x1, size[0])?;
        let y0 = edge(y0, size[1])?;
        let y1 = edge(y1, size[1])?;
        Some(Self {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        })
    }
    /// Limits isolated rectangular clip work while preserving its geometric
    /// coverage pass. One physical pixel includes rasterization edge precision.
    pub fn clip_bounds(clip: &Clip, outer: Affine, size: [u32; 2]) -> Option<Self> {
        let Clip::Fill {
            transform,
            shape: Geometry::Rect(rect),
            ..
        } = clip
        else {
            return None;
        };
        let bounds = (outer * *transform).transform_rect_bbox(*rect);
        Self::conservative_bounds(bounds, size)
    }
    /// Bound a completed group's output in frame coordinates, independently of
    /// its effect-local transform. FrameBuild includes shader effects' logical
    /// output domain. Only zero-source identity blends qualify.
    /// Retained transitions can draw an older, larger source absent from this ink.
    pub fn group_bounds(
        group: &FrameGroup,
        outer: Affine,
        size: [u32; 2],
        retained_transitions: bool,
    ) -> Option<Self> {
        if retained_transitions || !group.filters.is_empty() {
            return None;
        }
        match group.composite.blend {
            BlendMode::Standard(blend)
                if matches!(
                    (blend.mix, blend.compose),
                    (Mix::Normal | Mix::Multiply | Mix::Screen, Compose::SrcOver)
                ) =>
            {
                ()
            }
            _ => return None,
        }
        match group.ink_bounds {
            InkBounds::Empty => Some(Self::full([0, 0])),
            InkBounds::Unbounded => None,
            InkBounds::Rect(rect) => {
                if !rect.is_finite() || !outer.as_coeffs().into_iter().all(f64::is_finite) {
                    return None;
                }
                Self::conservative_bounds(outer.transform_rect_bbox(rect), size)
            }
        }
    }
    fn conservative_bounds(bounds: Rect, size: [u32; 2]) -> Option<Self> {
        if !bounds.is_finite() {
            return None;
        }
        let x0 = (bounds.x0.floor() - 1.).clamp(0., f64::from(size[0])) as u32;
        let x1 = (bounds.x1.ceil() + 1.).clamp(0., f64::from(size[0])) as u32;
        let y0 = (bounds.y0.floor() - 1.).clamp(0., f64::from(size[1])) as u32;
        let y1 = (bounds.y1.ceil() + 1.).clamp(0., f64::from(size[1])) as u32;
        Some(Self {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        })
    }
    #[cfg(feature = "wgpu")]
    pub fn apply(self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_scissor_rect(self.x, self.y, self.width, self.height);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(ink_bounds: InkBounds) -> FrameGroup {
        FrameGroup {
            image_mask: None,
            effect: None,
            clip: None,
            mask: None,
            filters: Vec::new(),
            composite: Default::default(),
            invert: 0.,
            owner: reel_ui::core::WidgetPod::new(reel_ui::widgets::Label::new("")).id(),
            effect_to_target: Affine::IDENTITY,
            ink_bounds,
        }
    }

    #[test]
    fn group_bounds_preserve_transformed_coverage_and_parent_samples() {
        let rect = Rect::new(2.25, 4.5, 35.5, 20.25);
        let group = group(InkBounds::Rect(rect));
        let parent = Scissor {
            x: 20,
            y: 16,
            width: 40,
            height: 28,
        };
        for outer in [
            Affine::translate((18.5, 12.5)),
            Affine::translate((60., 42.)) * Affine::scale_non_uniform(-0.6666667, -1.25),
            Affine::translate((30., 24.)) * Affine::rotate(0.6),
            Affine::new([1., 0.25, -0.3, 1., 25., 20.]),
        ] {
            let bounds = Scissor::group_bounds(&group, outer, [96, 64], false).unwrap();
            let intersection = parent.intersect(bounds);
            // Independent sample oracle: transform samples back into the source,
            // including positions near pixel boundaries and MSAA sample locations.
            for y in parent.y..parent.y + parent.height {
                for x in parent.x..parent.x + parent.width {
                    for offset in [0.001, 0.125, 0.5, 0.875, 0.999] {
                        let local = outer.inverse()
                            * reel_ui::kurbo::Point::new(x as f64 + offset, y as f64 + offset);
                        if rect.contains(local) {
                            assert!(x >= intersection.x && x < intersection.x + intersection.width);
                            assert!(
                                y >= intersection.y && y < intersection.y + intersection.height
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn group_bounds_keep_unknown_expanding_and_replacing_operations_conservative() {
        let size = [960, 544];
        let bounds = |g: &FrameGroup| Scissor::group_bounds(g, Affine::IDENTITY, size, false);
        let mut group = group(InkBounds::Empty);
        assert!(bounds(&group).unwrap().empty());
        group.ink_bounds = InkBounds::Unbounded;
        assert_eq!(bounds(&group), None);
        group.ink_bounds = InkBounds::Rect(Rect::new(200., 100., 320., 140.));
        let small = bounds(&group).unwrap();
        assert_eq!(small.width * small.height, 122 * 42);
        assert!(small.width * small.height < size[0] * size[1] / 100);
        group.effect_to_target = Affine::scale(100.);
        group.invert = 1.;
        group.composite.alpha = 0.5;
        assert_eq!(bounds(&group), Some(small));
        assert_eq!(
            Scissor::group_bounds(&group, Affine::IDENTITY, size, true),
            None
        );
        group.filters.push(reel_ui::imaging::Filter::Flood {
            color: reel_ui::peniko::Color::WHITE,
        });
        assert_eq!(bounds(&group), None);
        group.filters.clear();
        for blend in [
            BlendMode::Standard(reel_ui::peniko::BlendMode {
                mix: Mix::Normal,
                compose: Compose::Copy,
            }),
            BlendMode::Standard(reel_ui::peniko::BlendMode {
                mix: Mix::Normal,
                compose: Compose::PlusLighter,
            }),
            BlendMode::Subtract,
        ] {
            group.composite.blend = blend;
            assert_eq!(bounds(&group), None);
        }
        group.composite.blend = Default::default();
        assert_eq!(
            Scissor::group_bounds(&group, Affine::scale(f64::INFINITY), size, false),
            None
        );
        group.ink_bounds = InkBounds::Rect(Rect::new(0., 0., f64::NAN, 4.));
        assert_eq!(bounds(&group), None);
        group.ink_bounds = InkBounds::Rect(Rect::new(-100., -100., -10., -10.));
        assert!(bounds(&group).unwrap().empty());
    }

    fn clip(rect: Rect) -> Clip {
        Clip::Fill {
            transform: Affine::IDENTITY,
            shape: Geometry::Rect(rect),
            fill_rule: Default::default(),
        }
    }

    #[test]
    fn single_sample_scissor_matches_interior_pixel_centers() {
        for left in [-2.2, 0., 0.1, 0.4, 0.6, 1.25, 7.8, 12.2] {
            for right in [left, left + 0.2, left + 1.6, left + 8.1] {
                let rect = Rect::new(left, 1.2, right, 6.8);
                let Some(scissor) = Scissor::from_clip(&clip(rect), Affine::IDENTITY, [10, 10], 1)
                else {
                    continue;
                };
                for y in 0..10 {
                    for x in 0..10 {
                        let covered = x >= scissor.x
                            && x < scissor.x + scissor.width
                            && y >= scissor.y
                            && y < scissor.y + scissor.height;
                        let px = f64::from(x) + 0.5;
                        let py = f64::from(y) + 0.5;
                        assert_eq!(covered, px > left && px < right && py > 1.2 && py < 6.8);
                    }
                }
            }
        }
    }

    #[test]
    fn sample_edge_ownership_and_multisample_coverage_keep_geometry() {
        for rect in [
            Rect::new(0.5, 0., 8., 8.),
            Rect::new(0., 0.5, 8., 8.),
            Rect::new(0., 0., 8.5, 8.),
            Rect::new(0., 0., 8., 8.5),
            Rect::new(0.48, 0., 8., 8.),
            Rect::new(0., 0.52, 8., 8.),
        ] {
            assert!(Scissor::from_clip(&clip(rect), Affine::IDENTITY, [10, 10], 1).is_none());
        }
        let fractional = clip(Rect::new(0.2, 1.2, 8.2, 9.2));
        assert!(Scissor::from_clip(&fractional, Affine::IDENTITY, [10, 10], 1).is_some());
        assert!(Scissor::from_clip(&fractional, Affine::IDENTITY, [10, 10], 4).is_none());
        let integral = clip(Rect::new(0., 1., 8., 9.));
        assert!(Scissor::from_clip(&integral, Affine::IDENTITY, [10, 10], 4).is_some());
    }

    #[test]
    fn canvas_scale_mirrors_and_nested_intersections_remain_bounded() {
        let rect = clip(Rect::new(0., 0., 120., 60.));
        let scaled = Scissor::from_clip(
            &rect,
            Affine::scale(0.6666667) * Affine::translate((90., 90.)),
            [960, 544],
            1,
        )
        .unwrap();
        assert_eq!(
            scaled,
            Scissor {
                x: 60,
                y: 60,
                width: 80,
                height: 40
            }
        );
        let mirrored = Scissor::from_clip(
            &rect,
            Affine::translate((140., 100.)) * Affine::scale_non_uniform(-0.6666667, -0.6666667),
            [960, 544],
            1,
        )
        .unwrap();
        assert_eq!(scaled, mirrored);
        assert!(scaled.intersect(Scissor::full([30, 30])).empty());
        assert!(Scissor::from_clip(&rect, Affine::rotate(0.1), [960, 544], 1).is_none());
    }

    #[test]
    fn isolated_clip_bounds_cover_transformed_geometry_without_fullscreen_work() {
        let shape = Rect::new(0., 0., 120., 60.);
        let transform = Affine::translate((100., 100.)) * Affine::rotate(0.3);
        let bounds = Scissor::clip_bounds(&clip(shape), transform, [960, 544]).unwrap();
        let geometry = transform.transform_rect_bbox(shape);
        assert!(f64::from(bounds.x) <= geometry.x0 - 1.);
        assert!(f64::from(bounds.y) <= geometry.y0 - 1.);
        assert!(f64::from(bounds.x + bounds.width) >= geometry.x1 + 1.);
        assert!(f64::from(bounds.y + bounds.height) >= geometry.y1 + 1.);
        assert!(bounds.width * bounds.height < 960 * 544 / 20);
        assert!(
            Scissor::clip_bounds(&clip(shape), Affine::translate((-200., -100.)), [960, 544])
                .unwrap()
                .empty()
        );
    }
}
