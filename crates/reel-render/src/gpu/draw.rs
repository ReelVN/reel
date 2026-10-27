use super::{GpuError, mesh, renderer::reserve, replay::Context, resources, targets::Target};
use reel_ui::frame::{FrameDraw, FrameDrawing, ImageAppearance, RecordLocation};
use reel_ui::{
    imaging::record::Clip,
    kurbo::{Affine, Rect},
    peniko::{Brush, Extend, Fill, ImageQuality, color::LinearSrgb},
};
use std::sync::Arc;
use wgpu::util::DeviceExt;

pub(super) struct PaintBatch {
    target: Target,
    geometry: crate::common::draw_batch::Batch<PaintState>,
}
#[derive(PartialEq)]
struct VideoState {
    chroma: [wgpu::TextureView; 2],
    color: wgpu::Buffer,
}
#[derive(PartialEq)]
struct PaintState {
    image: wgpu::TextureView,
    texture_format: Option<reel_core::texture::TextureFormat>,
    coverage: bool,
    yuv: Option<VideoState>,
    repeat: [bool; 2],
    quality: ImageQuality,
    appearance: Option<super::gradient::Uniform>,
    direct: Option<mesh::DirectPaint>,
    opaque: Option<mesh::OpaquePaint>,
    scissor: super::targets::Scissor,
}

