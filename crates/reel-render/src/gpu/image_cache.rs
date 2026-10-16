//! Only submitted uploads may be reused by another preparation.
use super::{
    Gpu, GpuError,
    glyph_cache::GlyphCache,
    image_plan,
    renderer::{Budgets, Reservation, reserve},
    resources::{self, Image},
};
use reel_core::budget::{BudgetPool, BudgetTicket};
use reel_ui::resources::{ResourceHandle, ResourceLease, ResourceVersion};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Default)]
pub(super) struct ImageCache {
    images: Mutex<HashMap<ResourceHandle, Arc<Image>>>,
    uploads: Mutex<Vec<Upload>>,
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
        self.images.lock().unwrap().retain(|_, image| {
            let source = image.source.as_ref().unwrap();
            !source.lease.is_released() && !versions.contains(&source.version)
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
                        item.staging
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
        let reserve = |bytes| {
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
            let cost = item.resident + item.staging;
            glyphs.make_room(pool, cost, limit);
            self.make_room(pool, cost, limit);
            let reserved = (|| Ok((reserve(item.resident)?, reserve(item.staging)?)))();
            let (residency, upload) = match reserved {
                Ok(tickets) => tickets,
                Err(error) => {
                    deferred = Some(error);
                    break;
                }
            };
            let (mut image, _) = resources::image(gpu, &mut encoder, item.source, item.at, cost)?;
            image.residency = Some(residency);
            // Cache ownership must not pin decoded pixels. Existing frame leases
            // (including retired versions in a transition) may still reuse this
            // texture; the upload batch alone retains pixels through completion.
            image.source = Some(resources::ImageSource {
                version: item.source.version().clone(),
                lease: item.source.downgrade(),
            });
            let image = Arc::new(image);
            images.insert(handle, image.clone());
            uploaded.push(Copy {
                _image: image,
                source: item.source.clone(),
            });
            staging.push(upload);
        }
        if !uploaded.is_empty() {
            let done = Arc::new(AtomicBool::new(false));
            gpu.queue.submit([encoder.finish()]);
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
