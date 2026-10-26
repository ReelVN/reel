//! Queue-ordered scratch targets. Every use is cleared and fully consumed within
//! its command buffer, so subsequent submissions can reuse the same allocations.
use super::{Gpu, GpuError, Quality, targets::Target};
use reel_core::budget::{BudgetPool, BudgetTicket};
use std::sync::{Arc, Mutex};

pub(super) struct Allocation {
    pub(super) target: Target,
    _resident: BudgetTicket,
}
struct Entries {
    size: [u32; 2],
    quality: Quality,
    slots: Vec<Arc<Allocation>>,
}
#[derive(Default)]
pub(super) struct TargetCache(Mutex<Option<Entries>>);

impl TargetCache {
    pub(super) fn configure(&self, size: [u32; 2], quality: Quality) {
        let mut cache = self.0.lock().unwrap();
        if let Some(entries) = cache.as_ref()
            && entries.size == size
            && entries.quality == quality
        {
            return;
        }
        *cache = Some(Entries {
            size,
            quality,
            slots: Vec::new(),
        });
    }

    pub(super) fn get(
        &self,
        gpu: &Gpu,
        size: [u32; 2],
        quality: Quality,
        index: usize,
        pool: &BudgetPool,
        cost: u64,
    ) -> Result<Arc<Allocation>, GpuError> {
        if let Some(entries) = self.0.lock().unwrap().as_mut()
            && entries.quality == quality
        {
            if let Some(target) = entries.slots.get(index)
                && target.target.size == size
            {
                return Ok(target.clone());
            }
            // Frame allocation order changed. Drop only cache ownership of the
            // obsolete tail; prepared/in-flight owners keep their reservations.
            entries.slots.truncate(index);
        }
        let resident = pool.reserve(cost).map_err(|_| GpuError::Pressure {
            domain: "targets",
            requested: cost,
            available: gpu.limits.target_bytes.saturating_sub(pool.usage().bytes),
        })?;
        Ok(Arc::new(Allocation {
            target: Target::new(&gpu.device, size, quality),
            _resident: resident,
        }))
    }

    pub(super) fn publish(&self, size: [u32; 2], quality: Quality, slots: &[Arc<Allocation>]) {
        let mut cache = self.0.lock().unwrap();
        if let Some(entries) = cache.as_mut()
            && entries.size == size
            && entries.quality == quality
        {
            // A simple frame reuses the prefix; subsequent effects can reuse
            // the remaining budgeted allocations without recreating textures.
            let shared = entries.slots.len().min(slots.len());
            entries.slots[..shared].clone_from_slice(&slots[..shared]);
            entries.slots.extend_from_slice(&slots[shared..]);
        }
    }

    #[cfg(test)]
    pub(super) fn clear(&self) {
        *self.0.lock().unwrap() = None;
    }
}