struct SampledImage<'a> {
    view: &'a wgpu::TextureView,
    coverage: bool,
    format: Option<reel_core::texture::TextureFormat>,
    yuv: Option<&'a super::video::Planes>,
    quality: ImageQuality,
    source: mesh::PaintSource,
}
impl<'a> SampledImage<'a> {
    fn coverage(view: &'a wgpu::TextureView) -> Self {
        Self {
            coverage: true,
            ..Self::plain(view)
        }
    }
    fn plain(view: &'a wgpu::TextureView) -> Self {
        Self {
            view,
            coverage: false,
            format: None,
            yuv: None,
            quality: ImageQuality::Medium,
            source: mesh::PaintSource::Texture,
        }
    }
}
impl Context<'_> {
    /// Sample a complete linear source through its actual local-to-parent map.
    /// Submission of the sampling draw precedes retirement of its scratch target.
    pub(super) fn project_source(
        &mut self,
        target: &Target,
        source: &Target,
        parent_to_source: Affine,
        opacity: f32,
        at: RecordLocation,
    ) -> Result<(), GpuError> {
        if parent_to_source.determinant() == 0. {
            return Ok(());
        }
        self.flush_draws()?;
        self.ensure_sampled(&source.view);
        let mut mesh = mesh::Mesh::new(self.mesh_allocation());
        crate::common::effect_plan::projected_source(
            &mut mesh,
            source.size,
            source.storage_size(),
            false,
        )?;
        mesh::transform_glyphs(
            &mut mesh,
            parent_to_source.inverse(),
            self.size,
            [opacity; 4],
            at,
        )?;
        self.paint_mesh(
            target,
            mesh,
            SampledImage::plain(&source.view),
            [false; 2],
            None,
        )?;
        self.flush_draws()
    }

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
        self.paint_solid(target, mesh)
    }
    fn paint_solid(
        &mut self,
        target: &Target,
        mesh: impl Into<crate::common::recording_cache::Reference>,
    ) -> Result<(), GpuError> {
        let white = self.white()?;
        self.paint_mesh(
            target,
            mesh,
            SampledImage {
                source: mesh::PaintSource::Solid,
                ..SampledImage::plain(&white.view)
            },
            [false; 2],
            None,
        )
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
        frame: &FrameDrawing,
        target: &Target,
        draw: &FrameDraw,
        outer: Affine,
        origin: reel_ui::frame::CommandOrigin,
    ) -> Result<(), GpuError> {
        let at = origin.at;
        if !self.capture && self.geometry_policy.reuse && frame.recording(origin).is_some() {
            self.geometry_policy.optional.set(true);
        }
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
            FrameDraw::GlyphRun(run) => return self.glyphs(frame, target, run, outer, origin),
            FrameDraw::BlurredRoundedRect(_) => {
                return Err(GpuError::Unsupported {
                    at,
                    record: "blurred rounded rect",
                });
            }
        };
        self.composite_supported(composite, at)?;
        let (image, uv, color, repeat, appearance, quality) = match brush {
            Brush::Solid(c) => {
                let color = c
                    .convert::<LinearSrgb>()
                    .premultiply()
                    .components
                    .map(|v| v * composite.alpha);
                let allocation = self.mesh_allocation();
                let mesh = self.geometry_cache.shapes.lock().unwrap().shape(
                    frame,
                    origin,
                    (shape, fill, stroke),
                    crate::common::recording_cache::ShapeDependency {
                        transform: outer * transform,
                        size: self.size,
                        uv: Affine::IDENTITY,
                        color,
                        sampling: Default::default(),
                    },
                    allocation,
                    crate::common::recording_cache::Scope {
                        budgets: self.budgets,
                        limit: self.gpu.limits.transient_bytes,
                        enabled: !self.capture && self.geometry_policy.reuse,
                    },
                )?;
                return self.paint_solid(target, mesh);
            }
            Brush::Image(brush) => {
                if brush.sampler.quality == ImageQuality::High {
                    return Err(GpuError::Unsupported {
                        at,
                        record: "high-quality image sampling",
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
                let image = self.image_resource(brush.image.resource, at)?;
                let bt = brush_transform.unwrap_or(Affine::IDENTITY);
                let uv = image.uv_transform() * bt;
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
                        .appearance
                        .map(|appearance| appearance.transformed(outer)),
                    brush.sampler.quality,
                )
            }
            Brush::Gradient(gradient) => {
                let Some(paint) = super::gradient::uniform(
                    gradient,
                    outer * transform * brush_transform.unwrap_or(Affine::IDENTITY),
                    at,
                )?
                else {
                    return Ok(());
                };
                let allocation = self.mesh_allocation();
                let mesh = self.geometry_cache.shapes.lock().unwrap().shape(
                    frame,
                    origin,
                    (shape, fill, stroke),
                    crate::common::recording_cache::ShapeDependency {
                        transform: outer * transform,
                        size: self.size,
                        uv: Affine::IDENTITY,
                        color: [composite.alpha; 4],
                        sampling: Default::default(),
                    },
                    allocation,
                    crate::common::recording_cache::Scope {
                        budgets: self.budgets,
                        limit: self.gpu.limits.transient_bytes,
                        enabled: !self.capture && self.geometry_policy.reuse,
                    },
                )?;
                let white = self.white()?;
                return self.paint_mesh(
                    target,
                    mesh,
                    SampledImage::plain(&white.view),
                    [false; 2],
                    Some(paint),
                );
            }
        };
        let sampling = if let Brush::Image(brush) = brush {
            mesh::ImageSampling::with_layout(brush.image.source_rect, image.layout, repeat)
        } else {
            Default::default()
        };
        let allocation = self.mesh_allocation();
        let mesh = self.geometry_cache.shapes.lock().unwrap().shape(
            frame,
            origin,
            (shape, fill, stroke),
            crate::common::recording_cache::ShapeDependency {
                transform: outer * transform,
                size: self.size,
                uv,
                color,
                sampling,
            },
            allocation,
            crate::common::recording_cache::Scope {
                budgets: self.budgets,
                limit: self.gpu.limits.transient_bytes,
                enabled: !self.capture && self.geometry_policy.reuse,
            },
        )?;
        self.paint_resource(target, mesh, &image, repeat, appearance, quality)
    }
    pub(super) fn image_resource(
        &self,
        handle: reel_ui::resources::ResourceHandle,
        at: RecordLocation,
    ) -> Result<Arc<resources::Image>, GpuError> {
        self.images
            .get(&handle)
            .cloned()
            .ok_or(GpuError::Unsupported {
                at,
                record: "image missing from upload plan",
            })
    }
    fn glyphs(
        &mut self,
        frame: &FrameDrawing,
        target: &Target,
        run: &reel_ui::frame::FrameGlyphRun,
        outer: Affine,
        origin: reel_ui::frame::CommandOrigin,
    ) -> Result<(), GpuError> {
        let at = origin.at;
        if !self.capture && self.geometry_policy.reuse && frame.recording(origin).is_some() {
            self.geometry_policy.optional.set(true);
        }
        self.composite_supported(run.composite, at)?;
        let (color, paint) = match &run.brush {
            Brush::Solid(color) => (
                color
                    .convert::<LinearSrgb>()
                    .premultiply()
                    .components
                    .map(|x| x * run.composite.alpha),
                None,
            ),
            Brush::Gradient(gradient) => {
                let Some(paint) = super::gradient::uniform(
                    gradient,
                    outer * run.transform * run.brush_transform.unwrap_or(Affine::IDENTITY),
                    at,
                )?
                else {
                    return Ok(());
                };
                ([run.composite.alpha; 4], Some(paint))
            }
            Brush::Image(_) => {
                return Err(GpuError::Unsupported {
                    at,
                    record: "image glyph brush",
                });
            }
        };
        let mut page: Option<Arc<super::glyph_cache::Page>> = None;
        let mut batch = mesh::Mesh::new(self.mesh_allocation());
        let density = super::glyph::raster_density(outer * run.transform);
        let run_key = Arc::new(super::glyph_cache::RunKey::new(run, density));
        let dependency = crate::common::recording_cache::GlyphDependency {
            transform: outer * run.transform,
            size: self.size,
            color,
            font: run.font,
        };
        let cached = self
            .geometry_cache
            .glyphs
            .lock()
            .unwrap()
            .get(frame, origin, &dependency);
        if let Some(cached) = cached
            && cached.value().leases.iter().all(|(id, lease)| {
                self.glyphs
                    .entries
                    .get(&super::glyph_cache::Key::new(run_key.clone(), *id))
                    .is_some_and(|current| Arc::ptr_eq(current, lease))
            })
        {
            for (mesh, page) in &cached.value().segments {
                let mesh = mesh.borrow(self.mesh_allocation())?;
                self.paint_mesh(
                    target,
                    mesh,
                    SampledImage::coverage(&page.view),
                    [false; 2],
                    paint,
                )?;
            }
            return Ok(());
        }
        let mut captured = if !self.capture
            && self.geometry_policy.reuse
            && self.geometry_cache.glyphs.lock().unwrap().observe(
                frame,
                origin,
                &dependency,
                self.budgets,
                self.gpu.limits.transient_bytes,
            ) {
            crate::common::recording_cache::Glyphs::new(
                self.budgets,
                self.gpu.limits.transient_bytes,
            )
        } else {
            None
        };

        for index in 0..run.glyphs.len() {
            let key = super::glyph_cache::Key::new(run_key.clone(), run.glyphs[index].id);
            let glyph = self
                .glyphs
                .entries
                .get(&key)
                .cloned()
                .ok_or(GpuError::Unsupported {
                    at,
                    record: "glyph missing from preparation",
                })?;
            crate::common::recording_cache::Glyphs::lease(
                &mut captured,
                run.glyphs[index].id,
                glyph.clone(),
                self.budgets,
                self.gpu.limits.transient_bytes,
            );
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
                crate::common::recording_cache::Glyphs::capture(
                    &mut captured,
                    &mut batch,
                    previous.clone(),
                    self.budgets,
                    self.gpu.limits.transient_bytes,
                );
                self.paint_mesh(
                    target,
                    batch,
                    SampledImage::coverage(&previous.view),
                    [false; 2],
                    paint,
                )?;
                batch = mesh::Mesh::new(self.mesh_allocation());
            }
            page = Some(glyph_page.clone());
            let bounds = crate::common::glyph_plan::bitmap_bounds(
                [run.glyphs[index].x, run.glyphs[index].y],
                *offset,
                *size,
                density,
            );
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
            crate::common::recording_cache::Glyphs::capture(
                &mut captured,
                &mut batch,
                page.clone(),
                self.budgets,
                self.gpu.limits.transient_bytes,
            );
            self.paint_mesh(
                target,
                batch,
                SampledImage::coverage(&page.view),
                [false; 2],
                paint,
            )?;
        }
        if let Some(captured) = captured {
            self.geometry_cache.glyphs.lock().unwrap().insert(
                frame,
                origin,
                dependency,
                captured,
                self.budgets,
                self.gpu.limits.transient_bytes,
            );
        }
        Ok(())
    }
    pub(super) fn paint_resource(
        &mut self,
        target: &Target,
        mesh: impl Into<crate::common::recording_cache::Reference>,
        image: &super::resources::Image,
        repeat: [bool; 2],
        appearance: Option<ImageAppearance>,
        quality: ImageQuality,
    ) -> Result<(), GpuError> {
        if appearance
            .and_then(|appearance| appearance.gradient)
            .is_some_and(|gradient| gradient.transform.determinant() == 0.)
        {
            return Ok(());
        }
        self.paint_mesh(
            target,
            mesh,
            SampledImage {
                view: &image.view,
                coverage: false,
                format: image.texture_format,
                yuv: image.yuv.as_ref(),
                quality,
                source: mesh::PaintSource::Texture,
            },
            repeat,
            appearance.map(|appearance| {
                let mut uniform = appearance_uniform(appearance);
                uniform[8] = [
                    image.layout.content_size[0] as f32 / image.layout.storage_size[0] as f32,
                    image.layout.content_size[1] as f32 / image.layout.storage_size[1] as f32,
                    match image.texture_format {
                        Some(reel_core::texture::TextureFormat::Bc1Rgb) => 1.,
                        Some(reel_core::texture::TextureFormat::Bc3Rgba) => 2.,
                        _ => 0.,
                    },
                    f32::from(quality == ImageQuality::Low),
                ];
                if uniform[4][3] == 3.
                    && image.yuv.is_none()
                    && image.encoding == reel_ui::resources::ColorEncoding::Linear
                {
                    uniform[4][3] = 4.;
                }
                uniform
            }),
        )
    }

    fn paint_mesh(
        &mut self,
        target: &Target,
        mesh: impl Into<crate::common::recording_cache::Reference>,
        image: SampledImage<'_>,
        repeat: [bool; 2],
        appearance: Option<super::gradient::Uniform>,
    ) -> Result<(), GpuError> {
        let mesh = mesh.into();
        let SampledImage {
            view: image,
            coverage,
            format: texture_format,
            yuv,
            quality,
            source,
        } = image;
        if mesh.indices.is_empty() || self.scissor.empty() {
            return Ok(());
        }
        let clipped = self
            .mesh_clip
            .map(|clip| mesh.clipped(clip, self.mesh_allocation()))
            .transpose()?
            .flatten();
        let mesh = clipped
            .map(crate::common::recording_cache::Reference::from)
            .unwrap_or(mesh);
        if mesh.indices.is_empty() {
            return Ok(());
        }
        if self
            .paint_batch
            .as_ref()
            .is_some_and(|batch| batch.geometry.full(&mesh))
        {
            self.flush_draws()?;
        }
        let direct = if coverage {
            mesh.direct_coverage(appearance.as_ref())
        } else {
            yuv.is_none()
                .then(|| mesh.direct_paint(source, repeat, appearance.as_ref(), texture_format))
                .flatten()
        };
        let state = PaintState {
            direct,
            opaque: mesh.opaque_paint(direct),
            image: image.clone(),
            coverage: coverage && direct.is_none(),
            texture_format,
            yuv: yuv.map(|planes| VideoState {
                chroma: planes.chroma.clone(),
                color: planes.color.clone(),
            }),
            repeat,
            quality,
            appearance,
            scissor: self.scissor,
        };
        if let Some(mut batch) = self.paint_batch.take() {
            let result = batch.geometry.push(mesh, state);
            self.paint_batch = Some(batch);
            result
        } else {
            self.paint_batch = Some(PaintBatch {
                target: target.clone(),
                geometry: crate::common::draw_batch::Batch::new(mesh, state)?,
            });
            Ok(())
        }
    }
    /// Consume an ordered segment in one pass; uploads remain immutable in flight.
    pub(super) fn flush_draws(&mut self) -> Result<(), GpuError> {
        let Some(batch) = self.paint_batch.take() else {
            return Ok(());
        };
        if batch.geometry.capacity().indices == 0 {
            return Ok(());
        }
        self.transient(
            (batch.geometry.entries.len() * size_of::<wgpu::BindGroup>()
                + batch
                    .geometry
                    .entries
                    .iter()
                    .filter(|entry| entry.state.appearance.is_some())
                    .count()
                    * 144) as u64,
        )?;
        let device = &self.gpu.device;
        let index_format = match batch.geometry.capacity().index_format {
            crate::common::indices::Format::U16 => wgpu::IndexFormat::Uint16,
            crate::common::indices::Format::U32 => wgpu::IndexFormat::Uint32,
        };
        let (storage, local, metadata) = self.geometry_cache.upload_batch(
            self.gpu,
            self.budgets,
            &self.budget_scope,
            &batch.geometry,
            self.geometry_policy,
            &mut self.encoder,
        )?;
        self.tickets.push(super::renderer::Reservation {
            _shared: None,
            _local: local,
        });
        if let Some(local) = metadata {
            self.tickets.push(super::renderer::Reservation {
                _shared: None,
                _local: local,
            });
        }
        if storage.storage.has_staging() {
            self.geometry_policy.optional.set(true);
        }
        self.geometry.push(storage);
        let storage = &self.geometry.last().expect("retained geometry").storage;
        let vertices = &storage.vertices;
        let indices = &storage.indices;
        let mut groups = Vec::with_capacity(batch.geometry.entries.len());
        for entry in &batch.geometry.entries {
            let PaintState {
                image,
                texture_format,
                yuv,
                repeat,
                quality,
                appearance,
                ..
            } = &entry.state;
            #[cfg(test)]
            {
                self.gpu
                    .upload_creations
                    .draws
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.gpu
                    .upload_creations
                    .brush_groups
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            let appearance_buffer = appearance.as_ref().map(|values| {
                #[cfg(test)]
                self.gpu
                    .upload_creations
                    .appearance_buffers
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("brush appearance"),
                    contents: bytemuck::cast_slice(values),
                    usage: wgpu::BufferUsages::UNIFORM,
                })
            });
            let samplers = if *quality == ImageQuality::Low {
                &self.pipelines.nearest_samplers
            } else {
                &self.pipelines.samplers
            };
            let sampler = &samplers[usize::from(repeat[0]) + 2 * usize::from(repeat[1])];
            let appearance = appearance_buffer
                .as_ref()
                .unwrap_or_else(|| match texture_format {
                    Some(reel_core::texture::TextureFormat::Bc1Rgb) => {
                        &self.pipelines.block_appearance[0]
                    }
                    Some(reel_core::texture::TextureFormat::Bc3Rgba) => {
                        &self.pipelines.block_appearance[1]
                    }
                    _ => &self.pipelines.plain_appearance,
                });
            let base = [
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(image),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: appearance.as_entire_binding(),
                },
            ];
            let group = if let Some(yuv) = yuv {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("video brush"),
                    layout: &self.pipelines.yuv_layout,
                    entries: &[
                        base[0].clone(),
                        base[1].clone(),
                        base[2].clone(),
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::TextureView(&yuv.chroma[0]),
                        },
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: wgpu::BindingResource::TextureView(&yuv.chroma[1]),
                        },
                        wgpu::BindGroupEntry {
                            binding: 5,
                            resource: yuv.color.as_entire_binding(),
                        },
                    ],
                })
            } else {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("reel brush"),
                    layout: &self.pipelines.paint_layout,
                    entries: &base,
                })
            };
            groups.push(group);
        }
        let mut pass = self.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("reel ordered draw segment"),
            color_attachments: &[Some(
                batch
                    .target
                    .attachment(self.target_writes.take(batch.target.slot)),
            )],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            multiview_mask: None,
            occlusion_query_set: None,
        });
        pass.set_viewport(
            0.,
            0.,
            batch.target.size[0] as f32,
            batch.target.size[1] as f32,
            0.,
            1.,
        );
        pass.set_vertex_buffer(0, vertices.slice(..));
        pass.set_index_buffer(indices.slice(..), index_format);
        for (entry, group) in batch.geometry.entries.iter().zip(&groups) {
            let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Draw);
            pass.set_pipeline(if entry.state.coverage {
                #[cfg(test)]
                self.pipelines
                    .coverage_draws
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                &self.pipelines.coverage
            } else if entry.state.yuv.is_some() {
                &self.pipelines.yuv
            } else if let Some(direct) = entry.state.direct {
                let index = direct as usize;
                #[cfg(test)]
                self.pipelines.direct_draws[index]
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if let Some(opaque) = entry.state.opaque {
                    #[cfg(test)]
                    self.pipelines.opaque_draws[opaque as usize]
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    &self.pipelines.opaque[opaque as usize]
                } else {
                    &self.pipelines.direct[index]
                }
            } else {
                &self.pipelines.paint
            });
            entry.state.scissor.apply(&mut pass);
            pass.set_bind_group(0, group, &[]);
            pass.draw_indexed(entry.range.clone(), 0, 0..1);
        }

        Ok(())
    }
}

