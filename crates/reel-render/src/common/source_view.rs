//! Physical source placement shared by replay and target admission.
use super::scissor::Scissor;
use reel_ui::{
    frame::{FrameGroup, InkBounds},
    kurbo::Affine,
};

/// An isolated source keeps its storage, initialized content and parent pixels
/// distinct. Integer translation preserves the parent's rasterization samples.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SourceView {
    pub storage_size: [u32; 2],
    pub source_rect: Scissor,
    pub parent_rect: Scissor,
    pub parent_to_source: Affine,
}

impl SourceView {
    /// Plan a finite source-over group. Operations with their own sampling domain
    /// retain the complete source until their dependencies have a shared plan.
    pub fn group(
        group: &FrameGroup,
        ink: InkBounds,
        outer: Affine,
        parent_size: [u32; 2],
        parent_scissor: Scissor,
        retained_transitions: bool,
        max_dimension: u32,
    ) -> Option<Self> {
        if retained_transitions
            || group.composite.blend != Default::default()
            || group.clip.is_some()
            || group.mask.is_some()
            || group.image_mask.is_some()
            || group.effect.is_some()
            || group.mask_effect.is_some()
            || !group.filters.is_empty()
        {
            return None;
        }
        let parent_rect = parent_scissor.intersect(Scissor::ink_bounds(ink, outer, parent_size)?);
        // Buckets use 32 physical pixels, independent of author canvas scale.
        // Keeping adjacent extents in one bucket avoids rebuilding targets for
        // subpixel motion. The pool retains both buckets when an edge crosses it.
        let extent = |value: u32| {
            value
                .max(1)
                .div_ceil(32)
                .saturating_mul(32)
                .min(max_dimension)
        };
        let storage_size = [extent(parent_rect.width), extent(parent_rect.height)];
        if u64::from(storage_size[0]) * u64::from(storage_size[1])
            >= u64::from(parent_size[0]) * u64::from(parent_size[1])
        {
            return None;
        }
        Some(Self {
            storage_size,
            source_rect: Scissor {
                x: 0,
                y: 0,
                width: parent_rect.width,
                height: parent_rect.height,
            },
            parent_rect,
            parent_to_source: Affine::translate((
                -f64::from(parent_rect.x),
                -f64::from(parent_rect.y),
            )),
        })
    }

    pub fn frame_to_source(self, outer: Affine) -> Affine {
        self.parent_to_source * outer
    }

    /// Complete local shader sources retain their exact storage and an explicit
    /// affine projection. They never inherit an ancestor's output clipping.
    pub fn local(storage_size: [u32; 2], parent_to_source: Affine, parent_rect: Scissor) -> Self {
        Self {
            storage_size,
            source_rect: Scissor::full(storage_size),
            parent_rect,
            parent_to_source,
        }
    }

