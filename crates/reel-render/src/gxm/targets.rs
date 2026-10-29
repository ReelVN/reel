//! Queue-ordered scratch allocation and initialization for GXM replay.
use super::{GpuError, native, replay::Context, resources::Target};
use crate::common::renderer::Reservation;

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
    pub fn target(&mut self) -> Result<Target, GpuError> {
        self.target_for(false)
    }
    pub fn coverage_target(&mut self) -> Result<Target, GpuError> {
        self.target_for(true)
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
        let format = if coverage {
            native::TargetFormat::Coverage8
        } else {
            native::TargetFormat::Rgba8
        };
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
        let mut target = if let Some(index) = self.available_targets.iter().rposition(|target| {
            target.view.size() == storage_size && target.view.format() == texture_format
        }) {
            self.available_targets.swap_remove(index)
        } else {
            let bytes = self.gpu.target_layout(storage_size, format)?.texture_bytes;
            let local = self
                .budget_scope
                .targets
                .reserve(bytes)
                .map_err(|_| GpuError::Budget {
                    domain: "offscreen targets",
                    requested: bytes,
                    available: self
                        .limits
                        .target_bytes
                        .saturating_sub(self.budget_scope.targets.usage().bytes),
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
                self.offscreen.requirements()
            };
            let mut target = if let Some(target) = cache.get(storage_size, &self.targets) {
                reel_core::profile::count(reel_core::profile::Phase::TargetReused);
                target
            } else {
                let mut recovered = false;
                let mut allocation = Target::new(
                    self.gpu,
                    storage_size,
                    format,
                    &self.budgets.targets,
                    self.limits.target_bytes,
                );
                // Release optional owners before retrying capacity failures.
                // In-flight native owners retain their retirement path.
                for recent in [false, true] {
                    while allocation.as_ref().is_err_and(|(error, storage)| {
                        error.is_memory_pressure() && storage.is_none()
                    }) {
                        if cache.reclaim(requirements, recent)
                            || self.effect_results.reclaim_output()
                        {
                            // Dropping an optional effect pin can make a matching
                            // scratch allocation reusable without allocating again.
                            allocation = if let Some(target) =
                                cache.get(storage_size, &self.targets)
                            {
                                recovered = true;
                                reel_core::profile::count(reel_core::profile::Phase::TargetReused);
                                Ok(target)
                            } else {
                                Target::new(
                                    self.gpu,
                                    storage_size,
                                    format,
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
                    cache.insert(
                        &target,
                        &self.budgets.transient,
                        self.limits.transient_bytes,
                    );
                }
                target
            };
            self.tickets.push(Reservation {
                _shared: None,
                _local: local,
            });
            target.slot = self.targets.len();
            self.targets.push(target.clone());
            target
        };
        target.size = self.size;
        let mut writes = std::mem::take(&mut self.target_writes);
        writes.begin(target.slot, |bytes| self.transient(bytes))?;
        self.target_writes = writes;
        Ok(target)
    }
    pub(super) fn ensure_target(&mut self, target: &Target) -> Result<(), GpuError> {
        if self.target_writes.take(target.slot) {
            let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Clear);
            self.gpu
                .clear(native::Target::Framebuffer(&target.framebuffer), [0.; 4])?;
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
            self.gpu.clear(
                native::Target::Framebuffer(&self.targets[slot].framebuffer),
                [0.; 4],
            )?;
        }
        Ok(())
    }
    pub(super) fn recycle_target(&mut self, target: Target) -> Result<(), GpuError> {
        if !target.is_effect_pinned() {
            self.available_targets.push(target);
        }
        Ok(())
    }
}
