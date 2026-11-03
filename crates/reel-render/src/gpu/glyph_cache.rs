//! Submitted glyphs share atlas pages. An allocation remains occupied until its
//! last prepared, in-flight or cache reference goes away, including aborted frames.
use super::{
    Gpu, GpuError, glyph,
    renderer::{Budgets, Reservation, reserve},
};
use crate::common::resources::ResourceSource;
use guillotiere::{AllocId, AtlasAllocator, size2};
use reel_core::budget::{BudgetPool, BudgetTicket};
use reel_ui::{
    frame::{FrameDrawing, FrameGlyphRun, InkBounds, RecordLocation},
    resources::ResourceVersion,
};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, Weak},
};
mod upload;

pub(super) use crate::common::glyph_key::{Key, RunKey};
pub(super) struct Page {
    pub view: wgpu::TextureView,
    texture: wgpu::Texture,
    allocator: Mutex<AtlasAllocator>,
    pub size: [u32; 2],
    residency: BudgetTicket,
}
pub(super) enum Glyph {
    Empty(ResourceSource),
    Image {
        page: Arc<Page>,
        allocation: AllocId,
        origin: [u32; 2],
        size: [u32; 2],
        offset: [f64; 2],
        source: ResourceSource,
    },
}
impl Glyph {
    fn source(&self) -> &ResourceSource {
        match self {
            Self::Empty(source) | Self::Image { source, .. } => source,
        }
    }
}
impl Drop for Glyph {
    fn drop(&mut self) {
        if let Self::Image {
            page, allocation, ..
        } = self
        {
            page.allocator.lock().unwrap().deallocate(*allocation);
        }
    }
}
#[derive(Default)]
pub(super) struct GlyphCache {
    entries: Mutex<HashMap<Key, Arc<Glyph>>>,
    pages: Mutex<Vec<Weak<Page>>>,
    raster: Mutex<glyph::Rasterizer>,
}
#[derive(Default)]
pub(super) struct FrameGlyphs {
    pub entries: HashMap<Key, Arc<Glyph>>,
    pages: HashSet<usize>,
}

/// Glyph resource preparation needs no quality tier, pipelines or render target.
/// Its encoder, leases and charges move into the replay context after admission.
pub(super) struct Preparation<'a> {
    pub gpu: &'a Gpu,
    pub encoder: &'a mut wgpu::CommandEncoder,
    pub cache: &'a GlyphCache,
    pub frame: &'a mut FrameGlyphs,
    pub budgets: &'a Budgets,
    pub local: &'a Budgets,
    pub tickets: &'a mut Vec<Reservation>,
    pub images: &'a super::image_cache::ImageCache,
    pub uploads: crate::common::glyph_upload::Batch<Arc<Glyph>>,
}
impl crate::common::glyph_plan::Prepare for Preparation<'_> {
    fn finish(&mut self) -> Result<(), GpuError> {
        upload::flush(
            self.gpu,
            self.encoder,
            &mut self.uploads,
            self.budgets,
            self.local,
            self.tickets,
            self.images,
            self.cache,
        )
    }
    fn reserve(&mut self, bytes: u64) -> Result<(), GpuError> {
        self.tickets.push(reserve(
            &self.budgets.transient,
            &self.local.transient,
            bytes,
            self.gpu.limits.transient_bytes,
            "glyph preparation",
        )?);
        Ok(())
    }

    fn run(
        &mut self,
        drawing: &FrameDrawing,
        run: &FrameGlyphRun,
        density: f64,
        at: RecordLocation,
    ) -> Result<InkBounds, GpuError> {
        let key = Arc::new(RunKey::new(run, density));
        let mut ink = InkBounds::Empty;
        for (index, position) in run.glyphs.iter().enumerate() {
            let key = Key::new(key.clone(), position.id);
            let glyph = if let Some(glyph) = self.frame.entries.get(&key) {
                glyph.clone()
            } else {
                let glyph = match self.cache.get(&key) {
                    Some(glyph) => glyph,
                    None => self.cache.rasterize(
                        self.gpu,
                        self.encoder,
                        drawing,
                        run,
                        density,
                        index,
                        at,
                        self.budgets,
                        self.local,
                        self.tickets,
                        self.images,
                        &mut self.uploads,
                    )?,
                };
                GlyphCache::charge(
                    self.frame,
                    &glyph,
                    &self.local.resources,
                    self.gpu.limits.resource_bytes,
                    self.tickets,
                )?;
                self.frame.entries.insert(key, glyph.clone());
                glyph
            };
            if let Glyph::Image { offset, size, .. } = glyph.as_ref() {
                ink = crate::common::glyph_plan::union(
                    ink,
                    InkBounds::Rect(crate::common::glyph_plan::bitmap_bounds(
                        [position.x, position.y],
                        *offset,
                        *size,
                        density,
                    )),
                );
            }
        }
        Ok(ink)
    }
}

