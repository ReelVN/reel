//! Only submitted uploads may be reused by another preparation.
use super::{
    Gpu, GpuError,
    glyph_cache::GlyphCache,
    image_plan,
    renderer::{Budgets, Reservation, reserve},
    resources::{self, Image},
};
use reel_core::budget::{BudgetPool, BudgetTicket};
use reel_ui::resources::{
    ResourceData, ResourceHandle, ResourceLease, ResourcePixels, ResourceVersion,
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
    images: Mutex<HashMap<ResourceHandle, Arc<Image>>>,
    uploads: Mutex<Vec<Upload>>,
    partial: Mutex<HashMap<ResourceHandle, Partial>>,
    staging: Mutex<super::upload::Cache>,
    retired: Mutex<crate::common::resources::RetiredImages<Arc<Image>>>,
}

struct Partial {
    image: Arc<Image>,
    pixels: ResourcePixels,
    next_row: u32,
    total_rows: u32,
    _storage: Reservation,
}

struct Upload {
    done: Arc<AtomicBool>,
    copies: Vec<Copy>,
    _staging: Vec<BudgetTicket>,
    _admission: BudgetTicket,
    _storage: Reservation,
    #[expect(
        clippy::vec_box,
        reason = "Each upload owns a separately charged allocation reused by the cache"
    )]
    buffers: Vec<Box<super::upload::Buffer>>,
}
struct Copy {
    _image: Arc<Image>,
    source: ResourceLease,
    next_row: Option<u32>,
}

