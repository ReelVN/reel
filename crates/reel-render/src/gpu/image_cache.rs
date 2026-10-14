//! Only submitted uploads may be reused by another preparation.
use super::{GpuError, renderer::Reservation, resources::Image};
use reel_core::budget::{BudgetPool, BudgetTicket};
use reel_ui::resources::{ResourceHandle, ResourceVersion};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Default)]
pub(super) struct ImageCache(Mutex<HashMap<ResourceHandle, Arc<Image>>>);

impl ImageCache {
    pub(super) fn get(&self, handle: ResourceHandle) -> Option<Arc<Image>> {
        self.0.lock().unwrap().get(&handle).cloned()
    }

    pub(super) fn publish(&self, images: &HashMap<ResourceHandle, Arc<Image>>) {
        let mut cache = self.0.lock().unwrap();
        for (&handle, image) in images {
            if !image.source.as_ref().unwrap().is_retired() {
                cache.insert(handle, image.clone());
            }
        }
    }

    pub(super) fn retire(&self, versions: &[ResourceVersion]) {
        self.0.lock().unwrap().retain(|_, image| {
            let source = image.source.as_ref().unwrap();
            !source.is_retired() && !versions.contains(source.version())
        });
    }

    pub(super) fn reserve_upload(
        &self,
        pool: &BudgetPool,
        local: &BudgetPool,
        cost: u64,
        resident: u64,
        limit: u64,
    ) -> Result<(Reservation, BudgetTicket), GpuError> {
        let local = reserve_local(local, cost, limit)?;
        self.make_room(pool, cost, limit);
        let reserve = |bytes| {
            pool.reserve(bytes).map_err(|_| GpuError::Pressure {
                domain: "image upload",
                requested: bytes,
                available: limit.saturating_sub(pool.usage().bytes),
            })
        };
        let residency = reserve(resident)?;
        let upload = reserve(cost - resident)?;
        Ok((
            Reservation {
                _shared: Some(upload),
                _local: local,
            },
            residency,
        ))
    }

    pub(super) fn make_room(&self, pool: &BudgetPool, cost: u64, limit: u64) {
        // In-flight and prepared users hold their own Arc. Only unused cache
        // entries can release residency, without waiting for the device here.
        if cost > limit.saturating_sub(pool.usage().bytes) {
            self.0.lock().unwrap().retain(|_, image| {
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
