//! Consecutive image rectangles share one mesh, upload pair and texture binding.
//! This is part of the ordinary ordered renderer, including authored images.
use super::{GpuError, mesh, replay::Context, targets::Target};
use reel_ui::{
    frame::{FrameCommand, FrameDraw, FrameImage, FrameOp, RenderFrame},
    imaging::{Composite, record::Geometry},
    kurbo::{Affine, Rect},
    peniko::{Brush, Extend, ImageBrush, ImageQuality},
};

struct Quad<'a> {
    transform: Affine,
    bounds: Rect,
    brush: &'a ImageBrush<FrameImage>,
    brush_transform: Affine,
    alpha: f32,
}
fn quad(command: &FrameCommand) -> Option<Quad<'_>> {
    let FrameOp::Draw(FrameDraw::Fill {
        transform,
        shape: Geometry::Rect(bounds),
        brush: Brush::Image(brush),
        brush_transform,
        composite,
        ..
    }) = &command.op
    else {
        return None;
    };
    if composite.blend != Composite::default().blend
        || brush.sampler.quality != ImageQuality::Medium
        || brush.sampler.x_extend == Extend::Reflect
        || brush.sampler.y_extend == Extend::Reflect
    {
        return None;
    }
    Some(Quad {
        transform: *transform,
        bounds: *bounds,
        brush,
        brush_transform: brush_transform.unwrap_or(Affine::IDENTITY),
        alpha: composite.alpha * brush.sampler.alpha,
    })
}
impl Context<'_> {
    /// Returns the consumed ordered run; zero asks the existing generic path to draw.
    pub(super) fn image_batch(
        &mut self,
        frame: &RenderFrame,
        target: &Target,
        commands: &[FrameCommand],
        outer: Affine,
    ) -> Result<usize, GpuError> {
        let Some(first) = commands.first().and_then(quad) else {
            return Ok(0);
        };
        let handle = first.brush.image.resource;
        let image = self.image_resource(frame, handle, commands[0].origin.at)?;
        let mut batch = mesh::Mesh::new(self.mesh_allocation());
        let mut count = 0;
        for command in commands {
            let Some(quad) = quad(command) else {
                break;
            };
            if quad.brush.image.resource != handle
                || quad.brush.sampler.x_extend != first.brush.sampler.x_extend
                || quad.brush.sampler.y_extend != first.brush.sampler.y_extend
                || quad.brush.image.gradient != first.brush.image.gradient
            {
                break;
            }
            let at = command.origin.at;
            if quad.brush_transform.determinant() == 0. {
                return Err(GpuError::Unsupported {
                    at,
                    record: "singular brush transform",
                });
            }
            let transform = outer * quad.transform;
            let visible = transform
                .transform_rect_bbox(quad.bounds)
                .inflate(1., 1.)
                .intersect(Rect::new(
                    0.,
                    0.,
                    f64::from(self.size[0]),
                    f64::from(self.size[1]),
                ));
            if visible.width() > 0. && visible.height() > 0. {
                let uv = Affine::scale_non_uniform(
                    1. / f64::from(image.size[0]),
                    1. / f64::from(image.size[1]),
                ) * quad.brush_transform.inverse();
                batch.image_quad(
                    quad.bounds,
                    transform,
                    uv,
                    self.size,
                    quad.brush.image.color.map(|channel| channel * quad.alpha),
                    at,
                )?;
            }
            count += 1;
        }
        self.paint_image(
            target,
            &batch,
            &image.view,
            [
                first.brush.sampler.x_extend == Extend::Repeat,
                first.brush.sampler.y_extend == Extend::Repeat,
            ],
            first
                .brush
                .image
                .gradient
                .map(|gradient| gradient.transformed(outer)),
        )?;
        Ok(count)
    }
}
