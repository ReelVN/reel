//! Prepare one physical plan before replaying ordered macrotile cells.
use super::*;
use crate::common::target_plan::{
    self,
    macrotile::{Candidate, Key, OWNER_MAX_CELLS, Selection},
};
use smallvec::SmallVec;

pub(super) struct Source {
    plan: Candidate,
    targets: SmallVec<[Target; 4]>,
    restart: bool,
}

fn optional_pressure(error: &GpuError) -> bool {
    error.is_memory_pressure()
        || matches!(
            error,
            GpuError::Budget {
                domain: "transient" | "offscreen targets",
                ..
            }
        )
}

impl Context<'_> {
    /// Choose and prepare all owners before the first draw. New cache entries
    /// become visible only after the complete chosen set has been acquired.
    pub(super) fn prepare_target_plan(
        &mut self,
        frame: &RenderFrame,
        ordinary_bytes: u64,
    ) -> Result<u64, GpuError> {
        let checkpoint = self.tickets.len();
        let mut metadata = Vec::new();
        let enabled = !self.capture && self.target_storage == native::TargetStorage::Linear;
        #[cfg(feature = "profiling")]
        let enabled = enabled && !crate::profiling::serial_effect_timing();
        let selection = if enabled {
            let color = |size| super::super::targets::cost(self.gpu, size, false);
            let coverage = |size| super::super::targets::cost(self.gpu, size, true);
            let input = target_plan::Input {
                frame,
                second: None,
                size: self.size,
                capture: self.capture,
                samples: 1,
                max_dimension: self.gpu.capabilities().max_texture_size,
                glyphs: &self.glyph_plan,
                schedule: &self.offscreen,
                masks: target_plan::MaskExecution::Sampled,
                selection: &self.selection,
                cost: &color,
                coverage_cost: self.offscreen.coverage_enabled().then_some(&coverage),
            };
            let result = input.macrotile_selection(self.limits.target_bytes, |bytes| {
                metadata.push(reserve(
                    &self.budgets.transient,
                    &self.budget_scope.transient,
                    bytes,
                    self.limits.transient_bytes,
                    "transient",
                )?);
                Ok(())
            });
            match result {
                Ok(selection) => selection,
                Err(error) if optional_pressure(&error) => None,
                Err(error) => return Err(error),
            }
        } else {
            None
        };
        self.preparing_targets = true;
        let bytes = if let Some(selection) = selection {
            let mut metadata = Some(metadata);
            let bytes = target_plan::macrotile::prepare_or_ordinary(
                self,
                optional_pressure,
                |context| context.prepare_macro_targets(selection),
                |context| {
                    context.rollback_target_plan(checkpoint)?;
                    drop(metadata.take());
                    Ok(())
                },
                |context| context.prepare_targets().map(|()| ordinary_bytes),
            )?;
            if let Some(metadata) = metadata {
                self.tickets.extend(metadata);
            }
            bytes
        } else {
            drop(metadata);
            self.prepare_targets()?;
            ordinary_bytes
        };
        for target in std::mem::take(&mut self.staged_targets) {
            let cache = if target.view.format() == native::TextureFormat::Coverage8 {
                &mut self.coverage_target_cache
            } else {
                &mut self.target_cache
            };
            cache.insert(
                &target,
                &self.budgets.transient,
                self.limits.transient_bytes,
            );
        }
        self.preparing_targets = false;
        Ok(bytes)
    }

    fn rollback_target_plan(&mut self, checkpoint: usize) -> Result<(), GpuError> {
        drop(std::mem::take(&mut self.macrotile_sources));
        drop(std::mem::take(&mut self.macrotile_outputs));
        drop(std::mem::take(&mut self.staged_targets));
        drop(std::mem::take(&mut self.available_targets));
        drop(std::mem::take(&mut self.targets));
        self.target_writes = Default::default();
        self.planned_targets = None;
        self.tickets.truncate(checkpoint);
        self.gpu.check()
    }

    fn prepare_macro_targets(&mut self, selection: Selection) -> Result<u64, GpuError> {
        let Selection {
            entries,
            requirements,
            bytes,
        } = selection;
        self.planned_targets = Some(requirements);
        let mut sources: Vec<Source> = Vec::new();
        let mut owners: Vec<(super::super::target_cache::Macrotile, bool)> = Vec::new();
        // Claim the complete resident set before a missing owner can cause
        // pressure reclamation. Ordinary requirements cannot name whole macro
        // allocations; these leases protect later producers by actual ownership.
        let mut resident: Vec<Option<super::super::target_cache::Macrotile>> = Vec::new();
        for plan in &entries {
            if plan.owner != resident.len() {
                continue;
            }
            let layout =
                native::MacrotileLayout::new(plan.cell, plan.grid).map_err(native_error)?;
            let sizes = [plan.storage; OWNER_MAX_CELLS];
            let sizes = &sizes[..plan.owner_cells];
            if plan.owner_cells > 4 {
                self.transient(
                    (plan.owner_cells.next_power_of_two() * size_of::<Target>()) as u64,
                )?;
            }
            let target = self.target_cache.macrotile(layout.size(), sizes, |target| {
                resident
                    .iter()
                    .flatten()
                    .any(|set| set.cells[0].same_allocation(target))
            });
            push_stack(&mut resident, target, |bytes| self.transient(bytes))?;
        }
        for plan in entries {
            let restart = plan.first_cell == 0 && plan.owner < owners.len();
            if plan.owner == owners.len() {
                let layout =
                    native::MacrotileLayout::new(plan.cell, plan.grid).map_err(native_error)?;
                let sizes = [plan.storage; OWNER_MAX_CELLS];
                let sizes = &sizes[..plan.owner_cells];
                let footprint = self
                    .gpu
                    .macrotile_target_footprint(layout, sizes)
                    .map_err(native_error)?;
                let local = self
                    .budget_scope
                    .targets
                    .reserve(footprint.texture.texture_bytes)
                    .map_err(|_| GpuError::Pressure {
                        domain: "offscreen targets",
                        requested: footprint.texture.texture_bytes,
                        available: self
                            .limits
                            .target_bytes
                            .saturating_sub(self.budget_scope.targets.usage().bytes),
                    })?;
                let (mut targets, cache_new) = if let Some(set) = resident[plan.owner].take() {
                    reel_core::profile::count(reel_core::profile::Phase::TargetReused);
                    (set, false)
                } else {
                    let mut recovered = false;
                    let mut allocation = Target::macrotile(
                        self.gpu,
                        layout,
                        sizes,
                        &self.budgets.targets,
                        self.limits.target_bytes,
                    );
                    for recent in [false, true] {
                        while allocation.as_ref().is_err_and(|(error, ticket)| {
                            error.is_memory_pressure() && ticket.is_none()
                        }) {
                            if !self.target_cache.reclaim_for_target(
                                self.planned_targets.as_ref().unwrap(),
                                self.effect_results,
                                recent,
                            ) {
                                break;
                            }
                            allocation = if let Some(set) =
                                self.target_cache.macrotile(layout.size(), sizes, |target| {
                                    sources
                                        .iter()
                                        .any(|source| source.targets[0].same_allocation(target))
                                        || resident
                                            .iter()
                                            .flatten()
                                            .any(|set| set.cells[0].same_allocation(target))
                                }) {
                                recovered = true;
                                reel_core::profile::count(reel_core::profile::Phase::TargetReused);
                                Ok(set.cells)
                            } else {
                                Target::macrotile(
                                    self.gpu,
                                    layout,
                                    sizes,
                                    &self.budgets.targets,
                                    self.limits.target_bytes,
                                )
                            };
                        }
                    }
                    let cells = match allocation {
                        Ok(cells) => cells,
                        Err((error, ticket)) => {
                            self.failed_native.extend(ticket);
                            return Err(error);
                        }
                    };
                    (
                        super::super::target_cache::Macrotile::new(cells),
                        !recovered,
                    )
                };
                self.tickets.push(Reservation {
                    _shared: None,
                    _local: local,
                });
                for target in &mut targets.cells {
                    target.slot = self.targets.len();
                    let mut writes = std::mem::take(&mut self.target_writes);
                    let begun = writes.begin(target.slot, |bytes| self.transient(bytes));
                    self.target_writes = writes;
                    begun?;
                    let mut held = std::mem::take(&mut self.targets);
                    let stored =
                        push_stack(&mut held, target.clone(), |bytes| self.transient(bytes));
                    self.targets = held;
                    stored?;
                }
                push_stack(&mut owners, (targets, cache_new), |bytes| {
                    self.transient(bytes)
                })?;
            }
            let mut targets: SmallVec<[Target; 4]> = owners[plan.owner].0.cells
                [plan.first_cell..plan.first_cell + plan.count + 1]
                .iter()
                .cloned()
                .collect();
            for target in &mut targets {
                target.size = plan.logical;
                target.effect_cache_allowed = plan.last_use;
            }
            push_stack(
                &mut sources,
                Source {
                    plan,
                    targets,
                    restart,
                },
                |bytes| self.transient(bytes),
            )?;
        }
        self.prepare_targets()?;
        // Cache insertion cannot fail the prepared work. Until here, no new
        // atlas is retained outside the unsubmitted preparation and its tickets.
        for (owner, cache_new) in &owners {
            if *cache_new {
                self.target_cache.insert_macrotile(
                    owner,
                    &self.budgets.transient,
                    self.limits.transient_bytes,
                );
            }
        }
        self.macrotile_sources = sources;
        Ok(bytes)
    }

    pub(in crate::gxm) fn macrotile_output(
        &mut self,
        effect: &reel_ui::frame::FrameEffect,
    ) -> Option<Target> {
        let identity = std::ptr::from_ref(effect) as usize;
        self.macrotile_outputs
            .iter()
            .position(|(key, _)| *key == identity)
            .map(|index| {
                reel_core::profile::count(reel_core::profile::Phase::MacrotilePass);
                self.macrotile_outputs.swap_remove(index).1
            })
    }

    pub(super) fn macrotile_source(
        &mut self,
        current: &Stream<'_>,
    ) -> Result<Option<Target>, GpuError> {
        let key = Key::new(current.drawing, current.commands, current.next - 1);
        let Some(index) = self
            .macrotile_sources
            .iter()
            .position(|source| source.plan.key == key)
        else {
            return Ok(None);
        };
        let mut source = self.macrotile_sources.swap_remove(index);
        if self.size != source.plan.logical {
            return Err(GpuError::InvalidRecord {
                at: current.commands[current.next - 1].origin.at,
                record: "selected macrotile source grid mismatch",
            });
        }
        // Nonzero offsets continue the same generation in increasing cell order.
        // Reusing cell zero starts a new generation after its last parent read.
        if source.restart {
            // A clipped terminal draw may leave the previous macro scene open.
            // End that generation before restarting at cell zero, without waiting.
            self.gpu.fence()?;
        }
        for target in &mut source.targets {
            let mut writes = std::mem::take(&mut self.target_writes);
            let begun = writes.begin(target.slot, |bytes| self.transient(bytes));
            self.target_writes = writes;
            begun?;
            #[cfg(feature = "profiling")]
            {
                target.trace_begin_use();
                self.targets[target.slot].trace_share_use(target);
            }
        }
        for (index, &push) in source.plan.effects[..source.plan.count].iter().enumerate() {
            let FrameOp::PushGroup(group) = &current.commands[push].op else {
                unreachable!()
            };
            let identity = std::ptr::from_ref(group.effect.as_ref().unwrap()) as usize;
            let mut outputs = std::mem::take(&mut self.macrotile_outputs);
            let held = push_stack(
                &mut outputs,
                (identity, source.targets[index + 1].clone()),
                |bytes| self.transient(bytes),
            );
            self.macrotile_outputs = outputs;
            held?;
        }
        reel_core::profile::count(reel_core::profile::Phase::MacrotileChain);
        Ok(Some(source.targets[0].clone()))
    }
}
