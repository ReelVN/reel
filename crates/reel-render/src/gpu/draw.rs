use super::{GpuError, mesh, renderer::reserve, replay::Context, resources, targets::Target};
use reel_ui::frame::{FrameDraw, ImageColorGradient, RecordLocation, RenderFrame};
use reel_ui::{
    imaging::record::Clip,
    kurbo::{Affine, Rect},
    peniko::{Brush, Extend, Fill, color::LinearSrgb},
};
use std::sync::Arc;
use wgpu::util::DeviceExt;
impl Context<'_> {
    pub(super) fn mesh_allocation(&self) -> mesh::Allocation {
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
        self.glyph_cache.make_room(
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
        let (image, uv, color, repeat, gradient) = match brush {
            Brush::Solid(c) => {
                let color = c
                    .convert::<LinearSrgb>()
                    .premultiply()
                    .components
                    .map(|v| v * composite.alpha);
                (self.white()?, Affine::IDENTITY, color, [false; 2], None)
            }
            Brush::Image(brush) => {
                if brush.sampler.quality != reel_ui::peniko::ImageQuality::Medium {
                    return Err(GpuError::Unsupported {
                        at,
                        record: "non-bilinear image sampling",
                    });
                }
                if brush.sampler.x_extend == Extend::Reflect
                    || brush.sampler.y_extend == Extend::Reflect
                {
                    return Err(GpuError::Unsupported {
                        at,
                        record: "image reflect",
                    });
                }
                let image = self.image_resource(frame, brush.image.resource, at)?;
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
                let alpha = composite.alpha * brush.sampler.alpha;
                (
                    image,
                    uv,
                    brush.image.color.map(|channel| channel * alpha),
                    [
                        brush.sampler.x_extend == Extend::Repeat,
                        brush.sampler.y_extend == Extend::Repeat,
                    ],
                    brush
                        .image
                        .gradient
                        .map(|gradient| gradient.transformed(outer)),
                )
            }
            Brush::Gradient(_) => {
                return Err(GpuError::Unsupported {
                    at,
                    record: "gradient",
                });
            }
        };
        mesh::transform(&mut mesh, outer * transform, self.size, uv, color, at)?;
        self.paint_image(target, &mesh, &image.view, repeat, gradient)
    }
    pub(super) fn image_resource(
        &mut self,
        frame: &RenderFrame,
        handle: reel_ui::resources::ResourceHandle,
        at: RecordLocation,
    ) -> Result<Arc<resources::Image>, GpuError> {
        Ok(if let Some(image) = self.images.get(&handle) {
            image.clone()
        } else if let Some(image) = self.image_cache.get(handle) {
            self.tickets.push(super::renderer::Reservation {
                _shared: None,
                _local: super::image_cache::reserve_local(
                    &self.budget_scope.resources,
                    image.residency.as_ref().unwrap().bytes(),
                    self.gpu.limits.resource_bytes,
                )?,
            });
            self.images.insert(handle, image.clone());
            image
        } else {
            let lease = frame
                .resources()
                .leases()
                .iter()
                .find(|r| r.handle() == handle)
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
            self.glyph_cache.make_room(
                &self.budgets.resources,
                cost,
                self.gpu.limits.resource_bytes,
            );
            let (upload, residency) = self.image_cache.reserve_upload(
                &self.budgets.resources,
                &self.budget_scope.resources,
                cost,
                resident,
                self.gpu.limits.resource_bytes,
            )?;
            self.tickets.push(upload);
            let (mut image, _) =
                resources::image(self.gpu, &mut self.encoder, frame, handle, at, cost)?;
            image.residency = Some(residency);
            image.source = Some(lease.clone());
            let image = Arc::new(image);
            self.images.insert(handle, image.clone());
            image
        })
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
        let color = color
            .convert::<LinearSrgb>()
            .premultiply()
            .components
            .map(|x| x * run.composite.alpha);
        let mut page: Option<Arc<super::glyph_cache::Page>> = None;
        let mut batch = mesh::Mesh::new(self.mesh_allocation());
        let run_key = Arc::new(super::glyph_cache::RunKey::new(run));
        for index in 0..run.glyphs.len() {
            let key = super::glyph_cache::Key::new(run_key.clone(), run.glyphs[index].id);
            let glyph = if let Some(glyph) = self.glyphs.entries.get(&key) {
                glyph.clone()
            } else {
                let glyph = match self.glyph_cache.get(&key) {
                    Some(glyph) => glyph,
                    None => self.glyph_cache.rasterize(
                        self.gpu,
                        &mut self.encoder,
                        frame,
                        run,
                        index,
                        at,
                        self.budgets,
                        &self.budget_scope,
                        &mut self.tickets,
                        self.image_cache,
                    )?,
                };
                super::glyph_cache::GlyphCache::charge(
                    &mut self.glyphs,
                    &glyph,
                    &self.budget_scope.resources,
                    self.gpu.limits.resource_bytes,
                    &mut self.tickets,
                )?;
                self.glyphs.entries.insert(key, glyph.clone());
                glyph
            };
            let super::glyph_cache::Glyph::Image {
                page: glyph_page,
                origin,
                size,
                offset,
                ..
            } = glyph.as_ref()
            else {
                continue;
            };
            if let Some(previous) = &page
                && !Arc::ptr_eq(previous, glyph_page)
            {
                mesh::transform_glyphs(&mut batch, outer * run.transform, self.size, color, at)?;
                self.paint(target, &batch, &previous.view)?;
                batch = mesh::Mesh::new(self.mesh_allocation());
            }
            page = Some(glyph_page.clone());
            let x = f64::from(run.glyphs[index].x) + offset[0];
            let y = f64::from(run.glyphs[index].y) + offset[1];
            let bounds = Rect::new(x, y, x + f64::from(size[0]), y + f64::from(size[1]));
            let visible = (outer * run.transform)
                .transform_rect_bbox(bounds)
                .inflate(1., 1.)
                .intersect(Rect::new(
                    0.,
                    0.,
                    f64::from(self.size[0]),
                    f64::from(self.size[1]),
                ));
            if visible.width() <= 0. || visible.height() <= 0. {
                continue;
            }
            let uv = Rect::new(
                f64::from(origin[0]) / f64::from(glyph_page.size[0]),
                f64::from(origin[1]) / f64::from(glyph_page.size[1]),
                f64::from(origin[0] + size[0]) / f64::from(glyph_page.size[0]),
                f64::from(origin[1] + size[1]) / f64::from(glyph_page.size[1]),
            );
            batch.quad(bounds, uv)?;
        }
        if let Some(page) = page {
            mesh::transform_glyphs(&mut batch, outer * run.transform, self.size, color, at)?;
            self.paint(target, &batch, &page.view)?;
        }
        Ok(())
    }
    pub(super) fn paint(
        &mut self,
        target: &Target,
        mesh: &mesh::Mesh,
        image: &wgpu::TextureView,
    ) -> Result<(), GpuError> {
        self.paint_image(target, mesh, image, [false; 2], None)
    }

    pub(super) fn paint_image(
        &mut self,
        target: &Target,
        mesh: &mesh::Mesh,
        image: &wgpu::TextureView,
        repeat: [bool; 2],
        gradient: Option<ImageColorGradient>,
    ) -> Result<(), GpuError> {
        if mesh.indices.is_empty()
            || self.scissor.empty()
            || gradient.is_some_and(|gradient| gradient.transform.determinant() == 0.)
        {
            return Ok(());
        }
        #[cfg(test)]
        self.gpu
            .upload_creations
            .draws
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.transient(
            (mesh.vertices.len() * size_of::<mesh::Vertex>() + mesh.indices.len() * 4) as u64,
        )?;
        if gradient.is_some() {
            self.transient(96)?;
        }
        let device = &self.gpu.device;
        let gradient_buffer = gradient.map(|gradient| {
            let inverse = gradient.transform.inverse().as_coeffs();
            let mut values = [[0f32; 4]; 6];
            values[..4].copy_from_slice(&gradient.colors);
            values[4] = [inverse[0] as f32, inverse[2] as f32, inverse[4] as f32, 1.];
            values[5] = [inverse[1] as f32, inverse[3] as f32, inverse[5] as f32, 0.];
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("image corner colors"),
                contents: bytemuck::cast_slice(&values),
                usage: wgpu::BufferUsages::UNIFORM,
            })
        });
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
        #[cfg(test)]
        {
            self.gpu
                .upload_creations
                .mesh_buffers
                .fetch_add(2, std::sync::atomic::Ordering::Relaxed);
            self.gpu
                .upload_creations
                .brush_groups
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
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
                    resource: wgpu::BindingResource::Sampler(
                        &self.pipelines.samplers
                            [usize::from(repeat[0]) + 2 * usize::from(repeat[1])],
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: gradient_buffer
                        .as_ref()
                        .unwrap_or(&self.pipelines.plain_gradient)
                        .as_entire_binding(),
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
        self.scissor.apply(&mut pass);
        pass.set_bind_group(0, &group, &[]);
        pass.set_vertex_buffer(0, vertices.slice(..));
        pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..mesh.indices.len() as u32, 0, 0..1);
        Ok(())
    }
}
