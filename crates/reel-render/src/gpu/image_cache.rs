//! Only submitted uploads may be reused by another preparation.
use super::{
    Gpu, GpuError,
    glyph_cache::GlyphCache,
    image_plan,
    renderer::{Budgets, Reservation, reserve},
    resources::{self, Image},
};
use crate::common::resources::CachedImages;
use reel_core::budget::{BudgetPool, BudgetTicket};
use reel_ui::resources::{
    ResourceHandle, ResourceLease, ResourcePixels, ResourceVersion, ResourceView,
};
use std::{
    collections::{HashMap, hash_map::Entry},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Default)]
pub(super) struct ImageCache {
    images: Mutex<CachedImages<Arc<Image>>>,
    uploads: Mutex<Vec<Upload>>,
    partial: Mutex<HashMap<ResourceHandle, Partial>>,
    staging: Mutex<super::upload::Cache>,
    retired: Mutex<crate::common::resources::RetiredImages<Arc<Image>>>,
}

struct Partial {
    image: Arc<Image>,
    pixels: ResourcePixels,
    next_row: u32,
    end_row: u32,
    _storage: Reservation,
}

struct Upload {
    done: Arc<AtomicBool>,
    copies: Vec<Copy>,
    _admission: BudgetTicket,
    _storage: Reservation,
    staging: super::upload::Batch,
}
struct Copy {
    image: Arc<Image>,
    source: ResourceLease,
    next_row: Option<u32>,
}