impl ImageCache {
    pub(super) fn submitted(&self) {
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
    fn publish(&self, images: &HashMap<ResourceHandle, Arc<Image>>) {
        let mut cache = self.images.lock().unwrap();
        for (&handle, image) in images {
            if !image.source.as_ref().unwrap().lease.is_released() {
                cache.insert(handle, image.clone());
            }
        }
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
        for (_, image) in
            cache.extract_if(|_, image| !image.source.as_ref().unwrap().retained(versions))
        {
            let uses = image.reuse_count;
            retired.put(image, uses);
        }
        retired.prune_cold(reusable);
    }

    pub(super) fn prepare(
        &self,
        gpu: &Gpu,
        required: &[image_plan::Required<'_>],
        budgets: &Budgets,
        local: &Budgets,
        admission: &BudgetPool,
        glyphs: &GlyphCache,
    ) -> Result<HashMap<ResourceHandle, Arc<Image>>, GpuError> {
        // Serialize cold preparations, so concurrent callers cannot upload the
        // same texture twice. Queue ordering makes submitted copies reusable.
        let mut uploads = self.uploads.lock().unwrap();
        let mut partial = self.partial.lock().unwrap();
        // A new candidate may abandon a large image midway through upload.
        // Submitted copies retain their own Arc until GPU completion.
        partial.retain(|handle, _| required.iter().any(|item| item.source.handle() == *handle));
        let mut images = HashMap::with_capacity(required.len());
        {
            let cache = self.images.lock().unwrap();
            for item in required {
                if let Some(image) = cache.get(&item.source.handle()) {
                    images.insert(item.source.handle(), image.clone());
                }
            }
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
        if images.len() == required.len() {
            return Ok(images);
        }

        let admitted = admission.reserve(0).map_err(|_| GpuError::Busy)?;
        let capacity = required.len() - images.len();
        let storage = reserve(
            &budgets.transient,
            &local.transient,
            (capacity
                * (size_of::<Copy>()
                    + size_of::<BudgetTicket>()
                    + size_of::<Box<super::upload::Buffer>>())
                + size_of::<Upload>()) as u64,
            gpu.limits.transient_bytes,
            "image upload metadata",
        )?;
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("image upload batch"),
            });
        let mut uploaded = Vec::with_capacity(capacity);
        let mut staging = Vec::with_capacity(capacity);
        let mut buffers = Vec::with_capacity(capacity);
        let reserve_gpu = |bytes| {
            pool.reserve(bytes).map_err(|_| GpuError::Pressure {
                domain: "image upload",
                requested: bytes,
                available: limit.saturating_sub(pool.usage().bytes),
            })
        };
        let mut deferred = None;
        let mut work_remaining = gpu.limits.image_upload_bytes_per_prepare;
        let mut converted = false;
        for item in required {
            let handle = item.source.handle();
            if images.contains_key(&handle) {
                continue;
            }
            let row = match item.source.data() {
                ResourceData::Image { image, .. } => Some(resources::row_bytes(image.width)),
                ResourceData::Texture { .. } => Some(resources::texture_row_bytes(
                    item.texture.expect("planned texture"),
                )),
                _ => None,
            };
            let staging_limit = if let Some(row) = row {
                if converted && work_remaining < row {
                    deferred = Some(GpuError::Pressure {
                        domain: "image upload slice",
                        requested: row,
                        available: work_remaining,
                    });
                    break;
                }
                work_remaining.max(row)
            } else {
                item.staging
            };
            let mut recycled = if partial.contains_key(&handle) {
                None
            } else {
                self.recycle(item)
            };
            let resident = if recycled.is_some() || partial.contains_key(&handle) {
                0
            } else {
                item.resident
            };
            let cached_staging = self.staging.lock().unwrap().capacity();
            let minimum = resident
                + if cached_staging >= item.minimum_staging {
                    0
                } else {
                    item.minimum_staging
                };
            // Evict for this slice, rather than a whole-image staging buffer.
            // Only the minimum row is an admission requirement.
            let preferred_upload = item.staging.min(staging_limit);
            let preferred = resident
                + if cached_staging >= preferred_upload {
                    0
                } else {
                    preferred_upload
                };
            glyphs.make_room(pool, preferred, limit);
            self.make_room(pool, preferred, limit);
            let mut available = limit.saturating_sub(pool.usage().bytes);
            if minimum > available && cached_staging < item.minimum_staging {
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
            if let ResourceData::Image {
                image: source,
                encoding,
            } = item.source.data()
            {
                let row_bytes = resources::row_bytes(source.width);
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
                            resources::create_image(gpu, [source.width, source.height])?;
                        image.residency = Some(residency);
                        image
                    };
                    image.layout = reel_core::texture::TextureLayout::identity(image.size);
                    image.encoding = *encoding;
                    image.source = Some(resources::ResourceSource {
                        version: item.source.version().clone(),
                        lease: item.source.downgrade(),
                    });
                    entry.insert(Partial {
                        image: Arc::new(image),
                        pixels: item.source.read_pixels().ok_or(GpuError::Unsupported {
                            at: item.at,
                            record: "missing image pixels",
                        })?,
                        next_row: 0,
                        total_rows: source.height,
                        _storage: storage,
                    });
                }
                let pending = &partial[&handle];
                let count = resources::rows_per_upload(
                    pending.image.size,
                    pending.next_row,
                    limit
                        .saturating_sub(pool.usage().bytes)
                        .max(self.staging.lock().unwrap().capacity())
                        .min(staging_limit),
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
                let upload_bytes = resources::row_bytes(source.width) * u64::from(count);
                let upload = match self
                    .staging
                    .lock()
                    .unwrap()
                    .acquire(gpu, budgets, upload_bytes)
                {
                    Ok(ticket) => ticket,
                    Err(error) => {
                        deferred = Some(error);
                        break;
                    }
                };
                let one_off = if upload.is_none() {
                    Some(reserve_gpu(upload_bytes)?)
                } else {
                    None
                };
                resources::image_rows(
                    gpu,
                    &mut encoder,
                    (item.source, &pending.pixels),
                    item.at,
                    &pending.image,
                    pending.next_row..next_row,
                    upload.as_deref(),
                )?;
                work_remaining = work_remaining.saturating_sub(row_bytes * u64::from(count));
                converted = true;
                uploaded.push(Copy {
                    _image: pending.image.clone(),
                    source: item.source.clone(),
                    next_row: Some(next_row),
                });
                if let Some(upload) = upload {
                    buffers.push(upload);
                }
                if let Some(one_off) = one_off {
                    staging.push(one_off);
                }
                if next_row == source.height {
                    images.insert(handle, pending.image.clone());
                } else {
                    deferred = Some(GpuError::Pressure {
                        domain: "image upload",
                        requested: item.minimum_staging,
                        available: limit.saturating_sub(pool.usage().bytes),
                    });
                    break;
                }
            } else if let ResourceData::Texture { descriptor, .. } = item.source.data() {
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
                            resources::create_texture(gpu, *descriptor, layout, item.at)?;
                        image.residency = Some(residency);
                        image
                    };
                    image.layout = layout.layout;
                    image.encoding = descriptor.source_encoding;
                    image.source = Some(resources::ResourceSource {
                        version: item.source.version().clone(),
                        lease: item.source.downgrade(),
                    });
                    entry.insert(Partial {
                        image: Arc::new(image),
                        pixels: item.source.read_pixels().ok_or(GpuError::Unsupported {
                            at: item.at,
                            record: "missing image pixels",
                        })?,
                        next_row: 0,
                        total_rows: layout.rows,
                        _storage: storage,
                    });
                }
                let pending = &partial[&handle];
                let (level, level_row) = layout.at_row(*descriptor, pending.next_row);
                let row_bytes = resources::texture_row_bytes(level);
                let count = (limit
                    .saturating_sub(pool.usage().bytes)
                    .max(self.staging.lock().unwrap().capacity())
                    .min(staging_limit)
                    / row_bytes)
                    .min(u64::from(level.rows - level_row)) as u32;
                if count == 0 {
                    deferred = Some(GpuError::Pressure {
                        domain: "texture upload",
                        requested: row_bytes,
                        available: limit.saturating_sub(pool.usage().bytes),
                    });
                    break;
                }
                let next_row = pending.next_row + count;
                let upload_bytes = row_bytes * u64::from(count);
                let upload = match self
                    .staging
                    .lock()
                    .unwrap()
                    .acquire(gpu, budgets, upload_bytes)
                {
                    Ok(ticket) => ticket,
                    Err(error) => {
                        deferred = Some(error);
                        break;
                    }
                };
                let one_off = if upload.is_none() {
                    Some(reserve_gpu(upload_bytes)?)
                } else {
                    None
                };
                resources::texture_rows(
                    gpu,
                    &mut encoder,
                    (item.source, &pending.pixels),
                    &pending.image,
                    level,
                    level_row..level_row + count,
                    upload.as_deref(),
                )?;
                work_remaining = work_remaining.saturating_sub(row_bytes * u64::from(count));
                converted = true;
                uploaded.push(Copy {
                    _image: pending.image.clone(),
                    source: item.source.clone(),
                    next_row: Some(next_row),
                });
                if let Some(upload) = upload {
                    buffers.push(upload);
                }
                if let Some(one_off) = one_off {
                    staging.push(one_off);
                }
                if next_row == pending.total_rows {
                    images.insert(handle, pending.image.clone());
                } else {
                    deferred = Some(GpuError::Pressure {
                        domain: "texture upload slice",
                        requested: row_bytes,
                        available: limit.saturating_sub(pool.usage().bytes),
                    });
                    break;
                }
            } else if let ResourceData::Yuv420(source) = item.source.data() {
                let residency = if let Some(image) = &mut recycled {
                    image.residency.take().unwrap()
                } else {
                    reserve_gpu(item.resident)?
                };
                let upload = reserve_gpu(item.staging)?;
                let mut image = super::video::upload(gpu, &mut encoder, source, recycled)?;
                image.residency = Some(residency);
                image.source = Some(resources::ResourceSource {
                    version: item.source.version().clone(),
                    lease: item.source.downgrade(),
                });
                let image = Arc::new(image);
                images.insert(handle, image.clone());
                uploaded.push(Copy {
                    _image: image,
                    source: item.source.clone(),
                    next_row: None,
                });
                staging.push(upload);
            }
        }
        if !uploaded.is_empty() {
            let done = Arc::new(AtomicBool::new(false));
            gpu.queue.submit([encoder.finish()]);
            // Advance source cursors only after submission. Failed encoding
            // retries the same rows; no half-uploaded image enters the cache.
            for copy in &uploaded {
                if let Some(next_row) = copy.next_row {
                    let handle = copy.source.handle();
                    if next_row == partial[&handle].total_rows {
                        partial.remove(&handle);
                    } else {
                        partial.get_mut(&handle).unwrap().next_row = next_row;
                    }
                }
            }
            self.publish(&images);
            let signal = done.clone();
            gpu.queue
                .on_submitted_work_done(move || signal.store(true, Ordering::Release));
            uploads.push(Upload {
                done,
                copies: uploaded,
                _staging: staging,
                _admission: admitted,
                _storage: storage,
                buffers,
            });
            gpu.check()?;
        }
        match deferred {
            Some(error) => {
                self.staging.lock().unwrap().clear();
                Err(error)
            }
            None => Ok(images),
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
                && match item.source.data() {
                    ResourceData::Yuv420(source) => {
                        image.size == source.size()
                            && image
                                .yuv
                                .as_ref()
                                .is_some_and(|planes| planes.layout == source.layout())
                    }
                    ResourceData::Image { image: source, .. } => {
                        image.yuv.is_none()
                            && image.texture_format.is_none()
                            && image.size == [source.width, source.height]
                    }
                    ResourceData::Texture { descriptor, .. } => {
                        image.yuv.is_none()
                            && image.texture_format == Some(descriptor.format)
                            && image.texture.mip_level_count() == descriptor.mip_count
                            && image.size
                                == item.texture.expect("planned texture").layout.storage_size
                    }
                    _ => false,
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
                && uploads[index].buffers.iter().all(|b| b.settled())
            {
                let mut upload = uploads.swap_remove(index);
                let mut cache = self.staging.lock().unwrap();
                for buffer in upload.buffers.drain(..) {
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
            self.retired.lock().unwrap().clear();
            self.images.lock().unwrap().retain(|_, image| {
                cost <= limit.saturating_sub(pool.usage().bytes) || Arc::strong_count(image) > 1
            });
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
