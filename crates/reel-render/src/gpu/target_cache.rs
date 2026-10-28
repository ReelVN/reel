//! Queue-ordered scratch targets. Every use is cleared and fully consumed within
//! its command buffer, so subsequent submissions can reuse the same allocations.
use super::{Gpu, GpuError, Quality, targets::Target};
use reel_core::budget::{BudgetPool, BudgetTicket};
use std::sync::{Arc, Mutex};

pub(super) struct Allocation {
    pub(super) target: Target,
    pub(super) effect_pins: std::sync::atomic::AtomicUsize,
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
                .get(
                    &gpu,
                    size,
                    quality,
                    0,
                    &budgets.targets,
                    cost,
                    &crate::common::target_plan::Requirements::default(),
                )
                .unwrap();
            if let Some(first) = &first[bucket] {
                assert!(
                    Arc::ptr_eq(first, &allocation),
                    "one-pixel bucket crossings must stay warm"
                );
            } else {
                first[bucket] = Some(allocation.clone());
            }
            let used = [allocation];
            cache.reserve_metadata(
                [960, 544],
                quality,
                &used,
                &budgets.transient,
                limits.transient_bytes,
            );
            cache.publish([960, 544], quality, &used);
            let expected = if frame == 0 {
                32 * 32 * 4
            } else {
                (32 * 32 + 64 * 32) * 4
            };
            assert_eq!(budgets.targets.usage().bytes, expected);
            assert_eq!(budgets.targets.usage().peak_bytes, expected);
        }
        drop(first[1].take());
        for _ in 0..32 {
            cache.publish([960, 544], quality, &[first[0].as_ref().unwrap().clone()]);
        }
        assert_eq!(budgets.targets.usage().bytes, 32 * 32 * 4);
        let same = cache
            .get(
                &gpu,
                [32, 32],
                quality,
                0,
                &budgets.targets,
                32 * 32 * 4,
                &crate::common::target_plan::Requirements::default(),
            )
            .unwrap();
        assert!(Arc::ptr_eq(first[0].as_ref().unwrap(), &same));
        drop(same);
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

    #[test]
    #[ignore = "requires actual graphics adapter; allocates targets without drawing or readback"]
    fn effect_pin_prevents_scratch_reuse_until_last_guard_retires() {
        let limits = super::super::GpuLimits::default();
        let gpu = pollster::block_on(Gpu::request(
            &wgpu::Instance::default(),
            None,
            super::super::DeviceGeneration(1),
            limits,
        ))
        .unwrap();
        let quality = Quality {
            format: wgpu::TextureFormat::Rgba8Unorm,
            samples: 1,
            degraded: false,
        };
        let budgets = Budgets::new(limits);
        let cache = TargetCache::default();
        cache.configure([64, 64], quality);
        let size = [32, 32];
        let cost = super::super::targets::target_bytes(size, quality).unwrap();
        let requirements = crate::common::target_plan::Requirements::default();
        let first = cache
            .get(
                &gpu,
                size,
                quality,
                0,
                &budgets.targets,
                cost,
                &requirements,
            )
            .unwrap();
        cache.reserve_metadata(
            [64, 64],
            quality,
            std::slice::from_ref(&first),
            &budgets.transient,
            limits.transient_bytes,
        );
        cache.publish([64, 64], quality, std::slice::from_ref(&first));
        let pin = first
            .target
            .pin_effect(
                crate::common::effect_result::Token(1, 1),
                &budgets.transient,
            )
            .unwrap();
        let retained = pin.clone();
        assert!(first.target.is_effect_pinned());
        let second = cache
            .get(
                &gpu,
                size,
                quality,
                0,
                &budgets.targets,
                cost,
                &requirements,
            )
            .unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
        drop(pin);
        assert!(first.target.is_effect_pinned());
        drop(retained);
        assert!(!first.target.is_effect_pinned());
        let again = cache
            .get(
                &gpu,
                size,
                quality,
                0,
                &budgets.targets,
                cost,
                &requirements,
            )
            .unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        cache.clear();
        drop((first, second, again));
        assert_eq!(budgets.targets.usage().bytes, 0);
        assert_eq!(budgets.transient.usage().bytes, 0);
    }

    #[test]
    #[ignore = "native target allocation measurement; no drawing or readback"]
    fn continuous_bounds_capacity_cost() {
        use crate::common::{target_plan::Requirements, target_storage};
        let limits = super::super::GpuLimits::default();
        let gpu = pollster::block_on(Gpu::request(
            &wgpu::Instance::default(),
            None,
            super::super::DeviceGeneration(1),
            limits,
        ))
        .unwrap();
        let quality = Quality {
            format: wgpu::TextureFormat::Rgba8Unorm,
            samples: 1,
            degraded: false,
        };
        let root = [960, 544];
        for bucketed in [false, true] {
            let budgets = Budgets::new(limits);
            let cache = TargetCache::default();
            cache.configure(root, quality);
            let mut previous = std::sync::Weak::new();
            let mut allocations = 0;
            let began = std::time::Instant::now();
            for frame in 0..256 {
                let logical = [481 + frame % 32, 384];
                let exact = Requirements::collect([(logical, 1)].into_iter(), |_| Ok(())).unwrap();
                let plan = if bucketed {
                    target_storage::Plan::capacities(
                        &exact,
                        root,
                        4096,
                        limits.target_bytes,
                        &|size| super::super::targets::target_bytes(size, quality),
                        |_| Ok(()),
                    )
                    .unwrap()
                } else {
                    None
                };
                let (size, required) = plan.as_ref().map_or((logical, &exact), |(plan, _)| {
                    (plan.storage_size(logical), plan.requirements())
                });
                let bytes = super::super::targets::target_bytes(size, quality).unwrap();
                let allocation = cache
                    .get(&gpu, size, quality, 0, &budgets.targets, bytes, required)
                    .unwrap();
                allocations += usize::from(
                    !previous
                        .upgrade()
                        .is_some_and(|old| Arc::ptr_eq(&old, &allocation)),
                );
                previous = Arc::downgrade(&allocation);
                let used = [allocation];
                cache.reserve_metadata(
                    root,
                    quality,
                    &used,
                    &budgets.transient,
                    limits.transient_bytes,
                );
                cache.publish(root, quality, &used);
            }
            println!(
                "target_capacity bucketed={bucketed} frames=256 allocations={allocations} elapsed_us={} peak_bytes={}",
                began.elapsed().as_micros(),
                budgets.targets.usage().peak_bytes
            );
            assert_eq!(allocations, if bucketed { 1 } else { 256 });
            cache.clear();
            assert_eq!(budgets.targets.usage().bytes, 0);
        }
        gpu.check().unwrap();
    }
}
struct Entries {
    size: [u32; 2],
    quality: Quality,
    slots: crate::common::target_cache::Cache<Arc<Allocation>>,
}
#[derive(Default)]
pub(super) struct TargetCache(
    Mutex<Option<Entries>>,
    pub(super) Mutex<crate::common::scopes::Cache>,
);

