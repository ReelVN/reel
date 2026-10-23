use super::{GpuError, mesh, renderer::reserve, replay::Context, resources, targets::Target};
use reel_ui::frame::{FrameDraw, FrameDrawing, ImageAppearance, RecordLocation};
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
        frame: &FrameDrawing,
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
        let (image, uv, color, repeat, appearance) = match brush {
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
                let image = self.image_resource(brush.image.resource, at)?;
                let bt = brush_transform.unwrap_or(Affine::IDENTITY);
                let uv = Affine::scale_non_uniform(
                    1. / f64::from(image.size[0]),
                    1. / f64::from(image.size[1]),
                ) * bt;
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
                mesh::transform(
                    &mut mesh,
                    outer * transform,
                    self.size,
                    Affine::IDENTITY,
                    [composite.alpha; 4],
                    at,
                )?;
                let white = self.white()?;
                return self.paint_mesh(target, &mesh, &white.view, [false; 2], Some(paint), None);
            }
        };
        if let Brush::Image(brush) = brush {
            mesh.set_image_sampling(mesh::ImageSampling::new(
                brush.image.source_rect,
                image.size,
                repeat,
            ));
        }
        mesh::transform(&mut mesh, outer * transform, self.size, uv, color, at)?;
        self.paint_resource(target, &mesh, &image, repeat, appearance)
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
        at: RecordLocation,
    ) -> Result<(), GpuError> {
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
                        density,
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
                self.paint_mesh(target, &batch, &previous.view, [false; 2], paint, None)?;
                batch = mesh::Mesh::new(self.mesh_allocation());
            }
            page = Some(glyph_page.clone());
            let x = f64::from(run.glyphs[index].x) + offset[0] / density;
            let y = f64::from(run.glyphs[index].y) + offset[1] / density;
            let bounds = Rect::new(
                x,
                y,
                x + f64::from(size[0]) / density,
                y + f64::from(size[1]) / density,
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
            self.paint_mesh(target, &batch, &page.view, [false; 2], paint, None)?;
        }
        Ok(())
    }
    pub(super) fn paint(
        &mut self,
        target: &Target,
        mesh: &mesh::Mesh,
        image: &wgpu::TextureView,
    ) -> Result<(), GpuError> {
        self.paint_image(target, mesh, image, [false; 2], None, None)
    }

    pub(super) fn paint_resource(
        &mut self,
        target: &Target,
        mesh: &mesh::Mesh,
        image: &super::resources::Image,
        repeat: [bool; 2],
        appearance: Option<ImageAppearance>,
    ) -> Result<(), GpuError> {
        self.paint_image(
            target,
            mesh,
            &image.view,
            repeat,
            appearance,
            image.yuv.as_ref(),
        )
    }

    fn paint_image(
        &mut self,
        target: &Target,
        mesh: &mesh::Mesh,
        image: &wgpu::TextureView,
        repeat: [bool; 2],
        appearance: Option<ImageAppearance>,
        yuv: Option<&super::video::Planes>,
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
            image,
            repeat,
            appearance.map(appearance_uniform),
            yuv,
        )
    }

    fn paint_mesh(
        &mut self,
        target: &Target,
        mesh: &mesh::Mesh,
        image: &wgpu::TextureView,
        repeat: [bool; 2],
        appearance: Option<super::gradient::Uniform>,
        yuv: Option<&super::video::Planes>,
    ) -> Result<(), GpuError> {
        if mesh.indices.is_empty() || self.scissor.empty() {
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
        if appearance.is_some() {
            self.transient(128)?;
        }
        let device = &self.gpu.device;
        let appearance_buffer = appearance.map(|values| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("brush appearance"),
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
        let sampler = &self.pipelines.samplers[usize::from(repeat[0]) + 2 * usize::from(repeat[1])];
        let appearance = appearance_buffer
            .as_ref()
            .unwrap_or(&self.pipelines.plain_appearance);
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
        let mut pass = self.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("reel draw"),
            color_attachments: &[Some(target.attachment(false))],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            multiview_mask: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(if yuv.is_some() {
            &self.pipelines.yuv
        } else {
            &self.pipelines.paint
        });
        self.scissor.apply(&mut pass);
        pass.set_bind_group(0, &group, &[]);
        pass.set_vertex_buffer(0, vertices.slice(..));
        pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..mesh.indices.len() as u32, 0, 0..1);
        Ok(())
    }
}

fn appearance_uniform(appearance: ImageAppearance) -> [[f32; 4]; 8] {
    let mut values = [[0.; 4]; 8];
    // The ordinary shader path keeps its first branch and reads no other fields.
    values[4][3] = 1.;
    if let Some(gradient) = appearance.gradient {
        let inverse = gradient.transform.inverse().as_coeffs();
        values[..4].copy_from_slice(&gradient.colors);
        values[4] = [inverse[0] as f32, inverse[2] as f32, inverse[4] as f32, 1.];
        values[5] = [inverse[1] as f32, inverse[3] as f32, inverse[5] as f32, 1.];
    }
    let color = appearance.adjustment;
    values[6] = [
        color.gain[0],
        color.gain[1],
        color.gain[2],
        color.saturation,
    ];
    values[7] = [
        color.offset[0],
        color.offset[1],
        color.offset[2],
        f32::from(!color.is_identity()),
    ];
    values
}

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
            let appearance = ImageAppearance::new(corners, adjustment).unwrap();
            let uniform = appearance_uniform(appearance);
            assert_eq!(size_of_val(&uniform), 128);
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
