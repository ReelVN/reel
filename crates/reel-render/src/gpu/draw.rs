use super::{GpuError, mesh, renderer::reserve, replay::Context, resources, targets::Target};
use masonry_core::{
    imaging::record::{Clip, Geometry},
    kurbo::{Affine, Rect},
    peniko::{Brush, Extend, Fill},
};
use reel_stage::{FrameDraw, RecordLocation, RenderFrame};
use std::sync::Arc;
use wgpu::util::DeviceExt;
impl Context<'_> {
    pub(super) fn clip(
        &mut self,
        target: &Target,
        clip: &Clip,
        outer: Affine,
        at: RecordLocation,
    ) -> Result<(), GpuError> {
        let (transform, shape, fill, stroke) = match clip {
            Clip::Fill {
                transform,
                shape,
                fill_rule,
            } => (*transform, shape, *fill_rule, None),
            Clip::Stroke {
                transform,
                shape,
                stroke,
            } => (*transform, shape, Fill::NonZero, Some(stroke)),
        };
        let _scratch = reserve(
            &self.budgets.transient,
            2 << 20,
            self.gpu.limits.transient_bytes,
            "mesh scratch",
        )?;
        let mut mesh = mesh::tessellate(shape, fill, stroke, at, 1 << 20)?;
        mesh::transform(
            &mut mesh,
            outer * transform,
            self.size,
            Affine::IDENTITY,
            [1.; 4],
            at,
        )?;
        let white = self.upload([1, 1], &[1.; 4])?;
        self.paint(target, &mesh, &white.view)
    }
    fn upload(&mut self, size: [u32; 2], pixels: &[f32]) -> Result<resources::Image, GpuError> {
        let available = self
            .gpu
            .limits
            .resource_bytes
            .saturating_sub(self.budgets.resources.usage().bytes);
        let cost = resources::upload_cost(size)?;
        self.tickets.push(reserve(
            &self.budgets.resources,
            cost,
            self.gpu.limits.resource_bytes,
            "resources",
        )?);
        let (image, _) = resources::upload(self.gpu, &mut self.encoder, size, pixels, available)?;
        Ok(image)
    }
    pub(super) fn draw(
        &mut self,
        frame: &RenderFrame,
        target: &Target,
        draw: &FrameDraw,
        outer: Affine,
        at: RecordLocation,
    ) -> Result<(), GpuError> {
        let (transform, shape, fill, stroke, brush, brush_transform, composite) = match draw {
            FrameDraw::Fill {
                transform,
                shape,
                fill_rule,
                brush,
                brush_transform,
                composite,
            } => (
                *transform,
                shape,
                *fill_rule,
                None,
                brush,
                *brush_transform,
                *composite,
            ),
            FrameDraw::Stroke {
                transform,
                shape,
                stroke,
                brush,
                brush_transform,
                composite,
            } => (
                *transform,
                shape,
                Fill::NonZero,
                Some(stroke),
                brush,
                *brush_transform,
                *composite,
            ),
            FrameDraw::GlyphRun(run) => return self.glyphs(frame, target, run, outer, at),
            FrameDraw::BlurredRoundedRect(_) => {
                return Err(GpuError::Unsupported {
                    at,
                    record: "blurred rounded rect",
                });
            }
        };
        self.composite_supported(composite, at)?;
        let _scratch = reserve(
            &self.budgets.transient,
            2 << 20,
            self.gpu.limits.transient_bytes,
            "mesh scratch",
        )?;
        let mut mesh = mesh::tessellate(shape, fill, stroke, at, 1 << 20)?;
        let (image, uv, color) = match brush {
            Brush::Solid(c) => {
                let rgba = c.components.map(f64::from);
                let color =
                    crate::color::linear_premultiplied(rgba).map(|v| v as f32 * composite.alpha);
                (
                    Arc::new(self.upload([1, 1], &[1.; 4])?),
                    Affine::IDENTITY,
                    color,
                )
            }
            Brush::Image(brush) => {
                if brush.sampler.quality != masonry_core::peniko::ImageQuality::Medium {
                    return Err(GpuError::Unsupported {
                        at,
                        record: "non-bilinear image sampling",
                    });
                }
                if brush.sampler.x_extend != Extend::Pad || brush.sampler.y_extend != Extend::Pad {
                    return Err(GpuError::Unsupported {
                        at,
                        record: "image repeat/reflect",
                    });
                }
                let image = if let Some(image) = self.images.get(&brush.image) {
                    image.clone()
                } else {
                    let lease = frame
                        .resources()
                        .leases()
                        .iter()
                        .find(|r| r.handle() == brush.image)
                        .ok_or(GpuError::Unsupported {
                            at,
                            record: "missing image",
                        })?;
                    let reel_stage::ResourceData::Image { image, .. } = lease.data() else {
                        return Err(GpuError::Unsupported {
                            at,
                            record: "image type",
                        });
                    };
                    let cost = resources::upload_cost([image.width, image.height])?;
                    self.tickets.push(reserve(
                        &self.budgets.resources,
                        cost,
                        self.gpu.limits.resource_bytes,
                        "image upload",
                    )?);
                    let (image, _) = resources::image(
                        self.gpu,
                        &mut self.encoder,
                        frame,
                        brush.image,
                        at,
                        cost,
                    )?;
                    let image = Arc::new(image);
                    self.images.insert(brush.image, image.clone());
                    image
                };
                let bt = brush_transform.unwrap_or(Affine::IDENTITY);
                if bt.determinant() == 0. {
                    return Err(GpuError::Unsupported {
                        at,
                        record: "singular brush transform",
                    });
                }
                let uv = Affine::scale_non_uniform(
                    1. / f64::from(image.size[0]),
                    1. / f64::from(image.size[1]),
                ) * bt.inverse();
                (image, uv, [composite.alpha * brush.sampler.alpha; 4])
            }
            Brush::Gradient(_) => {
                return Err(GpuError::Unsupported {
                    at,
                    record: "gradient",
                });
            }
        };
        mesh::transform(&mut mesh, outer * transform, self.size, uv, color, at)?;
        self.paint(target, &mesh, &image.view)
    }
    fn glyphs(
        &mut self,
        frame: &RenderFrame,
        target: &Target,
        run: &reel_stage::FrameGlyphRun,
        outer: Affine,
        at: RecordLocation,
    ) -> Result<(), GpuError> {
        self.composite_supported(run.composite, at)?;
        let Brush::Solid(color) = &run.brush else {
            return Err(GpuError::Unsupported {
                at,
                record: "non-solid glyph brush",
            });
        };
        if !matches!(run.style, masonry_core::peniko::Style::Fill(_))
            || run.glyph_transform.is_some()
        {
            return Err(GpuError::Unsupported {
                at,
                record: "glyph stroke or glyph transform",
            });
        }
        let color = crate::color::linear_premultiplied(color.components.map(f64::from))
            .map(|x| x as f32 * run.composite.alpha);
        for index in 0..run.glyphs.len() {
            let available = self
                .gpu
                .limits
                .resource_bytes
                .saturating_sub(self.budgets.resources.usage().bytes);
            let Some(bitmap) = super::glyph::rasterize(frame, run, index, at, available)? else {
                continue;
            };
            let image = self.upload(bitmap.size, &bitmap.pixels)?;
            let shape = Geometry::Rect(Rect::new(
                bitmap.x,
                bitmap.y,
                bitmap.x + f64::from(bitmap.size[0]),
                bitmap.y + f64::from(bitmap.size[1]),
            ));
            let _scratch = reserve(
                &self.budgets.transient,
                8192,
                self.gpu.limits.transient_bytes,
                "glyph mesh scratch",
            )?;
            let mut mesh = mesh::tessellate(&shape, Fill::NonZero, None, at, 4096)?;
            let uv = Affine::scale_non_uniform(
                1. / f64::from(bitmap.size[0]),
                1. / f64::from(bitmap.size[1]),
            ) * Affine::translate((-bitmap.x, -bitmap.y));
            mesh::transform(&mut mesh, outer * run.transform, self.size, uv, color, at)?;
            self.paint(target, &mesh, &image.view)?;
        }
        Ok(())
    }
    fn paint(
        &mut self,
        target: &Target,
        mesh: &mesh::Mesh,
        image: &wgpu::TextureView,
    ) -> Result<(), GpuError> {
        if mesh.indices.is_empty() {
            return Ok(());
        }
        self.transient(
            (mesh.vertices.len() * size_of::<mesh::Vertex>() + mesh.indices.len() * 4) as u64,
        )?;
        let device = &self.gpu.device;
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("reel vertices"),
            contents: bytemuck::cast_slice(&mesh.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("reel indices"),
            contents: bytemuck::cast_slice(&mesh.indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("reel brush"),
            layout: &self.pipelines.paint_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(image),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.pipelines.sampler),
                },
            ],
        });
        let mut pass = self.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("reel draw"),
            color_attachments: &[Some(target.attachment(false))],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(&self.pipelines.paint);
        pass.set_bind_group(0, &group, &[]);
        pass.set_vertex_buffer(0, vertices.slice(..));
        pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..mesh.indices.len() as u32, 0, 0..1);
        Ok(())
    }
}
