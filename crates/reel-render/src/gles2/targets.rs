//! Queue-ordered scratch allocation and initialization for GLES replay.
use super::{GlesError, native, replay::Context, resources::Target};
use crate::common::renderer::Reservation;

impl Context<'_> {
    pub fn target(&mut self) -> Result<Target, GlesError> {
        self.flush_draws()?;
        let target = if let Some(index) = self
            .available_targets
            .iter()
            .rposition(|target| target.size == self.size)
        {
            self.available_targets.swap_remove(index)
        } else {
            let bytes = u64::from(self.size[0]) * u64::from(self.size[1]) * 4;
            let local =
                self.budget_scope
                    .targets
                    .reserve(bytes)
                    .map_err(|_| GlesError::Budget {
                        domain: "offscreen targets",
                        requested: bytes,
                        available: self
                            .limits
                            .target_bytes
                            .saturating_sub(self.budget_scope.targets.usage().bytes),
                    })?;
            // The previous frame's presentation draw precedes this clear on the
            // same GLES queue. Capture outputs use a separate allocation set.
            let index = self
                .targets
                .iter()
                .filter(|target| target.size == self.size)
                .count();
            let mut target = if let Some(target) = self
                .target_cache
                .iter()
                .filter(|target| target.size == self.size)
                .nth(index)
            {
                target.clone()
            } else {
                // Retain distinct size buckets across animation and allocation
                // order changes. Existing byte budgets bound the idle pool.
                if bytes
                    > self
                        .limits
                        .target_bytes
                        .saturating_sub(self.budgets.targets.usage().bytes)
                {
                    self.target_cache.retain(|target| {
                        self.targets.iter().any(|live| live.same_allocation(target))
                    });
                }
                let target = match Target::new(
                    self.gpu,
                    self.size,
                    &self.budgets.targets,
                    self.limits.target_bytes,
                ) {
                    Ok(target) => target,
                    Err((error, storage)) => {
                        self.tickets.push(Reservation {
                            _shared: storage,
                            _local: local,
                        });
                        return Err(error);
                    }
                };
                self.target_cache.push(target.clone());
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
        let mut writes = std::mem::take(&mut self.target_writes);
        writes.begin(target.slot, |bytes| self.transient(bytes))?;
        self.target_writes = writes;
        Ok(target)
    }
    pub(super) fn ensure_target(&mut self, target: &Target) -> Result<(), GlesError> {
        if self.target_writes.take(target.slot) {
            let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Clear);
            self.gpu
                .clear(native::Target::Framebuffer(&target.framebuffer), [0.; 4])?;
        }
        Ok(())
    }
    pub(super) fn ensure_sampled(&mut self, source: &native::Texture) -> Result<(), GlesError> {
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
    pub(super) fn recycle_target(&mut self, target: Target) -> Result<(), GlesError> {
        self.available_targets.push(target);
        Ok(())
    }
}
