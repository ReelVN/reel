use super::*;
use super::{
    programs::{Target, Values},
    replay::Context,
};
#[cfg(feature = "profiling")]
use crate::common::scissor::Scissor;
use crate::common::{
    appearance::appearance_uniform,
    glyph,
    glyph_key::{Key, RunKey},
    mesh,
};
use guillotiere::{AllocId, AtlasAllocator, size2};
use reel_ui::{
    frame::{
        FrameDraw, FrameDrawing, FrameGlyphRun, FrameOp, ImageAppearance, InkBounds, RecordLocation,
    },
    imaging::{Composite, record::Clip},
    kurbo::{Affine, Rect},
    peniko::{Brush, Extend, Fill, ImageQuality, color::LinearSrgb},
};
use std::{cell::RefCell, rc::Weak, sync::Arc};
#[path = "draw_instances.rs"]
mod instances;
pub(super) use instances::InstanceBatch;

struct Paint<'a> {
    texture: &'a native::Texture,
    planes: &'a [native::Texture],
    kind: f32,
    video: &'a [[f32; 4]; 4],
    dimensions: [u32; 2],
    appearance: [[f32; 4]; 10],
    repeat: [bool; 2],
    coverage: bool,
    format: Option<PixelFormat>,
    quality: ImageQuality,
    source: mesh::PaintSource,
}

pub(super) struct PaintBatch {
    target: Target,
    geometry: crate::common::draw_batch::Batch<PaintState>,
}
#[derive(PartialEq)]
struct PaintState {
    texture: native::Texture,
    planes: smallvec::SmallVec<[native::Texture; 2]>,
    kind: f32,
    video: [[f32; 4]; 4],
    dimensions: [u32; 2],
    appearance: [[f32; 4]; 10],
    repeat: [bool; 2],
    quality: ImageQuality,
    coverage: bool,
    direct: Option<mesh::DirectPaint>,
    sampling: Option<mesh::DirectPaint>,
    bounded: bool,
    alpha_tint: bool,
    monochrome: Option<crate::common::appearance::Monochrome>,
    opaque: Option<mesh::OpaquePaint>,
    replace_transparent: bool,
    scissor: crate::common::scissor::Scissor,
    interior: Option<crate::common::scissor::Scissor>,
}

