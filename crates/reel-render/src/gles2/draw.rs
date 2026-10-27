use super::*;
use super::{
    programs::{Target, Values},
    replay::Context,
};
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

struct Paint<'a> {
    texture: &'a native::Texture,
    planes: &'a [native::Texture],
    kind: f32,
    video: &'a [[f32; 4]; 4],
    dimensions: [u32; 2],
    appearance: [[f32; 4]; 9],
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
    appearance: [[f32; 4]; 9],
    repeat: [bool; 2],
    quality: ImageQuality,
    coverage: bool,
    direct: Option<mesh::DirectPaint>,
    opaque: Option<mesh::OpaquePaint>,
    scissor: Option<[u32; 4]>,
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
    fn rasterize(
        &mut self,
        device: DeviceResources<'_>,
        frame: &FrameDrawing,
        run: &FrameGlyphRun,
        density: f64,
        index: usize,
        at: RecordLocation,
    ) -> Result<Rc<Glyph>, GlesError> {
        let DeviceResources {
            gpu,
            budgets,
            limits,
        } = device;
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::GlyphRaster);
        let source = crate::common::resources::ResourceSource::new(
            frame
                .resources()
                .leases()
                .iter()
                .find(|r| r.handle() == run.font)
                .ok_or(GlesError::Unsupported {
                    at,
                    record: "missing font",
                })?,
        );
        let max = gpu.capabilities().max_texture_size;
        let Some(bitmap) = glyph::rasterize(
            &mut self.raster,
            frame,
            run,
            density,
            index,
            at,
            glyph::RasterLimits {
                available: limits
                    .transient_bytes
                    .saturating_sub(budgets.transient.usage().bytes),
                max_dimension: max.saturating_sub(2),
                budget: budgets.transient.clone(),
            },
        )?
        else {
            return Ok(Rc::new(Glyph {
                source,
                image: None,
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
            let bytes = u64::from(size[0]) * u64::from(size[1]);
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
            let texture = match gpu.create_texture(size, native::TextureFormat::Luma8) {
                Ok(texture) => texture,
                Err(error) => {
                    if gpu.finish().is_err() {
                        self.failures.push(storage);
                    }
                    return Err(error.into());
                }
            };
            let mut allocator = AtlasAllocator::new(size2(size[0] as i32, size[1] as i32));
            let allocation = allocator
                .allocate(requested)
                .ok_or(GlesError::InvalidSize)?;
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
            image: Some(GlyphImage {
                page: page.clone(),
                allocation: allocation.id,
                origin,
                size: bitmap.size,
                offset: [bitmap.x, bitmap.y],
            }),
        });
        self.pending_glyphs.push(glyph.clone());
        let stride = requested.width as usize;
        let bytes = stride * requested.height as usize;
        let _staging = resources::reserve(
            &budgets.transient,
            bytes as u64,
            limits.transient_bytes,
            "glyph staging",
        )?;
        let mut data = vec![0; bytes];
        bitmap.padded_rows(stride, |start, source| {
            data[start..start + source.len()].copy_from_slice(source)
        });
        gpu.upload(
            &page.texture,
            [origin[0] - 1, origin[1] - 1],
            [requested.width as u32, requested.height as u32],
            &data,
            stride,
        )?;
        Ok(glyph)
    }
}

