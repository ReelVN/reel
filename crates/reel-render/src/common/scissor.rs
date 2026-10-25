use reel_ui::{
    imaging::record::{Clip, Geometry},
    kurbo::{Affine, Rect},
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