impl ImageCache {
    pub(super) fn prepare_staging(
        &self,
        gpu: &Gpu,
        budgets: &Budgets,
        viewport_upload: u64,
    ) -> Result<(), GpuError> {
        self.staging
            .lock()
            .unwrap()
            .warm(gpu, budgets, viewport_upload)
    }
    pub(super) fn submitted(&self) {
        self.images.lock().unwrap().submitted();
        self.retired.lock().unwrap().submitted();
    }
    pub(super) fn usage(&self) -> [u64; 3] {
        let bytes = |image: &Arc<Image>| image.residency.as_ref().map_or(0, BudgetTicket::bytes);
        let cached = self.images.lock().unwrap().values().map(bytes).sum();
        let mut retired = 0;
        let mut readers = 0;
        for image in self.retired.lock().unwrap().iter() {
            let size = bytes(image);
            retired += size;
            if !reusable(image) {
                readers += size;
            }
        }
        [cached, retired, readers]
    }
    pub(super) fn retain_demand(&self, required: &[image_plan::Required<'_>]) {
        self.images.lock().unwrap().retain(|handle, image| {
            Arc::strong_count(image) > 1
                || required.iter().any(|item| item.source.handle() == *handle)
        });
        self.retired.lock().unwrap().clear();
    }
    pub(super) fn clear_idle(&self) -> bool {
        let mut cache = self.staging.lock().unwrap();
        let mut retired = self.retired.lock().unwrap();
        let had_storage = cache.capacity() != 0 || retired.iter().next().is_some();
        cache.clear();
        retired.clear();
        had_storage
    }
    pub(super) fn retire(&self, versions: &[ResourceVersion]) {
        let uploads = self.uploads.lock().unwrap();
        for upload in uploads.iter() {
            for copy in &upload.copies {
                if versions.contains(copy.source.version()) {
                    copy.source.prohibit_new_leases();
                }
            }
        }
        self.partial.lock().unwrap().retain(|_, pending| {
            let source = pending.image.source.as_ref().unwrap();
            source.retained(versions)
        });
        let mut cache = self.images.lock().unwrap();
        let mut retired = self.retired.lock().unwrap();
        for image in cache.retire(|image| !image.source.as_ref().unwrap().retained(versions)) {
            let uses = image.reuse_count;
            retired.put(image, uses);
        }
        retired.prune_cold(reusable);
    }

    pub(super) fn prepare(
        &self,
        gpu: &Gpu,
        required: &mut [image_plan::Required<'_>],
        budgets: &Budgets,
        local: &Budgets,
        admission: &BudgetPool,
        glyphs: &GlyphCache,
    ) -> Result<HashMap<ResourceHandle, Arc<Image>>, GpuError> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::ImageAllocate);
        // Serialize cold preparations, so concurrent callers cannot upload the
        // same texture twice. Queue ordering makes submitted copies reusable.
        let mut uploads = self.uploads.lock().unwrap();
        let mut partial = self.partial.lock().unwrap();
        // A new candidate may abandon a large image midway through upload.
        // Submitted copies retain their own Arc until GPU completion.
        partial.retain(|handle, _| required.iter().any(|item| item.source.handle() == *handle));
        let mut images = HashMap::with_capacity(required.len());
        {
            let mut cache = self.images.lock().unwrap();
            for item in required.iter() {
                if let Some(image) = cache.touch(&item.source.handle()) {
                    if item.source.pixel_discard_pending() {
                        image.discard_pending.store(true, Ordering::Relaxed);
                    }
                    images.insert(item.source.handle(), image.clone());
                }
            }
        }
        // A resident frame already holds the reservations for every image.
        if images.len() == required.len() {
            return Ok(images);
        }
        if let Some(demand) = crate::common::image_plan::missing_pixels(
            required
                .iter()
                .filter(|item| {
                    !images.contains_key(&item.source.handle())
                        && !partial.contains_key(&item.source.handle())
                })
                .map(|item| item.source),
            &budgets.transient,
            gpu.limits.transient_bytes,
        )? {
            return Err(GpuError::PixelsNeeded(demand));
        }
        let limit = gpu.limits.resource_bytes;
        let pool = &budgets.resources;
        image_plan::peak(
            required.iter().map(|item| {
                (
                    item.resident,
                    if images.contains_key(&item.source.handle()) {
                        0
                    } else {
                        item.minimum_staging
                    },
                )
            }),
            limit,
        )?;
        let admitted = admission.reserve(0).map_err(|_| GpuError::Busy)?;
        let capacity = required.len() - images.len();
        let mut buffer_slots = capacity + self.staging.lock().unwrap().mapped_count();
        let reserve_storage = |slots| {
            reserve(
                &budgets.transient,
                &local.transient,
                (capacity * (size_of::<Copy>() + size_of::<BudgetTicket>())
                    + slots * size_of::<Box<super::upload::Buffer>>()
                    + size_of::<Upload>()) as u64,
                gpu.limits.transient_bytes,
                "image upload metadata",
            )
        };
        let storage = reserve_storage(buffer_slots).or_else(|error| {
            if buffer_slots == capacity {
                return Err(error);
            }
            buffer_slots = capacity;
            reserve_storage(capacity)
        })?;
        if capacity > 1 && buffer_slots > capacity {
            // All these uploads gate the same frame. Complete a previously
            // started image first, then use single spans for smaller images
            // before spending the remaining mapped rows on larger uploads.
            required.sort_unstable_by_key(|item| {
                let handle = item.source.handle();
                (!partial.contains_key(&handle), item.staging, handle)
            });
        }
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("image upload batch"),
            });
        let mut uploaded = Vec::with_capacity(capacity);
        let mut staging = super::upload::Batch::new(capacity, buffer_slots);
        let reserve_gpu = |bytes| super::upload::reserve(gpu, budgets, bytes);
        let mut deferred = None;
        let mut work_remaining = gpu.limits.image_upload_bytes_per_prepare;
        let mut converted = false;
        for item in required {
            let handle = item.source.handle();
            if images.contains_key(&handle) {
                continue;
            }
            let row = if item.raster_format.is_some() {
                Some(item.minimum_staging)
            } else {
                item.texture.map(resources::texture_row_bytes)
            };
            let staging_limit = if let Some(row) = row {
                if converted && work_remaining < row {
                    deferred = Some(GpuError::Preparing);
                    break;
                }
                work_remaining.max(row)
            } else {
                item.staging
            };
            let pending = partial.get(&handle);
            let mut recycled = if pending.is_some() {
                None
            } else {
                self.recycle(item)
            };
            let resident = if recycled.is_some() || pending.is_some() {
                0
            } else {
                item.resident
            };
            let raster = item.raster_format.map(|format| {
                let source = item
                    .source
                    .image_descriptor()
                    .expect("planned raster image");
                let rows = if let Some(pending) = pending {
                    pending.next_row..pending.end_row
                } else if recycled.is_some() {
                    // Reused storage may hold pixels in otherwise transparent rows.
                    0..source.height
                } else {
                    resources::fresh_raster_rows(item.source, source.height)
                };
                (format, source, rows)
            });
            let new_staging = |requested| {
                let cache = self.staging.lock().unwrap();
                if let Some(row) = row {
                    staging.new_bytes(requested, staging.reusable_row_bytes(&cache, row))
                } else if cache.capacity() >= requested {
                    0
                } else {
                    requested
                }
            };
            let minimum_staging = new_staging(item.minimum_staging);
            let minimum = resident + minimum_staging;
            // Evict for this slice, rather than a whole-image staging buffer.
            // Only the minimum row is an admission requirement.
            let preferred_upload = raster
                .as_ref()
                .map_or(item.staging, |(_, _, rows)| {
                    item.minimum_staging * u64::from(rows.end - rows.start)
                })
                .min(staging_limit);
            let preferred_upload = row.map_or(preferred_upload, |row| preferred_upload / row * row);
            let preferred = resident + new_staging(preferred_upload);
            glyphs.make_room(pool, preferred, limit);
            self.make_room(pool, preferred, limit);
            let mut available = limit.saturating_sub(pool.usage().bytes);
            if minimum > available && minimum_staging != 0 {
                self.staging.lock().unwrap().clear();
                available = limit.saturating_sub(pool.usage().bytes);
            }
            if minimum > available {
                deferred = Some(GpuError::Pressure {
                    domain: "image upload",
                    requested: minimum,
                    available,
                });
                break;
            }
            if let Some((format, source, rows)) = raster {
                let row_bytes = item.minimum_staging;
                if let Entry::Vacant(entry) = partial.entry(handle) {
                    let storage = reserve(
                        &budgets.transient,
                        &local.transient,
                        (size_of::<Partial>() + 4 * size_of::<ResourceHandle>()) as u64,
                        gpu.limits.transient_bytes,
                        "partial image upload",
                    )?;
                    let mut image = if let Some(image) = recycled.take() {
                        image
                    } else {
                        let residency = reserve_gpu(item.resident)?;
                        let mut image =
                            resources::create_image(gpu, [source.width, source.height], format)?;
                        image.residency = Some(residency);
                        image
                    };
                    image.layout = reel_core::texture::TextureLayout::identity(image.size);
                    image.encoding = source.encoding;
                    image
                        .discard_pending
                        .store(item.source.pixel_discard_pending(), Ordering::Relaxed);
                    image.source = Some(resources::ResourceSource {
                        version: item.source.version().clone(),
                        lease: item.source.downgrade(),
                    });
                    entry.insert(Partial {
                        image: Arc::new(image),
                        pixels: crate::common::image_plan::required_pixels(
                            item.source,
                            &budgets.transient,
                            gpu.limits.transient_bytes,
                        )?,
                        next_row: rows.start,
                        end_row: rows.end,
                        _storage: storage,
                    });
                }
                let pending = &partial[&handle];
                let count = resources::rows_per_upload(
                    pending.end_row,
                    pending.next_row,
                    staging
                        .row_capacity(
                            &self.staging.lock().unwrap(),
                            row_bytes,
                            limit.saturating_sub(pool.usage().bytes),
                        )
                        .min(staging_limit),
                    row_bytes,
                );
                if count == 0 {
                    deferred = Some(GpuError::Pressure {
                        domain: "image upload",
                        requested: item.minimum_staging,
                        available: limit.saturating_sub(pool.usage().bytes),
                    });
                    break;
                }
                let next_row = pending.next_row + count;
                let upload_bytes = row_bytes * u64::from(count);
                match staging.copy_rows(
                    gpu,
                    budgets,
                    &self.staging,
                    (pending.next_row..next_row, row_bytes),
                    |rows, span| {
                        resources::image_rows(
                            gpu,
                            &mut encoder,
                            (item.source, &pending.pixels),
                            item.at,
                            &pending.image,
                            rows,
                            span,
                        )
                    },
                ) {
                    Ok(()) => (),
                    Err(error @ GpuError::Pressure { .. }) => {
                        deferred = Some(error);
                        break;
                    }
                    Err(error) => return Err(error),
                }
                work_remaining = work_remaining.saturating_sub(upload_bytes);
                converted = true;
                uploaded.push(Copy {
                    image: pending.image.clone(),
                    source: item.source.clone(),
                    next_row: Some(next_row),
                });
                if next_row == pending.end_row {
                    images.insert(handle, pending.image.clone());
                } else {
                    deferred = Some(GpuError::Preparing);
                    break;
                }
            } else if let Some(descriptor) = item.source.texture_descriptor() {
                let layout = item.texture.expect("planned texture");
                if let Entry::Vacant(entry) = partial.entry(handle) {
                    let storage = reserve(
                        &budgets.transient,
                        &local.transient,
                        (size_of::<Partial>() + 4 * size_of::<ResourceHandle>()) as u64,
                        gpu.limits.transient_bytes,
                        "partial texture upload",
                    )?;
                    let mut image = if let Some(image) = recycled.take() {
                        image
                    } else {
                        let residency = reserve_gpu(item.resident)?;
                        let mut image =
                            resources::create_texture(gpu, descriptor, layout, item.at)?;
                        image.residency = Some(residency);
                        image
                    };
                    image.layout = layout.layout;
                    image.encoding = descriptor.source_encoding;
                    image
                        .discard_pending
                        .store(item.source.pixel_discard_pending(), Ordering::Relaxed);
                    image.source = Some(resources::ResourceSource {
                        version: item.source.version().clone(),
                        lease: item.source.downgrade(),
                    });
                    entry.insert(Partial {
                        image: Arc::new(image),
                        pixels: crate::common::image_plan::required_pixels(
                            item.source,
                            &budgets.transient,
                            gpu.limits.transient_bytes,
                        )?,
                        next_row: 0,
                        end_row: layout.rows,
                        _storage: storage,
                    });
                }
                let pending = &partial[&handle];
                let (level, level_row) = layout.at_row(descriptor, pending.next_row);
                let row_bytes = resources::texture_row_bytes(level);
                let count = resources::rows_per_upload(
                    level.rows,
                    level_row,
                    staging
                        .row_capacity(
                            &self.staging.lock().unwrap(),
                            row_bytes,
                            limit.saturating_sub(pool.usage().bytes),
                        )
                        .min(staging_limit),
                    row_bytes,
                );
                if count == 0 {
                    deferred = Some(GpuError::Pressure {
                        domain: "texture upload",
                        requested: row_bytes,
                        available: limit.saturating_sub(pool.usage().bytes),
                    });
                    break;
                }
                let next_row = pending.next_row + count;
                match staging.copy_rows(
                    gpu,
                    budgets,
                    &self.staging,
                    (level_row..level_row + count, row_bytes),
                    |rows, span| {
                        resources::texture_rows(
                            gpu,
                            &mut encoder,
                            (item.source, &pending.pixels),
                            &pending.image,
                            level,
                            rows,
                            span,
                        )
                    },
                ) {
                    Ok(()) => (),
                    Err(error @ GpuError::Pressure { .. }) => {
                        deferred = Some(error);
                        break;
                    }
                    Err(error) => return Err(error),
                }
                work_remaining = work_remaining.saturating_sub(row_bytes * u64::from(count));
                converted = true;
                uploaded.push(Copy {
                    image: pending.image.clone(),
                    source: item.source.clone(),
                    next_row: Some(next_row),
                });
                if next_row == pending.end_row {
                    images.insert(handle, pending.image.clone());
                } else {
                    deferred = Some(GpuError::Preparing);
                    break;
                }
            } else if let ResourceView::Yuv420(source) = item.source.view() {
                let residency = if let Some(image) = &mut recycled {
                    image.residency.take().unwrap()
                } else {
                    reserve_gpu(item.resident)?
                };
                let upload = match self
                    .staging
                    .lock()
                    .unwrap()
                    .acquire(gpu, budgets, item.staging)
                {
                    Ok(buffer) => buffer,
                    Err(error) => {
                        deferred = Some(error);
                        break;
                    }
                };
                if upload.is_none() {
                    staging.one_off(reserve_gpu(item.staging)?);
                }
                let mut image =
                    super::video::upload(gpu, &mut encoder, source, recycled, upload.as_deref())?;
                staging.buffers.extend(upload);
                image.residency = Some(residency);
                image.source = Some(resources::ResourceSource {
                    version: item.source.version().clone(),
                    lease: item.source.downgrade(),
                });
                let image = Arc::new(image);
                images.insert(handle, image.clone());
                uploaded.push(Copy {
                    image,
                    source: item.source.clone(),
                    next_row: None,
                });
            }
        }
        if !uploaded.is_empty() {
            for buffer in &staging.buffers {
                buffer.finish_spans(&mut encoder);
            }
            let done = Arc::new(AtomicBool::new(false));
            gpu.submit(encoder);
            // Advance source cursors only after submission. Failed encoding
            // retries the same rows; no half-uploaded image enters the cache.
            {
                let mut cache = self.images.lock().unwrap();
                for copy in &uploaded {
                    let handle = copy.source.handle();
                    if let Some(next_row) = copy.next_row {
                        let pending = partial.get_mut(&handle).unwrap();
                        if next_row != pending.end_row {
                            pending.next_row = next_row;
                            continue;
                        }
                        partial.remove(&handle);
                    }
                    cache.insert(handle, copy.image.clone());
                }
            }
            let signal = done.clone();
            gpu.queue
                .on_submitted_work_done(move || signal.store(true, Ordering::Release));
            uploads.push(Upload {
                done,
                copies: uploaded,
                _admission: admitted,
                _storage: storage,
                staging,
            });
            gpu.check()?;
        }
        match deferred {
            Some(error) => {
                if matches!(error, GpuError::Pressure { .. }) {
                    self.staging.lock().unwrap().clear();
                }
                Err(error)
            }
            None => Ok(images),
        }
    }

    pub(super) fn discard_uploaded_pixels(&self, submitted: &HashMap<ResourceHandle, Arc<Image>>) {
        let cached = self.images.lock().unwrap();
        for (handle, image) in submitted {
            if !image.discard_pending.load(Ordering::Relaxed) {
                continue;
            }
            if !cached
                .get(handle)
                .is_some_and(|current| Arc::ptr_eq(current, image))
            {
                continue;
            }
            let source = image
                .source
                .as_ref()
                .and_then(|source| source.lease.upgrade());
            if source.is_none_or(|source| {
                !source.pixel_discard_pending() || source.discard_uploaded_pixels()
            }) {
                image.discard_pending.store(false, Ordering::Relaxed);
            }
        }
    }

    pub(super) fn has_pending(&self) -> bool {
        !self.uploads.lock().unwrap().is_empty()
    }

    fn recycle(&self, item: &image_plan::Required<'_>) -> Option<Image> {
        let image = self.retired.lock().unwrap().take_if(|image| {
            reusable(image)
                && image
                    .residency
                    .as_ref()
                    .is_some_and(|ticket| ticket.bytes() == item.resident)
                && if let Some(source) = item.source.image_descriptor() {
                    image.yuv.is_none()
                        && image.texture_format.is_none()
                        && Some(image.texture.format()) == item.raster_format
                        && image.size == [source.width, source.height]
                } else if let Some(descriptor) = item.source.texture_descriptor() {
                    image.yuv.is_none()
                        && image.texture_format == Some(descriptor.format)
                        && image.texture.mip_level_count() == descriptor.mip_count
                        && image.size == item.texture.expect("planned texture").layout.storage_size
                } else if let ResourceView::Yuv420(source) = item.source.view() {
                    image.size == source.size()
                        && image
                            .yuv
                            .as_ref()
                            .is_some_and(|planes| planes.layout == source.layout())
                } else {
                    false
                }
        })?;
        let mut image = Arc::try_unwrap(image).ok()?;
        image.reuse_count = image.reuse_count.saturating_add(1);
        Some(image)
    }

    pub(super) fn poll(&self) -> bool {
        let mut uploads = self.uploads.lock().unwrap();
        let before = uploads.len();
        let mut index = 0;
        while index < uploads.len() {
            if uploads[index].done.load(Ordering::Acquire)
                && uploads[index].staging.buffers.iter().all(|b| b.settled())
            {
                let mut upload = uploads.swap_remove(index);
                let mut cache = self.staging.lock().unwrap();
                for buffer in upload.staging.buffers.drain(..) {
                    cache.put(buffer);
                }
            } else {
                index += 1;
            }
        }
        uploads.len() != before
    }

    pub(super) fn make_room(&self, pool: &BudgetPool, cost: u64, limit: u64) {
        // In-flight and prepared users hold their own Arc. Only unused cache
        // entries can release residency, without waiting for the device here.
        if cost > limit.saturating_sub(pool.usage().bytes) {
            self.retired
                .lock()
                .unwrap()
                .reclaim(pool, cost, limit, reusable);
            self.images
                .lock()
                .unwrap()
                .reclaim(pool, cost, limit, |image| Arc::strong_count(image) == 1);
        }
    }
}

fn reusable(image: &Arc<Image>) -> bool {
    Arc::strong_count(image) == 1 && image.source.as_ref().is_some_and(|s| s.lease.is_released())
}

pub(super) fn reserve_local(
    local: &BudgetPool,
    bytes: u64,
    limit: u64,
) -> Result<BudgetTicket, GpuError> {
    local.reserve(bytes).map_err(|_| GpuError::Budget {
        domain: "image upload",
        requested: bytes,
        available: limit.saturating_sub(local.usage().bytes),
    })
}
