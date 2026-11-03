//! Acquire and recycle physical scratch targets under the current frame budget.
use super::*;

impl Context<'_> {
    pub(in crate::gpu) fn target(&mut self) -> Result<Target, GpuError> {
        self.target_for(false)
    }
    pub(in crate::gpu) fn coverage_target(&mut self) -> Result<Target, GpuError> {
        self.target_for(true)
    }
    fn target_for(&mut self, coverage: bool) -> Result<Target, GpuError> {
        self.flush_draws()?;
        let quality = if coverage {
            self.coverage_quality.expect("admitted coverage format")
        } else {
            self.quality
        };
        let storage_size = if coverage {
            self.size
        } else {
            self.offscreen.storage_size(self.size)
        };
        let mut target = if let Some(index) = self.available_targets.iter().rposition(|target| {
            target.storage_size() == storage_size && target.texture.format() == quality.format
        }) {
            self.available_targets.swap_remove(index)
        } else {
            let cost = target_bytes(storage_size, quality)?;
            let local = self
                .budget_scope
                .targets
                .reserve(cost)
                .map_err(|_| GpuError::Budget {
                    domain: "targets",
                    requested: cost,
                    available: self
                        .gpu
                        .limits
                        .target_bytes
                        .saturating_sub(self.budget_scope.targets.usage().bytes),
                })?;
            let cache = if coverage {
                self.coverage_target_cache
            } else {
                self.target_cache
            };
            let requirements = if coverage {
                self.offscreen.coverage_requirements()
            } else {
                self.offscreen.requirements()
            };
            let mut release_idle_bindings = true;
            let allocation = loop {
                match cache.get(
                    self.gpu,
                    storage_size,
                    quality,
                    &self.target_allocations,
                    &self.budgets.targets,
                    cost,
                    requirements,
                ) {
                    Err(error @ GpuError::Pressure { .. }) => {
                        if release_idle_bindings {
                            release_idle_bindings = false;
                            if self.effect_uniform_cache.release_target_bindings() != 0 {
                                continue;
                            }
                        }
                        if !self.effect_results.reclaim_output() {
                            return Err(error);
                        }
                        // Required scratch takes precedence over optional pixels.
                        // Existing readers retain their pins; content observations
                        // continue to identify unchanged downstream dependencies.
                        self.effect_admission = Default::default();
                    }
                    result => break result?,
                }
            };
            self.tickets.push(Reservation {
                _shared: None,
                _local: local,
            });
            let mut target = allocation.target.clone();
            target.slot = self.target_allocations.len();
            let mut allocations = std::mem::take(&mut self.target_allocations);
            let pushed = push_stack(&mut allocations, allocation, |bytes| self.transient(bytes));
            self.target_allocations = allocations;
            pushed?;
            target
        };
        target.size = self.size;
        target.source_crop = None;
        target.clear_effect_content();
        let mut writes = std::mem::take(&mut self.target_writes);
        writes.begin(target.slot, |bytes| self.transient(bytes))?;
        self.target_writes = writes;
        Ok(target)
    }
    pub(in crate::gpu) fn ensure_sampled(&mut self, source: &wgpu::TextureView) {
        if let Some(slot) = self
            .target_allocations
            .iter()
            .position(|a| a.target.view == *source)
            && self.target_writes.take(slot)
        {
            let target = &self.target_allocations[slot].target;
            let _pass = self.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("initialize empty source"),
                color_attachments: &[Some(target.attachment(true))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                multiview_mask: None,
                occlusion_query_set: None,
            });
        }
    }
    /// All reads have been encoded before recycling. Later passes can overwrite
    /// the texture, but its reservation remains live through queue completion.
    pub(super) fn recycle_target(&mut self, mut target: Target) -> Result<(), GpuError> {
        if target.is_effect_pinned() {
            return Ok(());
        }
        target.clear_effect_content();
        if self.available_targets.len() == self.available_targets.capacity() {
            let additional = self.available_targets.capacity().max(1);
            self.transient((additional * size_of::<Target>()) as u64)?;
            self.available_targets.reserve_exact(additional);
        }
        self.available_targets.push(target);
        Ok(())
    }
}
