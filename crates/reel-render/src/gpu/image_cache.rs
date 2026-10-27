//! Only submitted uploads may be reused by another preparation.
use super::{
    Gpu, GpuError,
    glyph_cache::GlyphCache,
    image_plan,
    renderer::{Budgets, Reservation, reserve},
    resources::{self, Image},
};
use reel_core::budget::{BudgetPool, BudgetTicket};
use reel_ui::resources::{ResourceData, ResourceHandle, ResourceLease, ResourceVersion};
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
}

struct Partial {
    image: Arc<Image>,
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
    buffers: Vec<Box<super::upload::Buffer>>,
}
struct Copy {
    _image: Arc<Image>,
    source: ResourceLease,
    next_row: Option<u32>,
}

impl ImageCache {
    pub(super) fn clear_idle_uploads(&self) -> bool {
        let mut cache = self.staging.lock().unwrap();
        let had_storage = cache.capacity() != 0;
        cache.clear();
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
        self.images.lock().unwrap().retain(|_, image| {
            let source = image.source.as_ref().unwrap();
            // Dead video entries are an allocation pool, still charged to the
            // same resource budget. Reuse additionally requires no GPU reader.
            image.yuv.is_some() || source.retained(versions)
        });
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
            let mut recycled = match item.source.data() {
                ResourceData::Yuv420(image) => self.recycle_video(image.size(), image.layout()),
                _ => None,
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
                    let residency = reserve_gpu(item.resident)?;
                    let mut image = resources::create_image(gpu, [source.width, source.height])?;
                    image.encoding = *encoding;
                    image.residency = Some(residency);
                    image.source = Some(resources::ResourceSource {
                        version: item.source.version().clone(),
                        lease: item.source.downgrade(),
                    });
                    entry.insert(Partial {
                        image: Arc::new(image),
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
                    item.source,
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
                let row_bytes = resources::texture_row_bytes(layout);
                if let Entry::Vacant(entry) = partial.entry(handle) {
                    let storage = reserve(
                        &budgets.transient,
                        &local.transient,
                        (size_of::<Partial>() + 4 * size_of::<ResourceHandle>()) as u64,
                        gpu.limits.transient_bytes,
                        "partial texture upload",
                    )?;
                    let residency = reserve_gpu(item.resident)?;
                    let mut image = resources::create_texture(gpu, *descriptor, layout, item.at)?;
                    image.residency = Some(residency);
                    image.source = Some(resources::ResourceSource {
                        version: item.source.version().clone(),
                        lease: item.source.downgrade(),
                    });
                    entry.insert(Partial {
                        image: Arc::new(image),
                        next_row: 0,
                        total_rows: layout.rows,
                        _storage: storage,
                    });
                }
                let pending = &partial[&handle];
                let count = (limit
                    .saturating_sub(pool.usage().bytes)
                    .max(self.staging.lock().unwrap().capacity())
                    .min(staging_limit)
                    / row_bytes)
                    .min(u64::from(pending.total_rows - pending.next_row))
                    as u32;
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
                    item.source,
                    &pending.image,
                    layout,
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

    fn recycle_video(
        &self,
        size: [u32; 2],
        layout: reel_core::image::Yuv420Layout,
    ) -> Option<Image> {
        let mut cache = self.images.lock().unwrap();
        let handle = *cache
            .iter()
            .find(|(_, image)| {
                image
                    .yuv
                    .as_ref()
                    .is_some_and(|planes| planes.layout == layout)
                    && image.size == size
                    && reusable(image)
            })?
            .0;
        Arc::try_unwrap(cache.remove(&handle).unwrap()).ok()
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