pub(super) struct Page {
    texture: native::Texture,
    allocator: RefCell<AtlasAllocator>,
    size: [u32; 2],
    _storage: super::ownership::NativeTicket,
}
struct GlyphImage {
    page: Rc<Page>,
    allocation: AllocId,
    origin: [u32; 2],
    size: [u32; 2],
    offset: [f64; 2],
}
pub(super) struct Glyph {
    source: crate::common::resources::ResourceSource,
    image: Option<GlyphImage>,
    ready: std::cell::Cell<bool>,
}
impl Drop for Glyph {
    fn drop(&mut self) {
        if let Some(image) = &self.image {
            image
                .page
                .allocator
                .borrow_mut()
                .deallocate(image.allocation);
        }
    }
}
#[derive(Default)]
pub(super) struct GlyphCache {
    entries: HashMap<Key, Rc<Glyph>>,
    pages: Vec<Weak<Page>>,
    raster: glyph::Rasterizer,
    pending_glyphs: Vec<Rc<Glyph>>,
    failures: Vec<super::ownership::NativeTicket>,
}
impl GlyphCache {
    pub fn begin_preparation(&mut self) {
        self.entries.retain(|_, glyph| glyph.ready.get());
    }
    pub fn flush_uploads(
        &mut self,
        device: DeviceResources<'_>,
        uploads: &mut crate::common::glyph_upload::Batch<Rc<Glyph>>,
    ) -> Result<(), GpuError> {
        uploads.flush(
            |a, b| {
                Rc::ptr_eq(
                    &a.image.as_ref().unwrap().page,
                    &b.image.as_ref().unwrap().page,
                )
            },
            |upload| {
                let stride = upload.size[0] as usize;
                let bytes = stride * upload.size[1] as usize;
                let _staging = resources::reserve(
                    &device.budgets.transient,
                    bytes as u64,
                    device.limits.transient_bytes,
                    "glyph staging",
                )?;
                let mut data = vec![0; bytes];
                upload.write_rows(stride, |start, source| {
                    data[start..start + source.len()].copy_from_slice(source)
                });
                device.gpu.upload(
                    &upload.owner.image.as_ref().unwrap().page.texture,
                    upload.origin,
                    upload.size,
                    &data,
                    stride,
                )?;
                Ok(())
            },
            |glyph| glyph.ready.set(true),
        )
    }
    pub fn prune_idle(&mut self) {
        self.entries.retain(|_, glyph| Rc::strong_count(glyph) > 1);
        self.pages.retain(|page| page.strong_count() != 0);
    }
    pub fn prune(&mut self, versions: &[ResourceVersion]) {
        self.entries
            .retain(|_, glyph| glyph.source.retained(versions));
        self.pages.retain(|page| page.strong_count() != 0);
    }
    pub fn take_uploads(&mut self) -> Vec<Rc<Glyph>> {
        std::mem::take(&mut self.pending_glyphs)
    }
    pub fn after_drain(&mut self) {
        self.failures.clear();
        self.pending_glyphs.clear();
    }
    fn upload(
        &mut self,
        device: DeviceResources<'_>,
        font: &reel_ui::resources::ResourceLease,
        bitmap: Option<glyph::GlyphBitmap>,
        uploads: &mut crate::common::glyph_upload::Batch<Rc<Glyph>>,
    ) -> Result<Rc<Glyph>, GpuError> {
        let DeviceResources {
            gpu,
            budgets,
            limits,
        } = device;
        let source = crate::common::resources::ResourceSource::new(font);
        let max = gpu.capabilities().max_texture_size;
        let Some(bitmap) = bitmap else {
            return Ok(Rc::new(Glyph {
                source,
                image: None,
                ready: std::cell::Cell::new(true),
            }));
        };
        let requested = size2(bitmap.size[0] as i32 + 2, bitmap.size[1] as i32 + 2);
        self.pages.retain(|page| page.strong_count() != 0);
        let found = self
            .pages
            .iter()
            .filter_map(Weak::upgrade)
            .find_map(|page| {
                let allocation = page.allocator.borrow_mut().allocate(requested)?;
                Some((page, allocation))
            });
        let (page, allocation) = if let Some(found) = found {
            found
        } else {
            let size = [
                512.min(max).max(requested.width as u32),
                512.min(max).max(requested.height as u32),
            ];
            let bytes = super::super::texture_bytes(size, native::TextureFormat::Luma8)?;
            if bytes
                > limits
                    .resource_bytes
                    .saturating_sub(budgets.resources.usage().bytes)
            {
                self.entries.retain(|_, glyph| Rc::strong_count(glyph) > 1);
            }
            let storage = resources::reserve(
                &budgets.resources,
                bytes,
                limits.resource_bytes,
                "glyph atlas",
            )?;
            let storage = gpu.ticket(storage);
            let texture = match gpu.create_texture(
                size,
                native::TextureFormat::Luma8,
                native::TextureMemory::Cdram,
            ) {
                Ok(texture) => texture,
                Err(error) => {
                    // Texture construction issues no GPU commands. Its native
                    // partial owners already retire their own allocations.
                    if gpu.refresh() {
                        self.failures.push(storage);
                    }
                    return Err(error.into());
                }
            };
            let mut allocator = AtlasAllocator::new(size2(size[0] as i32, size[1] as i32));
            let allocation = allocator.allocate(requested).ok_or(GpuError::InvalidSize)?;
            let page = Rc::new(Page {
                texture,
                allocator: RefCell::new(allocator),
                size,
                _storage: storage,
            });
            self.pages.push(Rc::downgrade(&page));
            (page, allocation)
        };
        let origin = [
            allocation.rectangle.min.x as u32 + 1,
            allocation.rectangle.min.y as u32 + 1,
        ];
        let glyph = Rc::new(Glyph {
            source,
            ready: std::cell::Cell::new(false),
            image: Some(GlyphImage {
                page: page.clone(),
                allocation: allocation.id,
                origin,
                size: bitmap.size,
                offset: [bitmap.x, bitmap.y],
            }),
        });
        self.pending_glyphs.push(glyph.clone());
        if uploads.full(&bitmap) {
            self.flush_uploads(
                DeviceResources {
                    gpu,
                    budgets,
                    limits,
                },
                uploads,
            )?;
        }
        uploads.push(
            glyph.clone(),
            [origin[0] - 1, origin[1] - 1],
            bitmap,
            &budgets.transient,
            limits.transient_bytes,
        )?;
        Ok(glyph)
    }
}

