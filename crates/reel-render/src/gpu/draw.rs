use super::{GpuError, mesh, renderer::reserve, replay::Context, resources, targets::Target};
use reel_ui::frame::{FrameDraw, RecordLocation, RenderFrame};
use reel_ui::{
    imaging::record::{Clip, Geometry},
    kurbo::{Affine, Rect},
    peniko::{Brush, Extend, Fill, color::LinearSrgb},
};
use std::sync::Arc;
use wgpu::util::DeviceExt;
impl Context<'_> {
    fn mesh_allocation(&self) -> mesh::Allocation {
        mesh::Allocation::new(
            self.budgets,
            &self.budget_scope,
            self.gpu.limits.transient_bytes,
            self.gpu.device.limits().max_buffer_size,
        )
    }
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
        let mut mesh = mesh::tessellate(shape, fill, stroke, at, self.mesh_allocation())?;
        mesh::transform(
            &mut mesh,
            outer * transform,
            self.size,
            Affine::IDENTITY,
            [1.; 4],
            at,
        )?;
        let white = self.white()?;
        self.paint(target, &mesh, &white.view)
    }
    fn white(&mut self) -> Result<Arc<resources::Image>, GpuError> {
        if let Some(image) = &self.white {
            return Ok(image.clone());
        }
        // The image never escapes this Context. Its upload is encoded here;
        // the existing ticket follows Prepared through observed completion.
        let image = Arc::new(self.upload([1, 1], &[1.; 4])?);
        self.white = Some(image.clone());
        Ok(image)
    }
    fn upload(&mut self, size: [u32; 2], pixels: &[f32]) -> Result<resources::Image, GpuError> {
        let available = self
            .gpu
            .limits
            .resource_bytes
            .saturating_sub(self.budget_scope.resources.usage().bytes);
        let cost = resources::upload_cost(size)?;
        self.image_cache.make_room(
            &self.budgets.resources,
            cost,
            self.gpu.limits.resource_bytes,
        );
        self.tickets.push(reserve(
            &self.budgets.resources,
            &self.budget_scope.resources,
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
        let mut mesh = mesh::tessellate(shape, fill, stroke, at, self.mesh_allocation())?;
        let (image, uv, color) = match brush {
            Brush::Solid(c) => {
                let color = c
                    .convert::<LinearSrgb>()
                    .premultiply()
                    .components
                    .map(|v| v * composite.alpha);
                (self.white()?, Affine::IDENTITY, color)
            }
            Brush::Image(brush) => {
                if brush.sampler.quality != reel_ui::peniko::ImageQuality::Medium {
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
                } else if let Some(image) = self.image_cache.get(brush.image) {
                    self.tickets.push(super::renderer::Reservation {
                        _shared: None,
                        _local: super::image_cache::reserve_local(
                            &self.budget_scope.resources,
                            image.residency.as_ref().unwrap().bytes(),
                            self.gpu.limits.resource_bytes,
                        )?,
                    });
                    self.images.insert(brush.image, image.clone());
                    image
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
                    let reel_ui::resources::ResourceData::Image { image, .. } = lease.data() else {
                        return Err(GpuError::Unsupported {
                            at,
                            record: "image type",
                        });
                    };
                    let cost = resources::upload_cost([image.width, image.height])?;
                    let resident = u64::from(image.width) * u64::from(image.height) * 8;
                    let (upload, residency) = self.image_cache.reserve_upload(
                        &self.budgets.resources,
                        &self.budget_scope.resources,
                        cost,
                        resident,
                        self.gpu.limits.resource_bytes,
                    )?;
                    self.tickets.push(upload);
                    let (mut image, _) = resources::image(
                        self.gpu,
                        &mut self.encoder,
                        frame,
                        brush.image,
                        at,
                        cost,
                    )?;
                    image.residency = Some(residency);
                    image.source = Some(lease.clone());
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
        run: &reel_ui::frame::FrameGlyphRun,
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
        if !matches!(run.style, reel_ui::peniko::Style::Fill(_)) || run.glyph_transform.is_some() {
            return Err(GpuError::Unsupported {
                at,
                record: "glyph stroke or glyph transform",
            });
        }
        let color = color
            .convert::<LinearSrgb>()
            .premultiply()
            .components
            .map(|x| x * run.composite.alpha);
        for index in 0..run.glyphs.len() {
            let available = self
                .gpu
                .limits
                .resource_bytes
                .saturating_sub(self.budget_scope.resources.usage().bytes);
            let Some(bitmap) = super::glyph::rasterize(
                frame,
                run,
                index,
                at,
                available,
                self.gpu.device.limits().max_texture_dimension_2d,
            )?
            else {
                continue;
            };
            let image = self.upload(bitmap.size, &bitmap.pixels)?;
            let shape = Geometry::Rect(Rect::new(
                bitmap.x,
                bitmap.y,
                bitmap.x + f64::from(bitmap.size[0]),
                bitmap.y + f64::from(bitmap.size[1]),
            ));
            let mut mesh =
                mesh::tessellate(&shape, Fill::NonZero, None, at, self.mesh_allocation())?;
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
            multiview_mask: None,
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
