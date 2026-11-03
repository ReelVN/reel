//! Lazy comparison of the coordinates submitted by ordinary image batches.
use crate::common::{image_coverage, mesh, resources::TextureUploadLayout};
use reel_core::texture::TextureLayout;
use reel_ui::{
    frame::{FrameDraw, FrameDrawing, FrameImage, ImageAlphaLayout},
    imaging::{Composite, record::Geometry},
    kurbo::{Affine, Rect},
    peniko::{Brush, Extend, ImageBrush, ImageQuality, ImageSampler},
    resources::{ResourceHandle, ResourceView, ResourceWeak},
};

/// Keep a small raster recipe and weak lookup of immutable coverage metadata,
/// rather than copying a maximum-sized vertex stream through every cache probe.
/// Expired coverage makes a changed-transform comparison miss; the recipe does
/// not extend the source pixels' normal retirement lifetime.
///
/// Paint colors, appearance uniforms, sampler state and recording identity stay
/// in the surrounding image key. Only their already-equal coordinate payload is
/// compared here. The caller establishes that no mesh clip changes this batch.
#[derive(Clone)]
pub(crate) struct Key {
    bounds: Rect,
    physical: Affine,
    brush: Affine,
    size: [u32; 2],
    layout: TextureLayout,
    origin: [u32; 2],
    source_rect: Option<Rect>,
    extend: [Extend; 2],
    atlas: bool,
    coverage: bool,
    handle: ResourceHandle,
    source: ResourceWeak,
}

/// Describe the Rect/default-blend image path shared by both renderers.
///
/// The WGPU path omits an entirely invisible rectangle before generating its
/// mesh; those invocations and unsupported image paths keep the affine key.
/// Per-backend color multiplication remains in the surrounding paint key.
pub(crate) fn image(
    drawing: &FrameDrawing,
    draw: &FrameDraw,
    outer: Affine,
    source_size: [u32; 2],
) -> Option<Key> {
    let FrameDraw::Fill {
        transform,
        shape: Geometry::Rect(bounds),
        brush: Brush::Image(brush),
        brush_transform,
        composite,
        ..
    } = draw
    else {
        return None;
    };
    if composite.blend != Composite::default().blend
        || brush.sampler.quality == ImageQuality::High
        || brush.sampler.x_extend == Extend::Reflect
        || brush.sampler.y_extend == Extend::Reflect
    {
        return None;
    }
    let physical = outer * *transform;
    let visible = physical
        .transform_rect_bbox(*bounds)
        .inflate(1., 1.)
        .intersect(Rect::new(
            0.,
            0.,
            f64::from(source_size[0]),
            f64::from(source_size[1]),
        ));
    if !(visible.width() > 0. && visible.height() > 0.) {
        return None;
    }
    let source = drawing
        .resources()
        .leases()
        .iter()
        .find(|lease| lease.handle() == brush.image.resource)?;
    let (layout, origin, atlas) = match source.view() {
        ResourceView::Image { image } if image.format == reel_ui::peniko::ImageFormat::Rgba8 => (
            TextureLayout::identity([image.width, image.height]),
            [0; 2],
            false,
        ),
        ResourceView::Texture { descriptor } => (
            TextureUploadLayout::new(descriptor).ok()?.layout,
            [0; 2],
            false,
        ),
        ResourceView::AtlasView { image, backing, .. } => {
            let storage = TextureUploadLayout::new(backing.texture_descriptor()?)
                .ok()?
                .layout
                .storage_size;
            image.validate(storage).ok()?;
            (
                TextureLayout {
                    logical_size: image.logical_size,
                    content_size: image.content_size,
                    storage_size: storage,
                },
                image.content_origin,
                true,
            )
        }
        _ => return None,
    };
    Some(Key {
        bounds: *bounds,
        physical,
        brush: brush_transform.unwrap_or(Affine::IDENTITY),
        size: source_size,
        layout,
        origin,
        source_rect: brush.image.source_rect,
        extend: [brush.sampler.x_extend, brush.sampler.y_extend],
        atlas,
        // Atlas Image owners have no independent source coverage in either
        // backend. Packed-alpha appearance also keeps the original rectangle.
        coverage: !atlas
            && source.image_coverage().is_some()
            && brush
                .image
                .appearance
                .is_none_or(|appearance| appearance.alpha_layout == ImageAlphaLayout::Native),
        handle: source.handle(),
        source: source.downgrade(),
    })
}

impl Key {
    fn normalized_origin(&self) -> [f64; 2] {
        std::array::from_fn(|i| f64::from(self.origin[i]) / f64::from(self.layout.storage_size[i]))
    }
    fn uv(&self) -> Affine {
        let [x, y] = self.layout.uv_scale();
        let [ox, oy] = self.normalized_origin();
        Affine::translate((ox, oy)) * Affine::scale_non_uniform(x, y) * self.brush
    }
    fn repeat(&self) -> [bool; 2] {
        self.extend.map(|extend| extend == Extend::Repeat)
    }
    fn window(&self) -> [u32; 4] {
        mesh::ImageSampling::for_image(
            self.source_rect,
            self.layout,
            self.normalized_origin(),
            self.repeat(),
            self.atlas,
        )
        .window()
        .map(f32::to_bits)
    }
    fn quads(&self) -> Option<image_coverage::Quads> {
        let source = if self.coverage {
            Some(self.source.upgrade()?)
        } else {
            None
        };
        // These are exactly the brush fields read by image_coverage::quads.
        // Non-native alpha and atlas coverage have already selected `None`.
        let brush = ImageBrush {
            image: FrameImage {
                resource: self.handle,
                color: [1.; 4],
                source_rect: self.source_rect,
                appearance: None,
            },
            sampler: ImageSampler {
                x_extend: self.extend[0],
                y_extend: self.extend[1],
                ..Default::default()
            },
        };
        Some(image_coverage::quads(
            self.bounds,
            &brush,
            self.brush,
            self.physical,
            self.layout,
            source.as_ref().and_then(|source| source.image_coverage()),
        ))
    }
}

impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        if self.handle != other.handle
            || self.size != other.size
            || self.layout != other.layout
            || self.origin != other.origin
            || self.extend != other.extend
            || self.atlas != other.atlas
            || self.coverage != other.coverage
        {
            return false;
        }
        if super::rect(self.bounds) == super::rect(other.bounds)
            && super::affine(self.physical) == super::affine(other.physical)
            && super::affine(self.brush) == super::affine(other.brush)
            && self.source_rect.map(super::rect) == other.source_rect.map(super::rect)
        {
            return true;
        }
        if self.window() != other.window() {
            return false;
        }
        let (Some(left), Some(right)) = (self.quads(), other.quads()) else {
            return false;
        };
        let mut left = left.iter();
        let mut right = right.iter();
        let left_uv = self.uv();
        let right_uv = other.uv();
        loop {
            let (a, b) = match (left.next(), right.next()) {
                (Some(a), Some(b)) => (a, b),
                (None, None) => return true,
                _ => return false,
            };
            // Mesh::image_quad uses these four vertices and the same six
            // indices in this order for every rectangle on both backends.
            for (a, b) in mesh::quad_local(a).into_iter().zip(mesh::quad_local(b)) {
                if mesh::project_position(a, self.physical, self.size).map(f32::to_bits)
                    != mesh::project_position(b, other.physical, other.size).map(f32::to_bits)
                    || mesh::project_uv(a, left_uv).map(f32::to_bits)
                        != mesh::project_uv(b, right_uv).map(f32::to_bits)
                {
                    return false;
                }
            }
        }
    }
}
impl Eq for Key {}

#[cfg(test)]
mod tests;
