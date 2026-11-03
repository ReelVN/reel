//! Queue-ordered scratch allocation and initialization for GXM replay.
use super::{GpuError, native, replay::Context, resources::Target};
use crate::common::renderer::Reservation;
#[path = "targets/warming.rs"]
mod warming;
pub(super) use warming::{Replacement, Replacements};

pub(super) fn cost(
    gpu: &super::ownership::Device,
    size: [u32; 2],
    coverage: bool,
) -> Result<u64, GpuError> {
    let format = if coverage {
        native::TargetFormat::Coverage8
    } else {
        native::TargetFormat::Rgba8
    };
    Ok(gpu.target_layout(size, format)?.texture_bytes)
}

impl Context<'_> {
    pub(super) fn cropped_target(
        &mut self,
        crop: crate::common::source_crop::Crop,
    ) -> Result<Target, GpuError> {
        let size = self.size;
        self.size = crop.size;
        let result = self.target();
        self.size = size;
        let mut target = result?;
        target.size = size;
        target.source_crop = Some(crop);
        #[cfg(feature = "profiling")]
        target.trace_event(crate::profiling::TraceKind::TargetAcquire, "cropped-view");
        Ok(target)
    }
    /// Acquire the selected physical scratch set before issuing its drawing work.
    /// Large surfaces go first so small blocks cannot split their available space.
    pub(super) fn prepare_targets(&mut self) -> Result<(), GpuError> {
        let count = self
            .planned_targets
            .as_ref()
            .unwrap_or_else(|| self.offscreen.requirements())
            .iter()
            .len()
            + self.offscreen.coverage_requirements().iter().len();
        self.transient((count * size_of::<(u64, [u32; 2], u64, bool)>()) as u64)?;
        let color = self
            .planned_targets
            .as_ref()
            .unwrap_or_else(|| self.offscreen.requirements());
        let mut requests = Vec::with_capacity(count);
        for (coverage, requirements) in [
            (false, color),
            (true, self.offscreen.coverage_requirements()),
        ] {
            for (size, count) in requirements.iter() {
                requests.push((cost(self.gpu, size, coverage)?, size, count, coverage));
            }
        }
        requests.sort_unstable_by_key(|request| std::cmp::Reverse(request.0));
        for (_, size, count, coverage) in requests {
            for _ in 0..count {
                let target = self.acquire_target(size, coverage)?;
                self.available_targets.push(target);
            }
        }
        Ok(())
    }
    pub fn target(&mut self) -> Result<Target, GpuError> {
        self.target_for(false)
    }
    pub fn coverage_target(&mut self) -> Result<Target, GpuError> {
        self.target_for(true)
    }
    /// Preserve the full prepared scratch set before extending an output's life.
    /// An optional cache result can borrow a spare resident owner; it cannot
    /// cause a new physical allocation after drawing has started.
    pub(super) fn effect_replacement(&mut self, output: &Target) -> Option<Replacement> {
        if let Some(replacement) = self.effect_replacements.take(output) {
            return Some(replacement);
        }
        if !self.can_retain_effect(output.view.allocated_bytes() as u64) {
            return None;
        }
        let size = output.view.size();
        let storage = output
            .view
            .target_storage()
            .expect("ordinary effect target");
        let replacement = self.target_cache.get(size, storage, &self.targets)?;
        let bytes = replacement.view.allocated_bytes() as u64;
        let local = self.budget_scope.targets.reserve(bytes).ok()?;
        Some(Replacement {
            target: replacement,
            ticket: Reservation {
                _shared: None,
                _local: local,
            },
            cache_new: false,
        })
    }
    pub(super) fn clip_target(
        &mut self,
        group: Option<&reel_ui::frame::FrameGroup>,
        delayed: bool,
    ) -> Result<Target, GpuError> {
        if self.offscreen.coverage_enabled()
            && crate::common::target_plan::single_clip(group, delayed)
        {
            self.coverage_target()
        } else {
            self.target()
        }
    }
    fn target_for(&mut self, coverage: bool) -> Result<Target, GpuError> {
        self.flush_draws()?;
        let texture_format = if coverage {
            native::TextureFormat::Coverage8
        } else {
            native::TextureFormat::Rgba8
        };
        let storage_size = if coverage {
            self.size
        } else {
            self.offscreen.storage_size(self.size)
        };
        let storage = self
            .target_storage
            .layout(
                storage_size,
                if coverage {
                    native::TargetFormat::Coverage8
                } else {
                    native::TargetFormat::Rgba8
                },
            )?
            .storage;
        let mut target = if let Some(index) = self.available_targets.iter().rposition(|target| {
            target.view.size() == storage_size
                && target.view.format() == texture_format
                && target.view.target_storage() == Some(storage)
        }) {
            self.available_targets.swap_remove(index)
        } else {
            reel_core::profile::count(reel_core::profile::Phase::TargetUnplanned);
            self.acquire_target(storage_size, coverage)?
        };
        target.size = self.size;
        target.source_crop = None;
        target.clear_effect_content();
        target.framebuffer = target.framebuffer.with_write_region(None)?;
        target.effect_cache_allowed = true;
        #[cfg(feature = "profiling")]
        {
            target.trace_write_region(None);
            target.trace_begin_use();
        }
        self.targets[target.slot] = target.clone();
        let mut writes = std::mem::take(&mut self.target_writes);
        writes.begin(target.slot, |bytes| self.transient(bytes))?;
        self.target_writes = writes;
        Ok(target)
    }
    fn acquire_target(
        &mut self,
        storage_size: [u32; 2],
        coverage: bool,
    ) -> Result<Target, GpuError> {
        let format = if coverage {
            native::TargetFormat::Coverage8
        } else {
            native::TargetFormat::Rgba8
        };
        let layout = self.target_storage.layout(storage_size, format)?;
        let bytes = layout.texture_bytes;
        #[cfg(feature = "profiling")]
        let request = crate::profiling::TraceData {
            label: "scratch-request",
            status: crate::profiling::TraceStatus::Pending,
            storage: storage_size,
            bytes,
            count: 1,
            stride_bytes: layout.row_stride_bytes,
            target_format: if coverage { 2 } else { 1 },
            storage_kind: match layout.storage {
                native::TargetStorage::Linear => 1,
                native::TargetStorage::Tiled => 2,
            },
            known: crate::profiling::KNOWN_STORAGE
                | crate::profiling::KNOWN_STRIDE
                | crate::profiling::KNOWN_TARGET_FORMAT
                | crate::profiling::KNOWN_STORAGE_KIND,
            ..Default::default()
        };
        #[cfg(feature = "profiling")]
        let trace_acquire = crate::profiling::TracePass::begin("target-acquire", request);
        #[cfg(feature = "profiling")]
        crate::profiling::record_trace(crate::profiling::TraceKind::TargetAcquire, request);
        let local = self.budget_scope.targets.reserve(bytes).map_err(|_| {
            #[cfg(feature = "profiling")]
            {
                let mut data = request;
                data.label = "scratch-budget-denied";
                data.status = crate::profiling::TraceStatus::Error;
                crate::profiling::record_trace(crate::profiling::TraceKind::TargetAcquire, data);
            }
            GpuError::Budget {
                domain: "offscreen targets",
                requested: bytes,
                available: self
                    .limits
                    .target_bytes
                    .saturating_sub(self.budget_scope.targets.usage().bytes),
            }
        })?;
        // The previous frame's presentation draw precedes this clear on the
        // same graphics queue. Capture outputs use a separate allocation set.
        let cache = if coverage {
            &mut self.coverage_target_cache
        } else {
            &mut self.target_cache
        };
        let requirements = if coverage {
            self.offscreen.coverage_requirements()
        } else {
            self.planned_targets
                .as_ref()
                .unwrap_or_else(|| self.offscreen.requirements())
        };
        let mut stage = false;
        let reusable = if let Some(target) = cache.get(storage_size, layout.storage, &self.targets)
        {
            Some(target)
        } else {
            match cache.reshape(
                self.gpu,
                storage_size,
                format,
                layout.storage,
                &self.targets,
                requirements,
            ) {
                Ok(target) => target,
                // A new scheduling view can need MAIN storage even when the
                // color allocation is reused. Use the existing reclaim path.
                Err(error) if error.is_memory_pressure() => None,
                Err(error) => return Err(error),
            }
        };
        let mut target = if let Some(target) = reusable {
            reel_core::profile::count(reel_core::profile::Phase::TargetReused);
            target
        } else {
            let mut recovered = false;
            let mut allocation = Target::new(
                self.gpu,
                storage_size,
                format,
                layout.storage,
                &self.budgets.targets,
                self.limits.target_bytes,
            );
            // Release optional owners before retrying capacity failures.
            // In-flight native owners retain their retirement path.
            for recent in [false, true] {
                while allocation
                    .as_ref()
                    .is_err_and(|(error, storage)| error.is_memory_pressure() && storage.is_none())
                {
                    // Actual physical pressure takes precedence over optional
                    // lifetimes admitted from aggregate free-byte estimates.
                    self.effect_admission = Default::default();
                    if cache.reclaim_for_target(requirements, self.effect_results, recent) {
                        // Dropping an optional effect pin can make a matching
                        // scratch allocation reusable without allocating again.
                        allocation = if let Some(target) =
                            cache.get(storage_size, layout.storage, &self.targets)
                        {
                            recovered = true;
                            reel_core::profile::count(reel_core::profile::Phase::TargetReused);
                            Ok(target)
                        } else {
                            Target::new(
                                self.gpu,
                                storage_size,
                                format,
                                layout.storage,
                                &self.budgets.targets,
                                self.limits.target_bytes,
                            )
                        };
                    } else {
                        break;
                    }
                }
            }
            let target = match allocation {
                Ok(target) => target,
                Err((error, storage)) => {
                    if let Some(storage) = storage {
                        self.failed_native.push(storage);
                    }
                    self.tickets.push(Reservation {
                        _shared: None,
                        _local: local,
                    });
                    return Err(error);
                }
            };
            if !recovered {
                if self.preparing_targets {
                    stage = true;
                } else {
                    cache.insert(
                        &target,
                        &self.budgets.transient,
                        self.limits.transient_bytes,
                    );
                }
            }
            target
        };
        if stage {
            let mut staged = std::mem::take(&mut self.staged_targets);
            let held = crate::common::scopes::push_stack(&mut staged, target.clone(), |bytes| {
                self.transient(bytes)
            });
            self.staged_targets = staged;
            held?;
        }
        self.tickets.push(Reservation {
            _shared: None,
            _local: local,
        });
        target.slot = self.targets.len();
        self.targets.push(target.clone());
        #[cfg(feature = "profiling")]
        {
            target.trace_event(
                crate::profiling::TraceKind::TargetAcquire,
                "scratch-acquired",
            );
            if let Some(trace) = trace_acquire {
                trace.finish(crate::profiling::TraceStatus::Ok, target.trace_data());
            }
        }
        Ok(target)
    }
    pub(super) fn ensure_target(&mut self, target: &Target) -> Result<(), GpuError> {
        self.initialize_target(target, None)
    }
    pub(super) fn initialize_target(
        &mut self,
        target: &Target,
        overwrite: Option<crate::common::target_writes::FullOverwrite>,
    ) -> Result<(), GpuError> {
        // A logical-cell overwrite leaves physical macrotile padding untouched.
        // Native clear initializes the complete cell before same-scene feedback.
        let overwrite = overwrite.filter(|_| target.macrotile_extent().is_none());
        #[cfg(feature = "profiling")]
        let first = self.target_writes.pending(target.slot);
        let clear = self.target_writes.write(target.slot, overwrite);
        #[cfg(feature = "profiling")]
        {
            let mut data = target.trace_data();
            data.label = if clear {
                "first-write-clear"
            } else if first && overwrite.is_some() {
                "proved-overwrite"
            } else {
                "already-initialized"
            };
            data.status = if clear {
                crate::profiling::TraceStatus::Pending
            } else if first && overwrite.is_some() {
                crate::profiling::TraceStatus::Ok
            } else {
                crate::profiling::TraceStatus::Skipped
            };
            data.count = 1;
            crate::profiling::record_trace(crate::profiling::TraceKind::TargetInit, data);
        }
        if clear {
            let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Clear);
            #[cfg(feature = "profiling")]
            let trace = crate::profiling::TracePass::begin("target-clear", target.trace_data());
            let result = self
                .gpu
                .clear(native::Target::Framebuffer(&target.framebuffer), [0.; 4]);
            #[cfg(feature = "profiling")]
            if let Some(trace) = trace {
                trace.finish(
                    if result.is_ok() {
                        crate::profiling::TraceStatus::Ok
                    } else {
                        crate::profiling::TraceStatus::Error
                    },
                    target.trace_data(),
                );
            }
            result?;
            #[cfg(feature = "profiling")]
            target.trace_event(
                crate::profiling::TraceKind::TargetInit,
                "first-write-cleared",
            );
        }
        Ok(())
    }
    pub(super) fn ensure_sampled(&mut self, source: &native::Texture) -> Result<(), GpuError> {
        if let Some(slot) = self
            .targets
            .iter()
            .position(|target| target.view == *source)
            && self.target_writes.take(slot)
        {
            let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Clear);
            #[cfg(feature = "profiling")]
            let trace = crate::profiling::TracePass::begin(
                "target-sample-clear",
                self.targets[slot].trace_data(),
            );
            let result = self.gpu.clear(
                native::Target::Framebuffer(&self.targets[slot].framebuffer),
                [0.; 4],
            );
            #[cfg(feature = "profiling")]
            if let Some(trace) = trace {
                trace.finish(
                    if result.is_ok() {
                        crate::profiling::TraceStatus::Ok
                    } else {
                        crate::profiling::TraceStatus::Error
                    },
                    self.targets[slot].trace_data(),
                );
            }
            result?;
            #[cfg(feature = "profiling")]
            self.targets[slot].trace_event(
                crate::profiling::TraceKind::TargetInit,
                "first-sample-cleared",
            );
        }
        Ok(())
    }
    pub(super) fn recycle_target(&mut self, mut target: Target) -> Result<(), GpuError> {
        if !target.is_effect_pinned() && target.macrotile_extent().is_none() {
            target.clear_effect_content();
            target.framebuffer = target
                .framebuffer
                .with_write_region(None)
                .map_err(|error| {
                    #[cfg(feature = "profiling")]
                    {
                        let mut data = target.trace_data();
                        data.label = "return-region-error";
                        data.status = crate::profiling::TraceStatus::Error;
                        crate::profiling::record_trace(
                            crate::profiling::TraceKind::TargetRecycle,
                            data,
                        );
                    }
                    error
                })?;
            target.effect_cache_allowed = true;
            #[cfg(feature = "profiling")]
            {
                target.trace_write_region(None);
                target.trace_event(
                    crate::profiling::TraceKind::TargetRecycle,
                    "frame-local-return",
                );
            }
            self.available_targets.push(target);
        } else {
            #[cfg(feature = "profiling")]
            {
                let mut data = target.trace_data();
                data.label = if target.is_effect_pinned() {
                    "return-effect-pinned"
                } else {
                    "return-macro-owner"
                };
                data.status = crate::profiling::TraceStatus::Skipped;
                crate::profiling::record_trace(crate::profiling::TraceKind::TargetRecycle, data);
            }
        }
        Ok(())
    }
}
