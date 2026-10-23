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
}

struct Partial {
    image: Arc<Image>,
    next_row: u32,
    _storage: Reservation,
}

struct Upload {
    done: Arc<AtomicBool>,
    copies: Vec<Copy>,
    _staging: Vec<BudgetTicket>,
    _admission: BudgetTicket,
    _storage: Reservation,
}
struct Copy {
    _image: Arc<Image>,
    source: ResourceLease,
    next_row: Option<u32>,
}

impl ImageCache {
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
            !source.lease.is_released() && !versions.contains(&source.version)
        });
        self.images.lock().unwrap().retain(|_, image| {
            let source = image.source.as_ref().unwrap();
            // Dead video entries are an allocation pool, still charged to the
            // same resource budget. Reuse additionally requires no GPU reader.
            image.yuv.is_some()
                || (!source.lease.is_released() && !versions.contains(&source.version))
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
            (capacity * (size_of::<Copy>() + size_of::<BudgetTicket>()) + size_of::<Upload>())
                as u64,
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
        let reserve_gpu = |bytes| {
            pool.reserve(bytes).map_err(|_| GpuError::Pressure {
                domain: "image upload",
                requested: bytes,
                available: limit.saturating_sub(pool.usage().bytes),
            })
        };
        let mut deferred = None;
        for item in required {
            let handle = item.source.handle();
            if images.contains_key(&handle) {
                continue;
            }
            let mut recycled = match item.source.data() {
                ResourceData::Yuv420(image) => self.recycle_video(image.size()),
                _ => None,
            };
            let resident = if recycled.is_some() || partial.contains_key(&handle) {
                0
            } else {
                item.resident
            };
            let minimum = resident + item.minimum_staging;
            // Prefer a single copy when evictable cache entries provide room;
            // only the minimum row is an admission requirement.
            let preferred = resident + item.staging;
            glyphs.make_room(pool, preferred, limit);
            self.make_room(pool, preferred, limit);
            let available = limit.saturating_sub(pool.usage().bytes);
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
                    image.source = Some(resources::ImageSource {
                        version: item.source.version().clone(),
                        lease: item.source.downgrade(),
                    });
                    entry.insert(Partial {
                        image: Arc::new(image),
                        next_row: 0,
                        _storage: storage,
                    });
                }
                let pending = &partial[&handle];
                let count = resources::rows_per_upload(
                    pending.image.size,
                    pending.next_row,
                    limit.saturating_sub(pool.usage().bytes),
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
                let upload =
                    match reserve_gpu(resources::row_bytes(source.width) * u64::from(count)) {
                        Ok(ticket) => ticket,
                        Err(error) => {
                            deferred = Some(error);
                            break;
                        }
                    };
                resources::image_rows(
                    gpu,
                    &mut encoder,
                    item.source,
                    item.at,
                    &pending.image,
                    pending.next_row..next_row,
                )?;
                uploaded.push(Copy {
                    _image: pending.image.clone(),
                    source: item.source.clone(),
                    next_row: Some(next_row),
                });
                staging.push(upload);
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
            } else if let ResourceData::Yuv420(source) = item.source.data() {
                let residency = if let Some(image) = &mut recycled {
                    image.residency.take().unwrap()
                } else {
                    reserve_gpu(item.resident)?
                };
                let upload = reserve_gpu(item.staging)?;
                let mut image = super::video::upload(gpu, &mut encoder, source, recycled)?;
                image.residency = Some(residency);
                image.source = Some(resources::ImageSource {
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
                    if next_row == copy._image.size[1] {
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
            });
            gpu.check()?;
        }
        match deferred {
            Some(error) => Err(error),
            None => Ok(images),
        }
    }

    pub(super) fn has_pending(&self) -> bool {
        !self.uploads.lock().unwrap().is_empty()
    }

    fn recycle_video(&self, size: [u32; 2]) -> Option<Image> {
        let mut cache = self.images.lock().unwrap();
        let handle = *cache
            .iter()
            .find(|(_, image)| image.yuv.is_some() && image.size == size && reusable(image))?
            .0;
        Arc::try_unwrap(cache.remove(&handle).unwrap()).ok()
    }

    pub(super) fn poll(&self) -> bool {
        let mut uploads = self.uploads.lock().unwrap();
        let before = uploads.len();
        uploads.retain(|upload| !upload.done.load(Ordering::Acquire));
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
