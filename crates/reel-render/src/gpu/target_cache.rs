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
    #[ignore = "requires actual graphics adapter; validates allocation ownership"]
    fn retained_output_excludes_only_its_own_storage_from_scratch_reuse() {
        let gpu = pollster::block_on(Gpu::request(
            &wgpu::Instance::default(),
            None,
            super::super::DeviceGeneration(1),
            Default::default(),
        ))
        .unwrap();
        let quality = Quality {
            format: wgpu::TextureFormat::Rgba8Unorm,
            samples: 1,
            degraded: false,
        };
        let budgets = Budgets::new(gpu.limits);
        let cache = TargetCache::default();
        cache.configure([64, 64], quality);
        let get = |size| {
            cache
                .get(
                    &gpu,
                    size,
                    quality,
                    &[],
                    &budgets.targets,
                    super::super::targets::target_bytes(size, quality).unwrap(),
                    &crate::common::target_plan::Requirements::default(),
                )
                .unwrap()
        };
        let output = get([32, 32]);
        let scratch = get([16, 16]);
        let submitted = [output.clone(), scratch.clone()];
        cache.reserve_metadata(
            [64, 64],
            quality,
            &submitted,
            &budgets.transient,
            gpu.limits.transient_bytes,
        );
        cache.publish([64, 64], quality, std::slice::from_ref(&output), None);
        cache.publish([64, 64], quality, &submitted, Some(&output));
        let next_output = get([32, 32]);
        let reused = get([16, 16]);
        assert!(
            !Arc::ptr_eq(&output, &next_output),
            "retained pixels must stay immutable"
        );
        assert!(
            Arc::ptr_eq(&scratch, &reused),
            "the other scratch allocation remains reusable"
        );
        assert_eq!(budgets.targets.usage().bytes, (2 * 32 * 32 + 16 * 16) * 4);
        cache.clear();
        drop((output, scratch, submitted, next_output, reused));
        assert_eq!(budgets.targets.usage().bytes, 0);
        assert_eq!(budgets.transient.usage().bytes, 0);
    }

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
                    &[],
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
            cache.publish([960, 544], quality, &used, None);
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
            cache.publish(
                [960, 544],
                quality,
                &[first[0].as_ref().unwrap().clone()],
                None,
            );
        }
        assert_eq!(budgets.targets.usage().bytes, (32 * 32 + 64 * 32) * 4);
        cache.release_idle();
        assert_eq!(budgets.targets.usage().bytes, 32 * 32 * 4);
        let same = cache
            .get(
                &gpu,
                [32, 32],
                quality,
                &[],
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
                &[],
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
        cache.publish([64, 64], quality, std::slice::from_ref(&first), None);
        let token = crate::common::effect_result::Token(1, 1);
        let mut content = first.target.clone().with_effect_content(token);
        assert_eq!(content.effect_token(), Some(token));
        assert!(!content.is_effect_pinned());
        let scratch = cache
            .get(
                &gpu,
                size,
                quality,
                &[],
                &budgets.targets,
                cost,
                &requirements,
            )
            .unwrap();
        assert!(
            Arc::ptr_eq(&first, &scratch),
            "content identity does not reserve pixels"
        );
        assert_eq!(
            scratch.target.effect_token(),
            None,
            "a new logical view starts unlabelled"
        );
        drop(scratch);
        let pin = content.pin_effect(token, &budgets.transient).unwrap();
        let retained = pin.clone();
        content.clear_effect_content();
        assert_eq!(content.effect_token(), None);
        assert_eq!(retained.effect_token(), Some(token));
        assert!(first.target.is_effect_pinned());
        let second = cache
            .get(
                &gpu,
                size,
                quality,
                &[],
                &budgets.targets,
                cost,
                &requirements,
            )
            .unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
        let equal_content = second.target.clone().with_effect_content(token);
        assert_eq!(equal_content.effect_token(), pin.effect_token());
        assert!(
            !equal_content.same_allocation(&pin),
            "equal pixels can occupy different allocations"
        );
        assert!(!equal_content.is_effect_pinned());
        drop((content, equal_content));
        drop(pin);
        assert!(first.target.is_effect_pinned());
        drop(retained);
        assert!(!first.target.is_effect_pinned());
        // Releasing an earlier pin adds it back to the eligible set. Selection
        // must still exclude a target already owned by the current candidate.
        let unused = cache
            .get(
                &gpu,
                size,
                quality,
                std::slice::from_ref(&first),
                &budgets.targets,
                cost,
                &requirements,
            )
            .unwrap();
        assert!(!Arc::ptr_eq(&first, &unused));
        drop(unused);
        let again = cache
            .get(
                &gpu,
                size,
                quality,
                &[],
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
        for mode in ["exact", "padded", "resident"] {
            let budgets = Budgets::new(limits);
            let cache = TargetCache::default();
            cache.configure(root, quality);
            let mut previous = std::sync::Weak::new();
            let mut allocations = 0;
            if mode == "resident" {
                let size = [512, 400];
                let required = Requirements::collect([(size, 1)].into_iter(), |_| Ok(())).unwrap();
                let bytes = super::super::targets::target_bytes(size, quality).unwrap();
                let owner = cache
                    .get(&gpu, size, quality, &[], &budgets.targets, bytes, &required)
                    .unwrap();
                previous = Arc::downgrade(&owner);
                allocations = 1;
                let used = [owner];
                cache.reserve_metadata(
                    root,
                    quality,
                    &used,
                    &budgets.transient,
                    limits.transient_bytes,
                );
                cache.publish(root, quality, &used, None);
            }
            let began = std::time::Instant::now();
            for frame in 0..256 {
                let logical = [481 + frame % 32, 384];
                let exact = Requirements::collect([(logical, 1)].into_iter(), |_| Ok(())).unwrap();
                let plan = if mode != "exact" {
                    target_storage::Plan::capacities(
                        &exact,
                        root,
                        4096,
                        limits.target_bytes,
                        &|size| super::super::targets::target_bytes(size, quality),
                        &cache.capacities(root, quality, |_| Ok(())).unwrap(),
                        |_| Ok(()),
                    )
                    .unwrap()
                } else {
                    None
                };
                let (size, required) = plan.as_ref().map_or((logical, &exact), |(plan, _)| {
                    (plan.storage_size(logical), plan.requirements())
                });
                if mode == "resident" {
                    assert_eq!(size, [512, 400]);
                }
                let bytes = super::super::targets::target_bytes(size, quality).unwrap();
                let allocation = cache
                    .get(&gpu, size, quality, &[], &budgets.targets, bytes, required)
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
                cache.publish(root, quality, &used, None);
            }
            println!(
                "target_capacity mode={mode} frames=256 allocations={allocations} elapsed_us={} peak_bytes={}",
                began.elapsed().as_micros(),
                budgets.targets.usage().peak_bytes
            );
            assert_eq!(allocations, if mode == "exact" { 256 } else { 1 });
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
    pub(super) fn capacities(
        &self,
        size: [u32; 2],
        quality: Quality,
        reserve: impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<crate::common::target_plan::Requirements, GpuError> {
        let cache = self.0.lock().unwrap();
        let Some(entries) = cache
            .as_ref()
            .filter(|entries| entries.size == size && entries.quality == quality)
        else {
            return Ok(Default::default());
        };
        entries
            .slots
            .capacities(|owner| !owner.target.is_effect_pinned(), reserve)
    }
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
        used: &[Arc<Allocation>],
        pool: &BudgetPool,
        cost: u64,
        required: &crate::common::target_plan::Requirements,
    ) -> Result<Arc<Allocation>, GpuError> {
        if let Some(entries) = self.0.lock().unwrap().as_ref()
            && entries.quality == quality
            && let Some(target) = entries.slots.get_eligible(size, 0, |target| {
                !target.target.is_effect_pinned()
                    && !used.iter().any(|used| Arc::ptr_eq(used, target))
            })
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
        let resident = resident.map_err(|_| {
            #[cfg(feature = "profiling")]
            crate::profiling::record_effect_input_event(
                crate::profiling::EffectInputEvent::TargetPoolPressure,
                1,
            );
            GpuError::Pressure {
                domain: "targets",
                requested: cost,
                available: gpu.limits.target_bytes.saturating_sub(pool.usage().bytes),
            }
        })?;
        #[cfg(feature = "profiling")]
        crate::profiling::record_effect_input_event(
            crate::profiling::EffectInputEvent::TargetCreations,
            1,
        );
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

    pub(super) fn publish(
        &self,
        size: [u32; 2],
        quality: Quality,
        slots: &[Arc<Allocation>],
        retained: Option<&Arc<Allocation>>,
    ) {
        let mut cache = self.0.lock().unwrap();
        if let Some(entries) = cache.as_mut()
            && entries.size == size
            && entries.quality == quality
        {
            if let Some(retained) = retained {
                // Only the retained output stays readable after this submission.
                // Other scratch uses are ordered before the next queue submission.
                entries
                    .slots
                    .exclude(|cached| Arc::ptr_eq(cached, retained));
            }
            let reusable = |allocation: &&Arc<Allocation>| {
                allocation.target.texture.format() == quality.format
                    && !retained.is_some_and(|retained| Arc::ptr_eq(allocation, retained))
            };
            entries.slots.submitted();
            for allocation in slots.iter().filter(reusable) {
                let size = allocation.target.storage_size();
                if !entries
                    .slots
                    .contains(size, |cached| Arc::ptr_eq(cached, allocation))
                {
                    entries.slots.insert(size, allocation.clone());
                }
            }
            for allocation in slots.iter().filter(reusable) {
                entries
                    .slots
                    .mark_used(allocation.target.storage_size(), |cached| {
                        Arc::ptr_eq(cached, allocation)
                    });
            }
        }
    }

    pub(super) fn release_idle(&self) {
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