impl GlyphCache {
    pub(super) fn prune_idle(&self) {
        self.entries
            .lock()
            .unwrap()
            .retain(|_, glyph| Arc::strong_count(glyph) > 1);
    }
    pub fn get(&self, key: &Key) -> Option<Arc<Glyph>> {
        self.entries.lock().unwrap().get(key).cloned()
    }
    pub fn publish(&self, frame: &FrameGlyphs) {
        let mut entries = self.entries.lock().unwrap();
        for (key, glyph) in &frame.entries {
            if glyph.source().retained(&[]) {
                entries.insert(key.clone(), glyph.clone());
            }
        }
    }
    pub fn retire(&self, versions: &[ResourceVersion]) {
        self.entries
            .lock()
            .unwrap()
            .retain(|_, glyph| glyph.source().retained(versions));
    }
    pub fn make_room(&self, pool: &BudgetPool, cost: u64, limit: u64) {
        if cost > limit.saturating_sub(pool.usage().bytes) {
            self.entries.lock().unwrap().retain(|_, glyph| {
                cost <= limit.saturating_sub(pool.usage().bytes) || Arc::strong_count(glyph) > 1
            });
        }
    }
    pub fn charge(
        frame: &mut FrameGlyphs,
        glyph: &Arc<Glyph>,
        local: &BudgetPool,
        limit: u64,
        tickets: &mut Vec<Reservation>,
    ) -> Result<(), GpuError> {
        let Glyph::Image { page, .. } = glyph.as_ref() else {
            return Ok(());
        };
        let id = Arc::as_ptr(page) as usize;
        if !frame.pages.contains(&id) {
            let ticket = super::image_cache::reserve_local(local, page.residency.bytes(), limit)?;
            tickets.push(Reservation {
                _shared: None,
                _local: ticket,
            });
            frame.pages.insert(id);
        }
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    pub fn rasterize(
        &self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        frame: &FrameDrawing,
        run: &FrameGlyphRun,
        density: f64,
        index: usize,
        at: RecordLocation,
        budgets: &Budgets,
        local: &Budgets,
        tickets: &mut Vec<Reservation>,
        images: &super::image_cache::ImageCache,
        uploads: &mut crate::common::glyph_upload::Batch<Arc<Glyph>>,
    ) -> Result<Arc<Glyph>, GpuError> {
        let source = ResourceSource::new(
            frame
                .resources()
                .leases()
                .iter()
                .find(|resource| resource.handle() == run.font)
                .ok_or(GpuError::Unsupported {
                    at,
                    record: "missing font lease",
                })?,
        );
        let max = gpu.device.limits().max_texture_dimension_2d;
        let available = gpu
            .limits
            .resource_bytes
            .saturating_sub(local.resources.usage().bytes);
        let Some(bitmap) = glyph::rasterize(
            &mut self.raster.lock().unwrap(),
            frame,
            run,
            density,
            index,
            at,
            glyph::RasterLimits {
                available: available.min(
                    gpu.limits
                        .transient_bytes
                        .saturating_sub(budgets.transient.usage().bytes),
                ),
                max_dimension: max.saturating_sub(2),
                budget: budgets.transient.clone(),
            },
        )?
        else {
            return Ok(Arc::new(Glyph::Empty(source)));
        };
        let requested = size2(bitmap.size[0] as i32 + 2, bitmap.size[1] as i32 + 2);
        let mut pages = self.pages.lock().unwrap();
        pages.retain(|page| page.strong_count() != 0);
        let found = pages.iter().filter_map(Weak::upgrade).find_map(|page| {
            let allocation = page.allocator.lock().unwrap().allocate(requested)?;
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
            images.make_room(&budgets.resources, bytes, gpu.limits.resource_bytes);
            self.make_room(&budgets.resources, bytes, gpu.limits.resource_bytes);
            // Check this preparation's demand before sharing the page globally.
            let check = super::image_cache::reserve_local(
                &local.resources,
                bytes,
                gpu.limits.resource_bytes,
            )?;
            let residency = budgets
                .resources
                .reserve(bytes)
                .map_err(|_| GpuError::Pressure {
                    domain: "glyph atlas",
                    requested: bytes,
                    available: gpu
                        .limits
                        .resource_bytes
                        .saturating_sub(budgets.resources.usage().bytes),
                })?;
            drop(check);
            let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("shared glyph atlas"),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            #[cfg(test)]
            gpu.upload_creations
                .textures
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let mut allocator = AtlasAllocator::new(size2(size[0] as i32, size[1] as i32));
            let allocation = allocator.allocate(requested).unwrap();
            let page = Arc::new(Page {
                view: texture.create_view(&Default::default()),
                texture,
                allocator: Mutex::new(allocator),
                size,
                residency,
            });
            pages.push(Arc::downgrade(&page));
            (page, allocation)
        };
        drop(pages);
        let glyph = Arc::new(Glyph::Image {
            page,
            allocation: allocation.id,
            origin: [
                allocation.rectangle.min.x as u32 + 1,
                allocation.rectangle.min.y as u32 + 1,
            ],
            size: bitmap.size,
            offset: [bitmap.x, bitmap.y],
            source,
        });
        let Glyph::Image { origin, .. } = glyph.as_ref() else {
            unreachable!()
        };
        let origin = [origin[0] - 1, origin[1] - 1];
        if uploads.full(&bitmap) {
            upload::flush(gpu, encoder, uploads, budgets, local, tickets, images, self)?;
        }
        uploads.push(
            glyph.clone(),
            origin,
            bitmap,
            &budgets.transient,
            gpu.limits.transient_bytes,
        )?;
        Ok(glyph)
    }
}
