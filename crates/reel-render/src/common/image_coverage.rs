//! Shared conservative image geometry. Resource support uses visible texels;
//! each authored rectangle keeps its original UV transform and source window.
use reel_core::{
    image_coverage::{IMAGE_COVERAGE_BANDS, ImageCoverage},
    texture::TextureLayout,
};
use reel_ui::{
    frame::{FrameImage, ImageAlphaLayout},
    kurbo::{Affine, Rect},
    peniko::{Extend, ImageBrush},
};

pub(crate) struct Quads {
    rectangles: [Rect; IMAGE_COVERAGE_BANDS],
    len: usize,
}
impl Quads {
    pub(crate) fn iter(&self) -> impl Iterator<Item = Rect> + '_ {
        self.rectangles[..self.len].iter().copied()
    }
}

/// Trim only a proven one-to-one source domain. Repetition, packed alpha,
/// transformed source axes and samples outside a clamp window retain the
/// original rectangle. Native color adjustments and subtree effects preserve
/// the zero source pixels removed here. The band's filter border is already
/// present in resource metadata; output rectangles share edges without overlap.
pub(crate) fn quads(
    bounds: Rect,
    brush: &ImageBrush<FrameImage>,
    brush_transform: Affine,
    physical_transform: Affine,
    layout: TextureLayout,
    coverage: Option<&ImageCoverage>,
) -> Quads {
    let mut output = Quads {
        rectangles: [Rect::ZERO; IMAGE_COVERAGE_BANDS],
        len: 1,
    };
    output.rectangles[0] = bounds;
    let Some(coverage) = coverage else {
        return output;
    };
    let [a, b, c, d, e, f] = brush_transform.as_coeffs();
    if brush.sampler.x_extend != Extend::Pad
        || brush.sampler.y_extend != Extend::Pad
        || brush
            .image
            .appearance
            .is_some_and(|appearance| appearance.alpha_layout != ImageAlphaLayout::Native)
        || b != 0.
        || c != 0.
        || a == 0.
        || d == 0.
        || ![a, d, e, f, bounds.x0, bounds.y0, bounds.x1, bounds.y1]
            .into_iter()
            .all(f64::is_finite)
        || bounds.width() <= 0.
        || bounds.height() <= 0.
        || coverage.size() != layout.content_size
    {
        return output;
    }
    if coverage.is_full() {
        return output;
    }
    let full = Rect::new(
        0.,
        0.,
        f64::from(layout.logical_size[0]),
        f64::from(layout.logical_size[1]),
    );
    let window = brush.image.source_rect.unwrap_or(full);
    let source = brush_transform.transform_rect_bbox(bounds);
    // Author layout composes affine scales in f64. A boundary mapped onto its
    // source window may differ by a few ulps, well inside the texel border.
    let epsilon: [f64; 2] = layout
        .logical_size
        .map(|n| f64::from(n) * f64::EPSILON * 8.);
    if window.width() <= 0.
        || window.height() <= 0.
        || ![window.x0, window.y0, window.x1, window.y1]
            .into_iter()
            .all(f64::is_finite)
        || window.x0 < full.x0
        || window.y0 < full.y0
        || window.x1 > full.x1
        || window.y1 > full.y1
        || source.x0 < window.x0 - epsilon[0]
        || source.y0 < window.y0 - epsilon[1]
        || source.x1 > window.x1 + epsilon[0]
        || source.y1 > window.y1 + epsilon[1]
    {
        return output;
    }
    if coverage.is_empty() {
        output.len = 0;
        return output;
    }
    let source_from_pixel = brush_transform * physical_transform.inverse();
    let [px0, py0, px1, py1, _, _] = source_from_pixel.as_coeffs();
    // Coverage samples and the fragment's interpolation point may differ within
    // one physical pixel. This box encloses their distance for all MSAA patterns.
    let pixel_border = [px0.abs() + px1.abs(), py0.abs() + py1.abs()];
    if !pixel_border.into_iter().all(f64::is_finite) {
        return output;
    }
    output.len = 0;
    let scale: [f64; 2] = std::array::from_fn(|axis| {
        f64::from(layout.logical_size[axis]) / f64::from(layout.content_size[axis])
    });
    let mut supports = [Rect::ZERO; IMAGE_COVERAGE_BANDS];
    let mut support_count = 0;
    let mut top = source.y1;
    let mut bottom = source.y0;
    for [x0, y0, x1, y1] in coverage.rectangles() {
        let covered = Rect::new(
            f64::from(x0) * scale[0],
            f64::from(y0) * scale[1],
            f64::from(x1) * scale[0],
            f64::from(y1) * scale[1],
        )
        .inflate(pixel_border[0], pixel_border[1])
        .intersect(source);
        if covered.width() <= 0. || covered.height() <= 0. {
            continue;
        }
        top = top.min(covered.y0);
        bottom = bottom.max(covered.y1);
        supports[support_count] = covered;
        support_count += 1;
    }
    if support_count == 0 {
        return output;
    }
    // Rebuild the union as disjoint bands after pixel padding. Inflating output
    // quads directly would overlap and apply source-over blending twice.
    // Minified portraits need fewer primitives. Preserve each band's complete
    // support union while keeping strips roughly two 32-pixel tiles tall;
    // fine transparent geometry can cost more than the zero-alpha fill it saves.
    let [_, _, physical_x, physical_y, _, _] = physical_transform.as_coeffs();
    let height = (bottom - top) / d.abs() * physical_x.hypot(physical_y);
    let bands = ((height / 64.).ceil() as usize).clamp(1, IMAGE_COVERAGE_BANDS);
    let step = (bottom - top) / bands as f64;
    for band in 0..bands {
        let y0 = top + step * band as f64;
        let y1 = if band + 1 == bands {
            bottom
        } else {
            top + step * (band + 1) as f64
        };
        let mut covered = Rect::new(source.x1, y0, source.x0, y1);
        for support in &supports[..support_count] {
            if support.y1 >= y0 && support.y0 <= y1 {
                covered.x0 = covered.x0.min(support.x0);
                covered.x1 = covered.x1.max(support.x1);
            }
        }
        if covered.width() <= 0. || covered.height() <= 0. {
            continue;
        }
        let x0 = (covered.x0 - e) / a;
        let x1 = (covered.x1 - e) / a;
        let y0 = (covered.y0 - f) / d;
        let y1 = (covered.y1 - f) / d;
        // Intersection also keeps division roundoff inside the authored shape.
        let rect = Rect::new(x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)).intersect(bounds);
        if rect.width() <= 0. || rect.height() <= 0. {
            continue;
        }
        if output.len > 0 {
            let previous = &mut output.rectangles[output.len - 1];
            if previous.x0 == rect.x0
                && previous.x1 == rect.x1
                && (previous.y0 == rect.y1 || previous.y1 == rect.y0)
            {
                *previous = previous.union(rect);
                continue;
            }
        }
        output.rectangles[output.len] = rect;
        output.len += 1;
    }
    output
}

#[cfg(test)]
#[path = "image_coverage_tests.rs"]
mod tests;
