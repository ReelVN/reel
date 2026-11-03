//! Share identical source pixels within an already admitted parent window.
use super::*;
use reel_ui::{
    frame::{FrameEffect, FrameGroup, FrameImage, ImageAppearance, InkBounds},
    imaging::record::Geometry,
    kurbo::Rect,
    peniko::Brush,
};

impl Plan {
    /// Freeze aliases after storage placement and physical output crops. Each
    /// alias names an earlier real producer in the same bounded source window.
    pub(super) fn reuse_sources(
        &mut self,
        drawing: &FrameDrawing,
        selection: &super::super::retained::Plan,
    ) -> bool {
        #[cfg(test)]
        if testing::disabled() {
            return false;
        }
        let mut start = 0;
        let mut reused = false;
        for index in 0..self.entries.len() {
            let entry = self.entries[index];
            if !entry.shared {
                let canonical = (start..index).find(|&previous| {
                    let previous = self.entries[previous];
                    previous.canonical_push.is_none()
                        && !previous.shared
                        && previous.source_size == entry.source_size
                        && previous.parent_scissor == entry.parent_scissor
                        && previous.view == entry.view
                        && self.same_source(drawing, selection, previous, entry)
                });
                if let Some(previous) = canonical {
                    self.entries[index].canonical_push = Some(self.entries[previous].push);
                    reused = true;
                }
            }
            if entry.last {
                start = index + 1;
            }
        }
        reused
    }

    fn same_source(
        &self,
        drawing: &FrameDrawing,
        selection: &super::super::retained::Plan,
        a: Entry,
        b: Entry,
    ) -> bool {
        let commands = drawing.commands();
        // The root is a completed source, not a deferred terminal shader.
        let root = |entry: Entry| matches!(&commands[entry.push].op, FrameOp::PushGroup(group) if group.effect.is_none());
        if a.end - a.push != b.end - b.push || !root(a) || !root(b) {
            return false;
        }
        (a.push..a.end).zip(b.push..b.end).all(|(a, b)| {
            if self.output_crop(a) != self.output_crop(b)
                || selection.sources.end(drawing, a).map(|end| end - a)
                    != selection.sources.end(drawing, b).map(|end| end - b)
            {
                return false;
            }
            match (&commands[a].op, &commands[b].op) {
                (FrameOp::PushGroup(a), FrameOp::PushGroup(b)) => same_group(a, b),
                (FrameOp::PopGroup, FrameOp::PopGroup)
                | (FrameOp::PushContext(_), FrameOp::PushContext(_))
                | (FrameOp::PopContext, FrameOp::PopContext) => true,
                (FrameOp::Draw(left), FrameOp::Draw(right)) => {
                    selection.selected(drawing, a) == selection.selected(drawing, b)
                        && same_draw(left, right)
                }
                _ => false,
            }
        })
    }
}