    pub fn origin(self) -> [u32; 2] {
        [self.parent_rect.x, self.parent_rect.y]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reel_ui::{
        core::Widget,
        kurbo::{Point, Rect},
        widgets::Label,
    };

    fn group() -> FrameGroup {
        FrameGroup {
            image_mask: None,
            effect: None,
            mask_effect: None,
            clip: None,
            mask: None,
            filters: Vec::new(),
            composite: reel_ui::frame::GroupComposite {
                alpha: 0.5,
                ..Default::default()
            },
            invert: 0.,
            owner: Label::new("").prepare().id(),
            effect_to_target: Affine::IDENTITY,
            ink_bounds: InkBounds::Empty,
        }
    }

    #[test]
    fn cropped_sources_preserve_physical_samples_under_affine_transforms() {
        let group = group();
        let ink = InkBounds::Rect(Rect::new(-3.5, 2.25, 110.5, 70.75));
        let parent_size = [960, 544];
        for outer in [
            Affine::translate((30.5, 20.25)),
            Affine::translate((250., 150.)) * Affine::rotate(0.4),
            Affine::translate((300., 200.)) * Affine::scale_non_uniform(-1.25, 0.75),
            Affine::new([1., 0.2, -0.3, 1., 100., 70.]),
        ] {
            let view = SourceView::group(
                &group,
                ink,
                outer,
                parent_size,
                Scissor::full(parent_size),
                false,
                4096,
            )
            .unwrap();
            let mapped = view.frame_to_source(outer);
            for local in [Point::new(0., 3.), Point::new(70., 50.)] {
                let parent = outer * local;
                let source = mapped * local;
                assert!((source.x + f64::from(view.parent_rect.x) - parent.x).abs() < 1e-10);
                assert!((source.y + f64::from(view.parent_rect.y) - parent.y).abs() < 1e-10);
            }
            for sample in [0.125, 0.375, 0.5, 0.875] {
                // Translation leaves both single-sample and MSAA sample grids unchanged.
                let parent = Point::new(
                    f64::from(view.parent_rect.x) + 5. + sample,
                    f64::from(view.parent_rect.y) + 7. + sample,
                );
                let source = view.frame_to_source(Affine::IDENTITY) * parent;
                assert!((source.x - (5. + sample)).abs() < 1e-10);
                assert!((source.y - (7. + sample)).abs() < 1e-10);
            }
            assert!(view.storage_size[0] >= view.source_rect.width);
            assert!(view.storage_size[1] >= view.source_rect.height);
        }
    }

    #[test]
    fn buckets_keep_content_and_clipping_separate() {
        let group = group();
        let plan = |right| {
            SourceView::group(
                &group,
                InkBounds::Rect(Rect::new(400., 200., right, 240.)),
                Affine::IDENTITY,
                [960, 544],
                Scissor::full([960, 544]),
                false,
                4096,
            )
            .unwrap()
        };
        let a = plan(550.1);
        let b = plan(551.1);
        assert_eq!(a.storage_size, b.storage_size);
        assert_ne!(a.source_rect.width, b.source_rect.width);
        assert!(a.storage_size[0] * a.storage_size[1] < 960 * 544 / 20);
        assert!(
            SourceView::group(
                &group,
                InkBounds::Unbounded,
                Affine::IDENTITY,
                [960, 544],
                Scissor::full([960, 544]),
                false,
                4096
            )
            .is_none()
        );
        assert!(
            SourceView::group(
                &group,
                InkBounds::Rect(Rect::new(0., 0., 20., 20.)),
                Affine::IDENTITY,
                [960, 544],
                Scissor::full([960, 544]),
                true,
                4096
            )
            .is_none()
        );
    }

    #[test]
    fn linear_sample_centers_stay_in_initialized_storage_at_content_edges() {
        let group = group();
        let parent_size = [960, 544];
        let clip = Scissor {
            x: 411,
            y: 205,
            width: 87,
            height: 23,
        };
        let view = SourceView::group(
            &group,
            InkBounds::Rect(Rect::new(400.25, 200.5, 550.75, 245.25)),
            Affine::IDENTITY,
            parent_size,
            clip,
            false,
            4096,
        )
        .unwrap();
        assert_eq!(view.parent_rect, clip);
        assert!(view.storage_size[0] > view.source_rect.width);
        assert!(view.storage_size[1] > view.source_rect.height);
        for y in [0, view.source_rect.height - 1] {
            for x in [0, view.source_rect.width - 1] {
                let parent = Point::new(f64::from(clip.x + x) + 0.5, f64::from(clip.y + y) + 0.5);
                let source = view.parent_to_source * parent;
                // The bilinear footprint is centered on the original physical
                // texel, never on a stale texel from a larger pooled allocation.
                assert_eq!(source, Point::new(f64::from(x) + 0.5, f64::from(y) + 0.5));
                let uv = Point::new(
                    source.x / f64::from(view.storage_size[0]),
                    source.y / f64::from(view.storage_size[1]),
                );
                assert!(uv.x > 0. && uv.x < 1. && uv.y > 0. && uv.y < 1.);
            }
        }
        // Sources with their own clip never bypass that final coverage stage,
        // even when the parent opacity or inversion could otherwise be folded.
        let mut clipped = group;
        clipped.clip = Some(reel_ui::imaging::record::Clip::Fill {
            transform: Affine::IDENTITY,
            shape: reel_ui::imaging::record::Geometry::Rect(Rect::new(0., 0., 5., 5.)),
            fill_rule: Default::default(),
        });
        clipped.invert = 1.;
        assert!(
            SourceView::group(
                &clipped,
                InkBounds::Rect(Rect::new(0., 0., 5., 5.)),
                Affine::IDENTITY,
                parent_size,
                clip,
                false,
                4096
            )
            .is_none()
        );
    }
}
