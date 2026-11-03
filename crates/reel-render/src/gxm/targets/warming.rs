//! Optional physical replacements acquired after the complete scratch plan.
use super::{Context, GpuError, Reservation, Target, native};
use crate::common::renderer::{Budgets, reserve};

pub(in crate::gxm) struct Replacement {
    pub target: Target,
    pub ticket: Reservation,
    pub cache_new: bool,
}

#[derive(Default)]
pub(in crate::gxm) struct Replacements {
    entries: Vec<Replacement>,
    bytes: u64,
    _metadata: Option<Reservation>,
}
impl Replacements {
    fn new(count: usize, budgets: &Budgets, local: &Budgets, limit: u64) -> Option<Self> {
        let bytes = count.checked_mul(size_of::<Replacement>())? as u64;
        let metadata = reserve(
            &budgets.transient,
            &local.transient,
            bytes,
            limit,
            "transient",
        )
        .ok()?;
        Some(Self {
            entries: Vec::with_capacity(count),
            bytes: 0,
            _metadata: Some(metadata),
        })
    }
    pub(in crate::gxm) fn take(&mut self, output: &Target) -> Option<Replacement> {
        let index = self.entries.iter().rposition(|entry| {
            entry.target.view.size() == output.view.size()
                && entry.target.view.format() == output.view.format()
                && entry.target.view.target_storage() == output.view.target_storage()
        })?;
        let replacement = self.entries.swap_remove(index);
        self.bytes -= replacement.ticket._local.bytes();
        Some(replacement)
    }

    fn release_for_hit(&mut self, bytes: u64, admission: &crate::common::effect_result::Admission) {
        if !admission.can_retain(bytes) {
            return;
        }
        while bytes
            .checked_add(self.bytes)
            .is_none_or(|total| !admission.can_retain(total))
        {
            let Some(replacement) = self.entries.pop() else {
                break;
            };
            self.bytes -= replacement.ticket._local.bytes();
        }
    }
}

impl Context<'_> {
    /// Unconsumed replacements already occupy local quota, even though they have
    /// not yet replaced a scratch slot or extended a result's lifetime.
    pub(in crate::gxm) fn can_retain_effect(&self, bytes: u64) -> bool {
        bytes
            .checked_add(self.effect_replacements.bytes)
            .is_some_and(|bytes| self.effect_admission.can_retain(bytes))
    }

    pub(in crate::gxm) fn admit_effect_hit(&mut self, bytes: u64) -> bool {
        // An already cached result takes priority over speculative warming.
        // These owners have never been submitted by this preparation.
        self.effect_replacements
            .release_for_hit(bytes, &self.effect_admission);
        self.can_retain_effect(bytes)
    }

    pub(in crate::gxm) fn prepare_effect_replacements(&mut self) -> Result<(), GpuError> {
        if !self.effect_admission.enabled() {
            return Ok(());
        }
        let Some(requests) = self.effect_results.warming_targets(&self.budgets.transient) else {
            return Ok(());
        };
        let Some(mut replacements) = Replacements::new(
            requests.sizes.len(),
            self.budgets,
            &self.budget_scope,
            self.limits.transient_bytes,
        ) else {
            return Ok(());
        };
        let Some(used_count) = self.targets.len().checked_add(requests.sizes.len()) else {
            return Ok(());
        };
        let Some(used_bytes) = used_count.checked_mul(size_of::<Target>()) else {
            return Ok(());
        };
        let Ok(_used_metadata) = reserve(
            &self.budgets.transient,
            &self.budget_scope.transient,
            used_bytes as u64,
            self.limits.transient_bytes,
            "transient",
        ) else {
            return Ok(());
        };
        let mut used = Vec::with_capacity(used_count);
        used.extend(self.targets.iter().cloned());
        let required = self
            .planned_targets
            .as_ref()
            .unwrap_or_else(|| self.offscreen.requirements());
        for size in requests.sizes.iter().copied() {
            if required.count(size) == 0 {
                continue;
            }
            let layout = match self
                .target_storage
                .layout(size, native::TargetFormat::Rgba8)
            {
                Ok(layout) => layout,
                Err(_) => break,
            };
            let bytes = layout.texture_bytes;
            let Some(total) = replacements.bytes.checked_add(bytes) else {
                break;
            };
            if !self.effect_admission.can_retain(total) {
                break;
            }
            let Ok(local) = self.budget_scope.targets.reserve(bytes) else {
                break;
            };
            let (target, cache_new) =
                if let Some(target) = self.target_cache.get(size, layout.storage, &used) {
                    (target, false)
                } else {
                    // Optional warming never reclaims renderer caches. Native
                    // allocation may release only its independently idle pools.
                    match Target::new(
                        self.gpu,
                        size,
                        native::TargetFormat::Rgba8,
                        layout.storage,
                        &self.budgets.targets,
                        self.limits.target_bytes,
                    ) {
                        Ok(target) => (target, true),
                        Err((error, None)) if error.is_memory_pressure() => {
                            self.gpu.refresh();
                            self.gpu.check()?;
                            break;
                        }
                        Err((error, ticket)) => {
                            if let Some(ticket) = ticket {
                                self.failed_native.push(ticket);
                            }
                            return Err(error);
                        }
                    }
                };
            used.push(target.clone());
            replacements.entries.push(Replacement {
                target,
                ticket: Reservation {
                    _shared: None,
                    _local: local,
                },
                cache_new,
            });
            replacements.bytes = total;
        }
        self.effect_replacements = replacements;
        Ok(())
    }
}
