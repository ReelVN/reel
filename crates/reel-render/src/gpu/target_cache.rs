//! Queue-ordered scratch targets. Every use is cleared and fully consumed within
//! its command buffer, so subsequent submissions can reuse the same allocations.
use super::{Gpu, GpuError, Quality, targets::Target};
use reel_core::budget::{BudgetPool, BudgetTicket};
use std::sync::{Arc, Mutex};

pub(super) struct Allocation {
    pub(super) target: Target,
    _resident: BudgetTicket,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::renderer::Budgets;

    #[test]
    #[ignore = "requires actual graphics adapter; no drawing or readback"]
    fn animated_bucket_crossings_reuse_storage_and_keep_actual_reservations() {
        let limits = super::super::GpuLimits::default();
        let gpu = pollster::block_on(Gpu::request(
            &wgpu::Instance::default(),
            None,
            super::super::DeviceGeneration(1),
            limits,
        ))
        .expect("actual adapter required");
        let quality = Quality {
            format: wgpu::TextureFormat::Rgba8Unorm,
            samples: 1,
            degraded: false,
        };
        let budgets = Budgets::new(limits);
        let cache = TargetCache::default();
        cache.configure([960, 544], quality);
        let mut first: [Option<Arc<Allocation>>; 2] = [None, None];
        for frame in 0..64 {
            let bucket = frame % 2;
            let size = if bucket == 0 { [32, 32] } else { [64, 32] };
            let cost = super::super::targets::target_bytes(size, quality).unwrap();
            let allocation = cache
                .get(&gpu, size, quality, 0, &budgets.targets, cost)
                .unwrap();
            if let Some(first) = &first[bucket] {
                assert!(
                    Arc::ptr_eq(first, &allocation),
                    "one-pixel bucket crossings must stay warm"
                );
            } else {
                first[bucket] = Some(allocation.clone());
            }
            cache.publish([960, 544], quality, &[allocation]);
            let expected = if frame == 0 {
                32 * 32 * 4
            } else {
                (32 * 32 + 64 * 32) * 4
            };
            assert_eq!(budgets.targets.usage().bytes, expected);
            assert_eq!(budgets.targets.usage().peak_bytes, expected);
        }
        // Cache eviction cannot return a flight's live bytes. The last owner
        // releases them independently of the now-empty idle cache.
        let flight = first[0].take().unwrap();
        drop(first);
        cache.clear();
        assert_eq!(budgets.targets.usage().bytes, 32 * 32 * 4);
        drop(flight);
        assert_eq!(budgets.targets.usage().bytes, 0);
        gpu.check().unwrap();
    }
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
            && let Some(target) = entries
                .slots
                .iter()
                .filter(|allocation| allocation.target.size == size)
                .nth(index)
        {
            return Ok(target.clone());
        }
        // Different size buckets survive changes in allocation order. Under
        // pressure, release only cache ownership; flights retain actual storage.
        if cost > gpu.limits.target_bytes.saturating_sub(pool.usage().bytes)
            && let Some(entries) = self.0.lock().unwrap().as_mut()
        {
            entries.slots.clear();
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
            // Cache ownership includes every warm size bucket, with the same
            // resident tickets that are held by prepared/in-flight submissions.
            for allocation in slots {
                if !entries
                    .slots
                    .iter()
                    .any(|cached| Arc::ptr_eq(cached, allocation))
                {
                    entries.slots.push(allocation.clone());
                }
            }
        }
    }

    #[cfg(test)]
    pub(super) fn clear(&self) {
        *self.0.lock().unwrap() = None;
    }
}
