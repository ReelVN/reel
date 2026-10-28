//! Queue-ordered scratch allocation and initialization for GXM replay.
use super::{GpuError, native, replay::Context, resources::Target};
use crate::common::renderer::Reservation;

impl Context<'_> {
    pub fn target(&mut self) -> Result<Target, GpuError> {
        self.flush_draws()?;
        let storage_size = self.offscreen.storage_size(self.size);
        let mut target = if let Some(index) = self
            .available_targets
            .iter()
            .rposition(|target| target.view.size() == storage_size)
        {
            self.available_targets.swap_remove(index)
        } else {
            let bytes = self.gpu.target_layout(storage_size)?.texture_bytes;
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
            let index = self
                .targets
                .iter()
                .filter(|target| target.view.size() == storage_size && !target.is_effect_pinned())
                .count();
            let mut target = if let Some(target) = self.target_cache.get(storage_size, index) {
                target
            } else {
                let mut allocation = Target::new(
                    self.gpu,
                    storage_size,
                    &self.budgets.targets,
                    self.limits.target_bytes,
                );
                // Only a refused reservation can be retried. An actual native
                // failure keeps its original owner/ticket retirement path.
                for recent in [false, true] {
                    while matches!(allocation, Err((GpuError::Pressure { .. }, None))) {
                        if self
                            .target_cache
                            .reclaim(self.offscreen.requirements(), recent)
                        {
                            allocation = Target::new(
                                self.gpu,
                                storage_size,
                                &self.budgets.targets,
                                self.limits.target_bytes,
                            );
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
                self.target_cache.insert(
                    &target,
                    &self.budgets.transient,
                    self.limits.transient_bytes,
                );
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
