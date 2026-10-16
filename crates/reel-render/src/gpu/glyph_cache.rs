//! Submitted glyphs share atlas pages. An allocation remains occupied until its
//! last prepared, in-flight or cache reference goes away, including aborted frames.
use super::{
    Gpu, GpuError, glyph,
    renderer::{Budgets, Reservation, reserve},
};
use guillotiere::{AllocId, AtlasAllocator, size2};
use reel_core::budget::{BudgetPool, BudgetTicket};
use reel_ui::{
    frame::{FrameGlyphRun, RecordLocation, RenderFrame},
    resources::{ResourceHandle, ResourceLease, ResourceVersion},
};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, Weak},
};
use wgpu::util::DeviceExt;

#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) struct Key {
    run: Arc<RunKey>,
    glyph: u32,
}
#[derive(PartialEq, Eq, Hash)]
pub(super) struct RunKey {
    font: ResourceHandle,
    index: u32,
    size: u32,
    density: u64,
    hint: bool,
    coords: Vec<i16>,
    transform: Option<[u64; 6]>,
    embolden: [u64; 2],
    style: StyleKey,
}
#[derive(PartialEq, Eq, Hash)]
enum StyleKey {
    Fill(bool),
    Stroke {
        width: u64,
        join: u8,
        miter: u64,
        caps: [u8; 2],
        offset: u64,
        dashes: Vec<u64>,
    },
}
impl RunKey {
    pub fn new(run: &FrameGlyphRun, density: f64) -> Self {
        Self {
            font: run.font,
            index: run.font_index,
            size: run.font_size.to_bits(),
            density: density.to_bits(),
            hint: run.hint,
            coords: run.normalized_coords.to_vec(),
            transform: run.glyph_transform.map(|t| t.as_coeffs().map(f64::to_bits)),
            embolden: [run.font_embolden.x.to_bits(), run.font_embolden.y.to_bits()],
            style: match &run.style {
                reel_ui::peniko::Style::Fill(fill) => {
                    StyleKey::Fill(*fill == reel_ui::peniko::Fill::EvenOdd)
                }
                reel_ui::peniko::Style::Stroke(s) => StyleKey::Stroke {
                    width: s.width.to_bits(),
                    join: s.join as u8,
                    miter: s.miter_limit.to_bits(),
                    caps: [s.start_cap as u8, s.end_cap as u8],
                    offset: s.dash_offset.to_bits(),
                    dashes: s.dash_pattern.iter().map(|v| v.to_bits()).collect(),
                },
            },
        }
    }
}
impl Key {
    pub fn new(run: Arc<RunKey>, glyph: u32) -> Self {
        Self { run, glyph }
    }
}
pub(super) struct Page {
    pub view: wgpu::TextureView,
    texture: wgpu::Texture,
    allocator: Mutex<AtlasAllocator>,
    pub size: [u32; 2],
    residency: BudgetTicket,
}
pub(super) enum Glyph {
    Empty(ResourceLease),
    Image {
        page: Arc<Page>,
        allocation: AllocId,
        origin: [u32; 2],
        size: [u32; 2],
        offset: [f64; 2],
        source: ResourceLease,
    },
}
impl Glyph {
    fn source(&self) -> &ResourceLease {
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
impl GlyphCache {
    pub fn get(&self, key: &Key) -> Option<Arc<Glyph>> {
        self.entries.lock().unwrap().get(key).cloned()
    }
    pub fn publish(&self, frame: &FrameGlyphs) {
        let mut entries = self.entries.lock().unwrap();
        for (key, glyph) in &frame.entries {
            if !glyph.source().is_retired() {
                entries.insert(key.clone(), glyph.clone());
            }
        }
    }
    pub fn retire(&self, versions: &[ResourceVersion]) {
        self.entries.lock().unwrap().retain(|_, glyph| {
            !glyph.source().is_retired() && !versions.contains(glyph.source().version())
        });
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
        frame: &RenderFrame,
        run: &FrameGlyphRun,
        density: f64,
        index: usize,
        at: RecordLocation,
        budgets: &Budgets,
        local: &Budgets,
        tickets: &mut Vec<Reservation>,
        images: &super::image_cache::ImageCache,
    ) -> Result<Arc<Glyph>, GpuError> {
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
            available,
            max.saturating_sub(2),
        )?
        else {
            let source = frame
                .resources()
                .leases()
                .iter()
                .find(|r| r.handle() == run.font)
                .unwrap()
                .clone();
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
            let bytes = u64::from(size[0]) * u64::from(size[1]) * 4;
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
                format: wgpu::TextureFormat::Rgba8Unorm,
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
            source: frame
                .resources()
                .leases()
                .iter()
                .find(|r| r.handle() == run.font)
                .unwrap()
                .clone(),
        });
        let Glyph::Image { page, origin, .. } = glyph.as_ref() else {
            unreachable!()
        };
        // Upload the transparent border too: a recycled rectangle may contain old glyph pixels.
        let row = (requested.width as u32 * 4).div_ceil(256) * 256;
        let cost = u64::from(row) * requested.height as u64;
        images.make_room(&budgets.resources, cost * 2, gpu.limits.resource_bytes);
        self.make_room(&budgets.resources, cost * 2, gpu.limits.resource_bytes);
        tickets.push(reserve(
            &budgets.resources,
            &local.resources,
            cost * 2,
            gpu.limits.resource_bytes,
            "glyph upload",
        )?);
        let mut bytes = vec![0; cost as usize];
        for y in 0..bitmap.size[1] as usize {
            for x in 0..bitmap.size[0] as usize {
                let alpha = bitmap.alpha[y * bitmap.size[0] as usize + x];
                let start = (y + 1) * row as usize + (x + 1) * 4;
                bytes[start..start + 4].fill(alpha);
            }
        }
        let staging = gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("glyph atlas upload"),
                contents: &bytes,
                usage: wgpu::BufferUsages::COPY_SRC,
            });
        encoder.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: None,
                },
            },
            wgpu::TexelCopyTextureInfo {
                texture: &page.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: origin[0] - 1,
                    y: origin[1] - 1,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: requested.width as u32,
                height: requested.height as u32,
                depth_or_array_layers: 1,
            },
        );
        #[cfg(test)]
        {
            gpu.upload_creations
                .staging
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            gpu.upload_creations
                .copies
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(glyph)
    }
}