use crate::common::appearance::appearance_uniform;

#[cfg(test)]
mod tests {
    use super::*;
    use reel_ui::{
        frame::{FrameImage, ImageColorAdjustment, ImageColorGradient},
        resources::ResourceHandle,
    };

    #[test]
    fn image_appearance_uniform_and_frame_wire_preserve_one_value() {
        let adjustment = ImageColorAdjustment {
            saturation: 0.4,
            gain: [1.2, 0.8, 0.5],
            offset: [0.1, -0.2, 0.],
        };
        let gradient = ImageColorGradient {
            colors: [[0.5; 4]; 4],
            transform: Affine::translate((10., 20.)) * Affine::scale_non_uniform(20., 40.),
        };
        for corners in [None, Some(gradient)] {
            let appearance = ImageAppearance::new(corners, adjustment, Default::default()).unwrap();
            let uniform = appearance_uniform(appearance);
            assert_eq!(size_of_val(&uniform), 144);
            assert_eq!(uniform[4][3], 1.);
            assert_eq!(uniform[5][3], f32::from(corners.is_some()));
            assert_eq!(uniform[6], [1.2, 0.8, 0.5, 0.4]);
            assert_eq!(uniform[7], [0.1, -0.2, 0., 1.]);
            if corners.is_some() {
                assert_eq!(uniform[4], [0.05, 0., -0.5, 1.]);
                assert_eq!(uniform[5], [0., 0.025, -0.5, 1.]);
            }
            let image = FrameImage {
                resource: ResourceHandle {
                    registry: 1,
                    slot: 2,
                    generation: 3,
                },
                color: [1.; 4],
                appearance: Some(appearance),
                source_rect: None,
            };
            let encoded = postcard::to_allocvec(&image).unwrap();
            let decoded = postcard::from_bytes::<FrameImage>(&encoded).unwrap();
            assert_eq!(decoded, image);
        }
    }
}