impl TargetCache {
    #[cfg(test)]
    pub(super) fn metadata_bytes(&self) -> u64 {
        self.0
            .lock()
            .unwrap()
            .as_ref()
            .map_or(0, |entries| entries.slots.capacity_bytes())
            + self.1.lock().unwrap().metadata_bytes()
    }
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
            slots: Default::default(),
        });
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one physical request shares its quality, lifetime demand and accounting"
    )]
    pub(super) fn get(
        &self,
        gpu: &Gpu,
        size: [u32; 2],
        quality: Quality,
        index: usize,
        pool: &BudgetPool,
        cost: u64,
        required: &crate::common::target_plan::Requirements,
    ) -> Result<Arc<Allocation>, GpuError> {
        if let Some(entries) = self.0.lock().unwrap().as_ref()
            && entries.quality == quality
            && let Some(target) = entries
                .slots
                .get_eligible(size, index, |target| !target.target.is_effect_pinned())
        {
            reel_core::profile::count(reel_core::profile::Phase::TargetReused);
            return Ok(target.clone());
        }
        let mut resident = pool.reserve(cost);
        for recent in [false, true] {
            while resident.is_err() {
                let released = self
                    .0
                    .lock()
                    .unwrap()
                    .as_mut()
                    .filter(|entries| entries.quality == quality)
                    .is_some_and(|entries| {
                        entries.slots.reclaim_eligible(
                            required,
                            recent,
                            |owner| !owner.target.is_effect_pinned(),
                            |owner| Arc::strong_count(owner) == 1,
                        )
                    });
                if released {
                    resident = pool.reserve(cost);
                } else {
                    break;
                }
            }
        }
        let resident = resident.map_err(|_| GpuError::Pressure {
            domain: "targets",
            requested: cost,
            available: gpu.limits.target_bytes.saturating_sub(pool.usage().bytes),
        })?;
        Ok(Arc::new_cyclic(|owner| {
            let mut target = Target::new(&gpu.device, size, quality);
            target.allocation = Some(owner.clone());
            Allocation {
                target,
                effect_pins: std::sync::atomic::AtomicUsize::new(0),
                _resident: resident,
            }
        }))
    }

    pub(super) fn reserve_metadata(
        &self,
        size: [u32; 2],
        quality: Quality,
        slots: &[Arc<Allocation>],
        metadata: &BudgetPool,
        limit: u64,
    ) {
        let mut cache = self.0.lock().unwrap();
        if let Some(entries) = cache.as_mut()
            && entries.size == size
            && entries.quality == quality
        {
            let additional = slots
                .iter()
                .filter(|allocation| allocation.target.texture.format() == quality.format)
                .filter(|allocation| {
                    !entries
                        .slots
                        .contains(allocation.target.storage_size(), |cached| {
                            Arc::ptr_eq(cached, allocation)
                        })
                })
                .count();
            // Reuse is optional. Metadata pressure cannot reject an otherwise
            // fully prepared submission; publish keeps only admitted slots.
            let _ = entries.slots.reserve(additional, metadata, limit);
        }
    }

    pub(super) fn publish(&self, size: [u32; 2], quality: Quality, slots: &[Arc<Allocation>]) {
        let mut cache = self.0.lock().unwrap();
        if let Some(entries) = cache.as_mut()
            && entries.size == size
            && entries.quality == quality
        {
            entries.slots.submitted();
            for allocation in slots
                .iter()
                .filter(|allocation| allocation.target.texture.format() == quality.format)
            {
                let size = allocation.target.storage_size();
                if !entries
                    .slots
                    .contains(size, |cached| Arc::ptr_eq(cached, allocation))
                {
                    entries.slots.insert(size, allocation.clone());
                }
            }
            for allocation in slots
                .iter()
                .filter(|allocation| allocation.target.texture.format() == quality.format)
            {
                entries
                    .slots
                    .mark_used(allocation.target.storage_size(), |cached| {
                        Arc::ptr_eq(cached, allocation)
                    });
            }
            entries
                .slots
                .prune_cold(|owner| Arc::strong_count(owner) == 1);
        }
    }

    pub(super) fn prune_cold(&self) {
        if let Some(entries) = self.0.lock().unwrap().as_mut() {
            entries
                .slots
                .prune_cold(|owner| Arc::strong_count(owner) == 1);
        }
    }

    pub(super) fn release_idle(&self) {
        self.1.lock().unwrap().disable();
        if let Some(entries) = self.0.lock().unwrap().as_mut() {
            entries
                .slots
                .release_idle(|owner| Arc::strong_count(owner) == 1);
        }
    }

    pub(super) fn clear(&self) {
        self.1.lock().unwrap().clear();
        *self.0.lock().unwrap() = None;
    }
}