impl crate::common::glyph_plan::Prepare for Context<'_> {
    fn reserve(&mut self, bytes: u64) -> Result<(), GlesError> {
        self.transient(bytes)
    }

    fn run(
        &mut self,
        frame: &FrameDrawing,
        run: &FrameGlyphRun,
        density: f64,
        at: RecordLocation,
    ) -> Result<InkBounds, GlesError> {
        let key = Arc::new(RunKey::new(run, density));
        let mut ink = InkBounds::Empty;
        for (index, position) in run.glyphs.iter().enumerate() {
            let key = Key::new(key.clone(), position.id);
            let glyph = if let Some(glyph) = self.glyph_cache.entries.get(&key) {
                glyph.clone()
            } else {
                let glyph = self.glyph_cache.rasterize(
                    DeviceResources {
                        gpu: self.gpu,
                        budgets: self.budgets,
                        limits: self.limits,
                    },
                    frame,
                    run,
                    density,
                    index,
                    at,
                )?;
                self.glyph_cache.entries.insert(key, glyph.clone());
                glyph
            };
            if let Some(image) = &glyph.image {
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
    ) -> Result<(), GlesError> {
        if parent_to_source.determinant() == 0. {
            return Ok(());
        }
        self.flush_draws()?;
        self.ensure_sampled(&source.view)?;
        let mut mesh = mesh::Mesh::new(self.mesh_allocation());
        crate::common::effect_plan::projected_source(
            &mut mesh,
            source.size,
            source.view.size(),
            true,
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
            Paint {
                texture: &source.view,
                planes: &[],
                kind: 0.,
                video: &[[0.; 4]; 4],
                dimensions: source.view.size(),
                appearance: [[0.; 4]; 9],
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
    ) -> Result<Rc<Image>, GlesError> {
        self.images
            .get(&handle)
            .cloned()
            .ok_or(GlesError::Unsupported {
                at,
                record: "image missing from upload plan",
            })
    }
    pub fn white(&mut self) -> Result<Rc<Image>, GlesError> {
        self.image_cache
            .white(self.gpu, self.budgets, self.limits.resource_bytes)
    }
    pub fn clip(
        &mut self,
        target: &Target,
        clip: &Clip,
        outer: Affine,
        at: RecordLocation,
    ) -> Result<(), GlesError> {
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
    ) -> Result<(), GlesError> {
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
                appearance: [[0.; 4]; 9],
                repeat: [false; 2],
                coverage: false,
                format: None,
                quality: ImageQuality::Medium,
                source: mesh::PaintSource::Solid,
            },
        )
    }
    pub fn draw(
        &mut self,
        frame: &FrameDrawing,
        target: &Target,
        draw: &FrameDraw,
        outer: Affine,
        origin: reel_ui::frame::CommandOrigin,
    ) -> Result<(), GlesError> {
        let at = origin.at;
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
            FrameDraw::BlurredRoundedRect(_) => {
                return Err(GlesError::Unsupported {
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
                    return Err(GlesError::Unsupported {
                        at,
                        record: "image sampler",
                    });
                }
                let image = self.image_resource(brush.image.resource, at)?;
                let repeat = [
                    brush.sampler.x_extend == Extend::Repeat,
                    brush.sampler.y_extend == Extend::Repeat,
                ];
                let sampling =
                    mesh::ImageSampling::with_layout(brush.image.source_rect, image.layout, repeat);
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
    ) -> Result<usize, GlesError> {
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
                || brush.sampler != first_brush.sampler
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
                        sampling: mesh::ImageSampling::with_layout(
                            brush.image.source_rect,
                            image.layout,
                            repeat,
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
    ) -> Result<(), GlesError> {
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
                return Err(GlesError::Unsupported {
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
                    .ok_or(GlesError::Unsupported {
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
    ) -> Result<(), GlesError> {
        self.paint_mesh(
            target,
            mesh,
            Paint {
                texture: &page.texture,
                planes: &[],
                kind: 0.,
                video: &[[0.; 4]; 4],
                dimensions: page.size,
                appearance: paint.unwrap_or([[0.; 4]; 9]),
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
    ) -> Result<(), GlesError> {
        if appearance
            .and_then(|a| a.gradient)
            .is_some_and(|g| g.transform.determinant() == 0.)
        {
            return Ok(());
        }
        let mut values = appearance.map(appearance_uniform).unwrap_or([[0.; 4]; 9]);
        values[8] = [
            image.layout.content_size[0] as f32 / image.layout.storage_size[0] as f32,
            image.layout.content_size[1] as f32 / image.layout.storage_size[1] as f32,
            match image.format {
                Some(PixelFormat::Bc1Rgb) => 1.,
                Some(PixelFormat::Bc3Rgba) => 2.,
                _ => 0.,
            },
            f32::from(quality == ImageQuality::Low),
        ];
        if values[4][3] == 3. && image.kind == 0. && image.encoding == ColorEncoding::Linear {
            values[4][3] = 4.;
        }
        self.paint_mesh(
            target,
            mesh,
            Paint {
                texture: &image.texture,
                planes: &image.planes,
                kind: image.kind,
                video: &image.video,
                dimensions: image.layout.storage_size,
                appearance: values,
                repeat,
                coverage: image.coverage,
                format: image.format,
                quality,
                source: mesh::PaintSource::Texture,
            },
        )
    }
    fn paint_mesh(
        &mut self,
        target: &Target,
        mesh: impl Into<crate::common::recording_cache::Reference>,
        paint: Paint<'_>,
    ) -> Result<(), GlesError> {
        let Paint {
            texture,
            planes,
            kind,
            video,
            dimensions,
            appearance,
            repeat,
            coverage,
            format,
            quality,
            source,
        } = paint;
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
        if self
            .paint_batch
            .as_ref()
            .is_some_and(|batch| batch.geometry.full(&mesh))
        {
            self.flush_draws()?;
        }
        let direct = if coverage {
            mesh.direct_coverage(Some(&appearance))
        } else {
            (kind == 0.)
                .then(|| mesh.direct_paint(source, repeat, Some(&appearance), format))
                .flatten()
        };
        let state = PaintState {
            direct,
            opaque: mesh.opaque_paint(direct),
            texture: texture.clone(),
            planes: planes.iter().cloned().collect(),
            kind,
            video: *video,
            dimensions,
            appearance,
            repeat,
            quality,
            coverage,
            scissor: self.gl_scissor(),
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
    /// Flush before target clearing, composition, or leaving the replay stream.
    pub(super) fn flush_draws(&mut self) -> Result<(), GlesError> {
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
                direct,
                opaque,
                scissor,
            } = entry.state;
            let styled = appearance[5][3] != 0. || appearance[7][3] != 0.;
            let shader = if let Some(direct) = direct {
                &self.programs.direct[direct as usize]
            } else if kind > 0. {
                if styled {
                    &self.programs.video_appearance
                } else {
                    &self.programs.video
                }
            } else if coverage {
                if appearance[4][3] == 2. {
                    &self.programs.glyph_gradient
                } else {
                    &self.programs.coverage
                }
            } else if appearance[4][3] >= 3. {
                &self.programs.packed
            } else if appearance[4][3] == 2. {
                &self.programs.gradient
            } else if styled {
                &self.programs.appearance
            } else {
                &self.programs.rgba
            };
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
            {
                let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Sampler);
                self.gpu.set_repeat(&texture, repeat)?;
                for plane in &planes {
                    self.gpu.set_repeat(plane, repeat)?;
                }
            }
            let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Draw);
            self.gpu.draw_filtered(
                native::Draw {
                    target: native::Target::Framebuffer(&batch.target.framebuffer),
                    size: self.size,
                    program: &shader.program,
                    vertices,
                    indices: Some(indices),
                    range: entry.range,
                    textures: &[
                        &texture,
                        planes.first().unwrap_or(&texture),
                        planes.get(1).or(planes.first()).unwrap_or(&texture),
                    ],
                    uniforms: &values,
                    blend: if opaque.is_some() {
                        native::Blend::Replace
                    } else {
                        native::Blend::Premultiplied
                    },
                    scissor,
                },
                if quality == ImageQuality::Low {
                    native::TextureFilter::Nearest
                } else {
                    native::TextureFilter::Linear
                },
            )?;
        }
        Ok(())
    }
}