fn same_affine(a: Affine, b: Affine) -> bool {
    a.as_coeffs().map(f64::to_bits) == b.as_coeffs().map(f64::to_bits)
}
fn same_rect(a: Rect, b: Rect) -> bool {
    [a.x0, a.y0, a.x1, a.y1].map(f64::to_bits) == [b.x0, b.y0, b.x1, b.y1].map(f64::to_bits)
}
fn same_option<T>(a: Option<&T>, b: Option<&T>, equal: impl FnOnce(&T, &T) -> bool) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => equal(a, b),
        (None, None) => true,
        _ => false,
    }
}
fn same_effect(a: &FrameEffect, b: &FrameEffect) -> bool {
    a.definition == b.definition
        && a.shader == b.shader
        && a.values == b.values
        && a.images == b.images
        && same_rect(a.bounds, b.bounds)
        && a.coverage_invert == b.coverage_invert
        && a.resolution_scale == b.resolution_scale
}
fn same_group(a: &FrameGroup, b: &FrameGroup) -> bool {
    a.clip.is_none()
        && b.clip.is_none()
        && a.mask.is_none()
        && b.mask.is_none()
        && a.mask_effect.is_none()
        && b.mask_effect.is_none()
        && a.image_mask.is_none()
        && b.image_mask.is_none()
        && a.filters.is_empty()
        && b.filters.is_empty()
        && a.composite.blend == b.composite.blend
        && a.composite.alpha.to_bits() == b.composite.alpha.to_bits()
        && a.invert.to_bits() == b.invert.to_bits()
        && same_affine(a.effect_to_target, b.effect_to_target)
        && same_option(a.effect.as_ref(), b.effect.as_ref(), same_effect)
        && match (a.ink_bounds, b.ink_bounds) {
            (InkBounds::Rect(a), InkBounds::Rect(b)) => same_rect(a, b),
            (InkBounds::Empty, InkBounds::Empty) | (InkBounds::Unbounded, InkBounds::Unbounded) => {
                true
            }
            _ => false,
        }
}
fn same_appearance(a: &ImageAppearance, b: &ImageAppearance) -> bool {
    a.alpha_layout == b.alpha_layout
        && a.adjustment.saturation.to_bits() == b.adjustment.saturation.to_bits()
        && a.adjustment.gain.map(f32::to_bits) == b.adjustment.gain.map(f32::to_bits)
        && a.adjustment.offset.map(f32::to_bits) == b.adjustment.offset.map(f32::to_bits)
        && same_option(a.gradient.as_ref(), b.gradient.as_ref(), |a, b| {
            a.colors.map(|c| c.map(f32::to_bits)) == b.colors.map(|c| c.map(f32::to_bits))
                && same_affine(a.transform, b.transform)
        })
}
fn same_image(a: &FrameImage, b: &FrameImage) -> bool {
    a.resource == b.resource
        && a.color.map(f32::to_bits) == b.color.map(f32::to_bits)
        && same_option(a.source_rect.as_ref(), b.source_rect.as_ref(), |a, b| {
            same_rect(*a, *b)
        })
        && same_option(
            a.appearance.as_ref(),
            b.appearance.as_ref(),
            same_appearance,
        )
}
fn same_draw(a: &FrameDraw, b: &FrameDraw) -> bool {
    let (
        FrameDraw::Fill {
            transform: a_transform,
            fill_rule: a_rule,
            brush: a_brush,
            brush_transform: a_brush_transform,
            shape: Geometry::Rect(a_rect),
            composite: a_composite,
        },
        FrameDraw::Fill {
            transform: b_transform,
            fill_rule: b_rule,
            brush: b_brush,
            brush_transform: b_brush_transform,
            shape: Geometry::Rect(b_rect),
            composite: b_composite,
        },
    ) = (a, b)
    else {
        return false;
    };
    same_affine(*a_transform, *b_transform)
        && a_rule == b_rule
        && same_rect(*a_rect, *b_rect)
        && a_composite.blend == b_composite.blend
        && a_composite.alpha.to_bits() == b_composite.alpha.to_bits()
        && same_option(
            a_brush_transform.as_ref(),
            b_brush_transform.as_ref(),
            |a, b| same_affine(*a, *b),
        )
        && match (a_brush, b_brush) {
            (Brush::Solid(a), Brush::Solid(b)) => {
                a.components.map(f32::to_bits) == b.components.map(f32::to_bits)
            }
            (Brush::Image(a), Brush::Image(b)) => {
                a.sampler.x_extend == b.sampler.x_extend
                    && a.sampler.y_extend == b.sampler.y_extend
                    && a.sampler.quality == b.sampler.quality
                    && a.sampler.alpha.to_bits() == b.sampler.alpha.to_bits()
                    && same_image(&a.image, &b.image)
            }
            _ => false,
        }
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
pub(crate) mod testing {
    std::thread_local! { static DISABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
    pub(super) fn disabled() -> bool {
        DISABLED.get()
    }
    pub(crate) fn without<T>(run: impl FnOnce() -> T) -> T {
        struct Restore(bool);
        impl Drop for Restore {
            fn drop(&mut self) {
                DISABLED.set(self.0);
            }
        }
        let _restore = Restore(DISABLED.replace(true));
        run()
    }
}