impl crate::common::glyph_plan::Prepare for Context<'_> {
    fn finish(&mut self) -> Result<(), GpuError> {
        self.glyph_cache.flush_uploads(
            DeviceResources {
                gpu: self.gpu,
                budgets: self.budgets,
                limits: self.limits,
            },
            &mut self.glyph_uploads,
        )
    }
    fn reserve(&mut self, bytes: u64) -> Result<(), GpuError> {
        self.transient(bytes)
    }

    fn run(
        &mut self,
        frame: &FrameDrawing,
        run: &FrameGlyphRun,
        density: f64,
        at: RecordLocation,
        measure_ink: bool,
    ) -> Result<InkBounds, GpuError> {
        let key = Arc::new(RunKey::new(run, density));
        let raster = glyph::RasterRun::new(frame, run, density, at)?;
        let mut ink = InkBounds::Empty;
        for (index, position) in run.glyphs.iter().enumerate() {
            let key = Key::new(key.clone(), position.id);
            let glyph = if let Some(glyph) = self.glyph_cache.entries.get(&key) {
                glyph.clone()
            } else {
                let bitmap = raster.glyph(
                    &mut self.glyph_cache.raster,
                    index,
                    glyph::RasterLimits {
                        available: self
                            .limits
                            .transient_bytes
                            .saturating_sub(self.budgets.transient.usage().bytes),
                        max_dimension: self.gpu.capabilities().max_texture_size.saturating_sub(2),
                        budget: self.budgets.transient.clone(),
                    },
                )?;
                let glyph = self.glyph_cache.upload(
                    DeviceResources {
                        gpu: self.gpu,
                        budgets: self.budgets,
                        limits: self.limits,
                    },
                    raster.source,
                    bitmap,
                    &mut self.glyph_uploads,
                )?;
                self.glyph_cache.entries.insert(key, glyph.clone());
                glyph
            };
            if measure_ink && let Some(image) = &glyph.image {
                ink = crate::common::glyph_plan::union(
                    ink,
                    InkBounds::Rect(crate::common::glyph_plan::bitmap_bounds(
                        [position.x, position.y],
                        image.offset,
                        image.size,
                        density,
                    )),
                );
            }
            // Pin the region during all later uploads, including another run's
            // pressure eviction, and transfer it to the submitted Work object.
            crate::common::scopes::push_stack(&mut self.glyphs, glyph, |bytes| {
                let ticket = crate::common::renderer::reserve(
                    &self.budgets.transient,
                    &self.budget_scope.transient,
                    bytes,
                    self.limits.transient_bytes,
                    "glyph references",
                )?;
                self.tickets.push(ticket);
                Ok(())
            })?;
        }
        Ok(ink)
    }
}

impl Context<'_> {
    /// Project a complete linear offscreen source back into its parent pixels.
    /// Flush both sides of the dependency before its scratch storage is reused.
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
        self.ensure_sampled(&source.view)?;
        let mut mesh = mesh::Mesh::new(self.mesh_allocation());
        crate::common::effect_plan::projected_source(&mut mesh, source.size, source.view.size())?;
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
            Paint {
                texture: &source.view,
                planes: &[],
                kind: 0.,
                video: &[[0.; 4]; 4],
                dimensions: source.view.size(),
                appearance: [[0.; 4]; 10],
                repeat: [false; 2],
                coverage: false,
                format: None,
                quality: ImageQuality::Medium,
                source: mesh::PaintSource::Texture,
            },
        )?;
        self.flush_draws()
    }

    pub fn mesh_allocation(&self) -> mesh::Allocation {
        mesh::Allocation::new(
            self.budgets,
            &self.budget_scope,
            self.limits.transient_bytes,
            u64::from(i32::MAX as u32),
        )
    }
    pub fn image_resource(
        &self,
        handle: ResourceHandle,
        at: RecordLocation,
    ) -> Result<Rc<Image>, GpuError> {
        self.images
            .get(&handle)
            .cloned()
            .ok_or(GpuError::Unsupported {
                at,
                record: "image missing from upload plan",
            })
    }
    pub fn white(&mut self) -> Result<Rc<Image>, GpuError> {
        self.image_cache
            .white(self.gpu, self.budgets, self.limits.resource_bytes)
    }
    pub fn clip(
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
            Paint {
                texture: &white.texture,
                planes: &[],
                kind: 0.,
                video: &[[0.; 4]; 4],
                dimensions: [1, 1],
                appearance: [[0.; 4]; 10],
                repeat: [false; 2],
                coverage: false,
                format: None,
                quality: ImageQuality::Medium,
                source: mesh::PaintSource::Solid,
            },
        )
    }
    fn draw_sprites(
        &mut self,
        target: &Target,
        batch: &reel_ui::frame::FrameSprites,
        outer: Affine,
        at: RecordLocation,
    ) -> Result<(), GpuError> {
        self.composite_supported(batch.composite, at)?;
        let image = self.image_resource(batch.brush.image.resource, at)?;
        let source = image.source.as_ref().and_then(|s| s.lease.upgrade());
        let coverage = source.as_ref().and_then(|s| s.image_coverage());
        if self.sprite_instances(target, batch, outer, &image, coverage, at)? {
            return Ok(());
        }
        self.expand_instances()?;
        let mut mesh = mesh::Mesh::new(self.mesh_allocation());
        mesh.sprites(
            batch,
            outer,
            image.layout,
            image.uv_transform(),
            coverage,
            self.size,
            image._backing.is_some(),
            at,
        )?;
        self.paint_resource(
            target,
            mesh,
            &image,
            [
                batch.brush.sampler.x_extend == Extend::Repeat,
                batch.brush.sampler.y_extend == Extend::Repeat,
            ],
            batch.brush.image.appearance.map(|a| a.transformed(outer)),
            batch.brush.sampler.quality,
        )
    }
    pub fn draw(
        &mut self,
        frame: &FrameDrawing,
        target: &Target,
        draw: &FrameDraw,
        outer: Affine,
        origin: reel_ui::frame::CommandOrigin,
    ) -> Result<(), GpuError> {
        let at = origin.at;
        if !matches!(draw, FrameDraw::Sprites(_)) {
            self.expand_instances()?;
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
            FrameDraw::GlyphRun(run) => return self.draw_glyphs(frame, target, run, outer, origin),
            FrameDraw::Sprites(batch) => return self.draw_sprites(target, batch, outer, at),
            FrameDraw::BlurredRoundedRect(_) => {
                return Err(GpuError::Unsupported {
                    at,
                    record: "blurred rounded rect",
                });
            }
        };
        self.composite_supported(composite, at)?;
        match brush {
            Brush::Solid(color) => {
                let color = color
                    .convert::<LinearSrgb>()
                    .premultiply()
                    .components
                    .map(|v| v * composite.alpha);
                let allocation = self.mesh_allocation();
                let mesh = self.geometry_cache.shapes.shape(
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
                        limit: self.limits.transient_bytes,
                        enabled: !self.capture && !self.geometry_cache.cpu_disabled,
                    },
                )?;
                self.paint_solid(target, mesh)
            }
            Brush::Image(brush) => {
                if brush.sampler.quality == ImageQuality::High
                    || brush.sampler.x_extend == Extend::Reflect
                    || brush.sampler.y_extend == Extend::Reflect
                {
                    return Err(GpuError::Unsupported {
                        at,
                        record: "image sampler",
                    });
                }
                let image = self.image_resource(brush.image.resource, at)?;
                self.image_instances(&image)?;
                let repeat = [
                    brush.sampler.x_extend == Extend::Repeat,
                    brush.sampler.y_extend == Extend::Repeat,
                ];
                let sampling = mesh::ImageSampling::for_image(
                    brush.image.source_rect,
                    image.layout,
                    image.normalized_origin(),
                    repeat,
                    image._backing.is_some(),
                );
                let color = brush
                    .image
                    .color
                    .map(|c| c * composite.alpha * brush.sampler.alpha);
                let allocation = self.mesh_allocation();
                let mesh = self.geometry_cache.shapes.shape(
                    frame,
                    origin,
                    (shape, fill, stroke),
                    crate::common::recording_cache::ShapeDependency {
                        transform: outer * transform,
                        size: self.size,
                        uv: image.uv_transform() * brush_transform.unwrap_or(Affine::IDENTITY),
                        color,
                        sampling,
                    },
                    allocation,
                    crate::common::recording_cache::Scope {
                        budgets: self.budgets,
                        limit: self.limits.transient_bytes,
                        enabled: !self.capture && !self.geometry_cache.cpu_disabled,
                    },
                )?;
                self.paint_resource(
                    target,
                    mesh,
                    &image,
                    repeat,
                    brush
                        .image
                        .appearance
                        .map(|appearance| appearance.transformed(outer)),
                    brush.sampler.quality,
                )
            }
            Brush::Gradient(gradient) => {
                let Some(paint) = crate::common::gradient::uniform(
                    gradient,
                    outer * transform * brush_transform.unwrap_or(Affine::IDENTITY),
                    at,
                )?
                else {
                    return Ok(());
                };
                let allocation = self.mesh_allocation();
                let mesh = self.geometry_cache.shapes.shape(
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
                        limit: self.limits.transient_bytes,
                        enabled: !self.capture && !self.geometry_cache.cpu_disabled,
                    },
                )?;
                let white = self.white()?;
                self.paint_mesh(
                    target,
                    mesh,
                    Paint {
                        texture: &white.texture,
                        planes: &white.planes,
                        kind: white.kind,
                        video: &white.video,
                        dimensions: [1, 1],
                        appearance: paint,
                        repeat: [false; 2],
                        coverage: false,
                        format: white.format,
                        quality: ImageQuality::Medium,
                        source: mesh::PaintSource::Texture,
                    },
                )
            }
        }
    }
    pub fn image_batch(
        &mut self,
        target: &Target,
        commands: &[reel_ui::frame::FrameCommand],
        outer: Affine,
    ) -> Result<usize, GpuError> {
        let Some(first) = commands.first() else {
            return Ok(0);
        };
        let FrameOp::Draw(FrameDraw::Fill {
            shape: reel_ui::imaging::record::Geometry::Rect(_),
            brush: Brush::Image(first_brush),
            composite: first_composite,
            ..
        }) = &first.op
        else {
            return Ok(0);
        };
        if first_composite.blend != Composite::default().blend {
            return Ok(0);
        }
        let image = self.image_resource(first_brush.image.resource, first.origin.at)?;
        self.image_instances(&image)?;
        let source = image
            .source
            .as_ref()
            .and_then(|source| source.lease.upgrade());
        let coverage = source.as_ref().and_then(|source| source.image_coverage());
        let repeat = [
            first_brush.sampler.x_extend == Extend::Repeat,
            first_brush.sampler.y_extend == Extend::Repeat,
        ];
        let mut mesh = mesh::Mesh::new(self.mesh_allocation());
        let mut count = 0;
        for command in commands {
            let FrameOp::Draw(FrameDraw::Fill {
                transform,
                shape: reel_ui::imaging::record::Geometry::Rect(rect),
                brush: Brush::Image(brush),
                brush_transform,
                composite,
                ..
            }) = &command.op
            else {
                break;
            };
            if brush.image.resource != first_brush.image.resource
                || brush.image.appearance != first_brush.image.appearance
                || brush.sampler.quality != first_brush.sampler.quality
                || brush.sampler.x_extend != first_brush.sampler.x_extend
                || brush.sampler.y_extend != first_brush.sampler.y_extend
                || composite.blend != first_composite.blend
            {
                break;
            }
            if brush.sampler.quality == ImageQuality::High
                || brush.sampler.x_extend == Extend::Reflect
                || brush.sampler.y_extend == Extend::Reflect
            {
                break;
            }
            for bounds in crate::common::image_coverage::quads(
                *rect,
                brush,
                brush_transform.unwrap_or(Affine::IDENTITY),
                outer * *transform,
                image.layout,
                coverage,
            )
            .iter()
            {
                mesh.image_quad(
                    bounds,
                    outer * *transform,
                    image.uv_transform() * brush_transform.unwrap_or(Affine::IDENTITY),
                    self.size,
                    mesh::ImagePaint {
                        color: brush
                            .image
                            .color
                            .map(|c| c * composite.alpha * brush.sampler.alpha),
                        sampling: mesh::ImageSampling::for_image(
                            brush.image.source_rect,
                            image.layout,
                            image.normalized_origin(),
                            repeat,
                            image._backing.is_some(),
                        ),
                    },
                    command.origin.at,
                )?;
            }
            count += 1;
        }
        if count != 0 {
            self.paint_resource(
                target,
                mesh,
                &image,
                repeat,
                first_brush.image.appearance.map(|p| p.transformed(outer)),
                first_brush.sampler.quality,
            )?;
        }
        Ok(count)
    }
    fn draw_glyphs(
        &mut self,
        frame: &FrameDrawing,
        target: &Target,
        run: &FrameGlyphRun,
        outer: Affine,
        origin: reel_ui::frame::CommandOrigin,
    ) -> Result<(), GpuError> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::GeometryPrepare);
        let at = origin.at;
        self.composite_supported(run.composite, at)?;
        let (color, paint) = match &run.brush {
            Brush::Solid(color) => (
                color
                    .convert::<LinearSrgb>()
                    .premultiply()
                    .components
                    .map(|v| v * run.composite.alpha),
                None,
            ),
            Brush::Gradient(gradient) => {
                let Some(paint) = crate::common::gradient::uniform(
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
        let density = glyph::raster_density(outer * run.transform);
        let key = Arc::new(RunKey::new(run, density));
        let dependency = crate::common::recording_cache::GlyphDependency {
            transform: outer * run.transform,
            size: self.size,
            color,
            font: run.font,
        };
        let cached = self.geometry_cache.glyphs.get(frame, origin, &dependency);
        if let Some(cached) = cached
            && cached.value().leases.iter().all(|(id, lease)| {
                self.glyph_cache
                    .entries
                    .get(&Key::new(key.clone(), *id))
                    .is_some_and(|current| Rc::ptr_eq(current, lease))
            })
        {
            for (mesh, page) in &cached.value().segments {
                let mesh = mesh.borrow(self.mesh_allocation())?;
                self.paint_glyph_mesh(target, mesh, page, paint)?;
            }
            return Ok(());
        }
        let mut captured = if !self.capture
            && !self.geometry_cache.cpu_disabled
            && self.geometry_cache.glyphs.observe(
                frame,
                origin,
                &dependency,
                self.budgets,
                self.limits.transient_bytes,
            ) {
            crate::common::recording_cache::Glyphs::new(self.budgets, self.limits.transient_bytes)
        } else {
            None
        };

        let mut page: Option<Rc<Page>> = None;
        let mut batch = mesh::Mesh::new(self.mesh_allocation());
        for index in 0..run.glyphs.len() {
            let key = Key::new(key.clone(), run.glyphs[index].id);
            let glyph =
                self.glyph_cache
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
                self.limits.transient_bytes,
            );
            let Some(image) = &glyph.image else {
                continue;
            };
            if let Some(previous) = &page
                && !Rc::ptr_eq(previous, &image.page)
            {
                mesh::transform_glyphs(&mut batch, outer * run.transform, self.size, color, at)?;
                crate::common::recording_cache::Glyphs::capture(
                    &mut captured,
                    &mut batch,
                    previous.clone(),
                    self.budgets,
                    self.limits.transient_bytes,
                );
                self.paint_glyph_mesh(target, batch, previous, paint)?;
                batch = mesh::Mesh::new(self.mesh_allocation());
            }
            page = Some(image.page.clone());
            let bounds = crate::common::glyph_plan::bitmap_bounds(
                [run.glyphs[index].x, run.glyphs[index].y],
                image.offset,
                image.size,
                density,
            );
            let uv = Rect::new(
                f64::from(image.origin[0]) / f64::from(image.page.size[0]),
                f64::from(image.origin[1]) / f64::from(image.page.size[1]),
                f64::from(image.origin[0] + image.size[0]) / f64::from(image.page.size[0]),
                f64::from(image.origin[1] + image.size[1]) / f64::from(image.page.size[1]),
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
                self.limits.transient_bytes,
            );
            self.paint_glyph_mesh(target, batch, &page, paint)?;
        }
        if let Some(captured) = captured {
            self.geometry_cache.glyphs.insert(
                frame,
                origin,
                dependency,
                captured,
                self.budgets,
                self.limits.transient_bytes,
            );
        }
        Ok(())
    }
    fn paint_glyph_mesh(
        &mut self,
        target: &Target,
        mesh: impl Into<crate::common::recording_cache::Reference>,
        page: &Page,
        paint: Option<crate::common::gradient::Uniform>,
    ) -> Result<(), GpuError> {
        self.paint_mesh(
            target,
            mesh,
            Paint {
                texture: &page.texture,
                planes: &[],
                kind: 0.,
                video: &[[0.; 4]; 4],
                dimensions: page.size,
                appearance: paint.unwrap_or([[0.; 4]; 10]),
                repeat: [false; 2],
                coverage: true,
                format: None,
                quality: ImageQuality::Medium,
                source: mesh::PaintSource::Texture,
            },
        )
    }
    fn paint_resource(
        &mut self,
        target: &Target,
        mesh: impl Into<crate::common::recording_cache::Reference>,
        image: &Image,
        repeat: [bool; 2],
        appearance: Option<ImageAppearance>,
        quality: ImageQuality,
    ) -> Result<(), GpuError> {
        let Some(paint) = Paint::image(image, repeat, appearance, quality) else {
            return Ok(());
        };
        self.paint_mesh(target, mesh, paint)
    }
    fn paint_mesh(
        &mut self,
        target: &Target,
        mesh: impl Into<crate::common::recording_cache::Reference>,
        paint: Paint<'_>,
    ) -> Result<(), GpuError> {
        let mesh = mesh.into();
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
        let direct = if paint.coverage {
            mesh.direct_coverage(Some(&paint.appearance))
        } else {
            (paint.kind == 0. || paint.kind > 1.5)
                .then(|| {
                    mesh.direct_paint(
                        paint.source,
                        paint.repeat,
                        Some(&paint.appearance),
                        paint.format,
                    )
                })
                .flatten()
                .filter(|direct| paint.kind == 0. || *direct == mesh::DirectPaint::Rgba)
        };
        let direct = crate::common::sampling_domain::direct_paint(
            &mesh,
            direct,
            paint.dimensions,
            self.size,
            self.scissor,
            paint.texture.mip_count(),
        );
        let bounded = mesh.bounded_color.paint(direct, paint.format);
        let sampling = if direct.is_none()
            && paint.kind == 0.
            && !paint.coverage
            && paint.source == mesh::PaintSource::Texture
        {
            crate::common::sampling_domain::direct_paint(
                &mesh,
                mesh.direct_paint(
                    paint.source,
                    paint.repeat,
                    Some(&paint.sampling_appearance()),
                    paint.format,
                ),
                paint.dimensions,
                self.size,
                self.scissor,
                paint.texture.mip_count(),
            )
        } else {
            None
        };
        let opaque = mesh.opaque_paint(paint.opaque_source(direct, sampling));
        let mut state = paint.state(
            direct,
            sampling,
            bounded,
            opaque,
            self.scissor.for_vertices(&mesh.vertices, self.size),
        );
        state.alpha_tint = crate::common::appearance::constant_alpha_tint(
            state.sampling,
            &state.appearance,
            mesh.vertices.iter().map(|vertex| vertex.color),
        );
        state.monochrome = crate::common::appearance::monochrome_adjustment(
            state.sampling,
            &state.appearance,
            mesh.vertices.iter().map(|vertex| vertex.color),
        );
        if state.shader_kind().unclamped().is_some() {
            state.interior = crate::common::sampling_domain::regional_interior(
                &mesh,
                state.dimensions,
                self.size,
                state.scissor,
                state.texture.mip_count(),
            );
        }
        self.expand_instances()?;
        self.queue_mesh(target, mesh, state)
    }
    fn queue_mesh(
        &mut self,
        target: &Target,
        mesh: crate::common::recording_cache::Reference,
        mut state: PaintState,
    ) -> Result<(), GpuError> {
        if self
            .paint_batch
            .as_ref()
            .is_some_and(|batch| batch.geometry.full(&mesh))
        {
            self.flush_draws()?;
        }
        // Keep this first draw in its own entry: merging a later overlapping
        // image would no longer be source-over on a transparent destination.
        state.replace_transparent = self.paint_batch.is_none()
            && self.target_writes.pending(target.slot)
            && state.opaque.is_none()
            && crate::common::target_writes::single_quad(&mesh, self.size);
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
    /// Flush before target clearing, composition, or leaving the replay stream.
    pub(super) fn flush_draws(&mut self) -> Result<(), GpuError> {
        self.finish_instances()?;
        let Some(batch) = self.paint_batch.take() else {
            return Ok(());
        };
        if batch.geometry.capacity().indices == 0 {
            return Ok(());
        }
        self.ensure_target(&batch.target)?;
        let (storage, local, metadata) = super::geometry::upload_batch(
            self.geometry_cache,
            self.gpu,
            self.budgets,
            &self.budget_scope,
            self.limits,
            &batch.geometry,
        )?;
        self.tickets.push(crate::common::renderer::Reservation {
            _shared: None,
            _local: local,
        });
        if let Some(local) = metadata {
            self.tickets.push(crate::common::renderer::Reservation {
                _shared: None,
                _local: local,
            });
        }
        self.buffers.push(storage);
        let storage = &self.buffers.last().expect("retained draw buffers").storage;
        let vertices = &storage.vertices;
        let indices = &storage.indices;
        for entry in batch.geometry.entries {
            let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::DrawPrepare);
            let program = entry.state.shader_kind();
            let PaintState {
                texture,
                planes,
                kind,
                video,
                dimensions,
                appearance,
                repeat,
                quality,
                coverage,
                direct: _,
                sampling: _,
                bounded: _,
                alpha_tint: _,
                monochrome: _,
                opaque,
                replace_transparent,
                scissor,
                interior,
            } = entry.state;
            let regions = interior.map_or([scissor; 5], |inner| {
                crate::common::sampling_domain::partition(scissor, inner)
            });
            {
                let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Sampler);
                self.gpu.set_repeat(&texture, repeat)?;
                for plane in &planes {
                    self.gpu.set_repeat(plane, repeat)?;
                }
            }
            for (index, scissor) in regions
                .into_iter()
                .take(if interior.is_some() { 5 } else { 1 })
                .enumerate()
            {
                if scissor.empty() {
                    continue;
                }
                let program = if index == 0 && interior.is_some() {
                    program.unclamped().expect("proved regional interior")
                } else {
                    program
                };
                let shader = self.programs.paint.get(
                    self.gpu,
                    program,
                    !scissor.tile_exact(32, batch.target.view.size()),
                    false,
                )?;
                let mut values = Values::new();
                for (name, unit) in [("u_image", 0), ("u_chroma_u", 1), ("u_chroma_v", 2)] {
                    values.extend(shader.int(name, unit));
                }
                values.extend(shader.vec2("u_dimensions", dimensions.map(|n| n as f32)));
                values.extend(shader.vec2("u_output_size", self.size.map(|n| n as f32)));
                values.extend(shader.vec2("u_repeat", repeat.map(f32::from)));
                values.extend(shader.array("u_appearance[0]", &appearance));
                values.extend(shader.array("u_video[0]", &video));
                values.extend(shader.float("u_video_kind", kind));
                values.extend(shader.float("u_image_flip", 0.));
                values.extend(shader.float("u_coverage", f32::from(coverage)));
                let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Draw);
                #[cfg(feature = "profiling")]
                let trace_data = {
                    let mut data = batch.target.trace_data();
                    data.logical = self.size;
                    data.source_extent = texture.size();
                    let mut region = scissor.intersect(Scissor::full(batch.target.view.size()));
                    if batch.target.macrotile_extent().is_none()
                        && data.known & crate::profiling::KNOWN_WRITE_REGION != 0
                    {
                        let [x, y, width, height] = data.write_region;
                        region = region.intersect(Scissor {
                            x,
                            y,
                            width,
                            height,
                        });
                    }
                    data.scissor = [region.x, region.y, region.width, region.height];
                    data.known |= crate::profiling::KNOWN_LOGICAL
                        | crate::profiling::KNOWN_SOURCE_EXTENT
                        | crate::profiling::KNOWN_SCISSOR;
                    data.count = 1;
                    data.texture_bindings = 3;
                    data.known |= crate::profiling::KNOWN_TEXTURE_BINDINGS;
                    data
                };
                #[cfg(feature = "profiling")]
                let trace =
                    crate::profiling::TracePass::begin(program.diagnostic_label(), trace_data);
                let result = self.gpu.draw_filtered(
                    native::Draw {
                        target: native::Target::Framebuffer(&batch.target.framebuffer),
                        size: self.size,
                        program: &shader.program,
                        vertices,
                        instances: None,
                        indices: Some(indices),
                        range: entry.range.clone(),
                        textures: &[
                            &texture,
                            planes.first().unwrap_or(&texture),
                            planes.get(1).or(planes.first()).unwrap_or(&texture),
                        ],
                        uniforms: &values,
                        blend: if opaque.is_some() || replace_transparent {
                            native::Blend::Replace
                        } else {
                            native::Blend::Premultiplied
                        },
                        scissor: Some([scissor.x, scissor.y, scissor.width, scissor.height]),
                    },
                    if quality == ImageQuality::Low {
                        native::TextureFilter::Nearest
                    } else {
                        native::TextureFilter::Linear
                    },
                    #[cfg(feature = "profiling")]
                    program.diagnostic_label(),
                );
                #[cfg(feature = "profiling")]
                if let Some(trace) = trace {
                    trace.finish(
                        if result.is_ok() {
                            crate::profiling::TraceStatus::Ok
                        } else {
                            crate::profiling::TraceStatus::Error
                        },
                        trace_data,
                    );
                }
                result?;
            }
            #[cfg(feature = "profiling")]
            if interior.is_some() || replace_transparent {
                crate::profiling::record_paint_regions(crate::profiling::PaintRegions {
                    batches: u64::from(interior.is_some()),
                    extra_draws: interior.map_or(0, |_| {
                        regions.iter().filter(|r| !r.empty()).count() as u64 - 1
                    }),
                    original_pixels: interior
                        .map_or(0, |_| u64::from(scissor.width) * u64::from(scissor.height)),
                    direct_pixels: interior
                        .map_or(0, |inner| u64::from(inner.width) * u64::from(inner.height)),
                    transparent_replacements: u64::from(replace_transparent),
                    transparent_pixels: u64::from(replace_transparent)
                        * u64::from(scissor.width)
                        * u64::from(scissor.height),
                });
            }
        }
        Ok(())
    }
}
