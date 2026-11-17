//! Reuse complete effect content before encoding its source passes.
use super::*;
use crate::common::{
    effect_plan,
    effect_result::{Grid, Probe},
};

impl Context<'_> {
    pub(in crate::gpu) fn cached_effect(
        &mut self,
        probe: &Probe<'_>,
    ) -> Result<Option<Target>, GpuError> {
        let Some((_, hit)) = self.effect_results.lookup(probe) else {
            return Ok(None);
        };

        if self
            .effect_pins
            .iter()
            .any(|held| held.same_allocation(&hit.target))
        {
            return Ok(Some(hit.target));
        }
        let bytes = crate::gpu::targets::target_bytes(hit.target.storage_size(), self.quality)?;
        if self.effect_admission.can_retain(bytes) {
            let local = self
                .budget_scope
                .targets
                .reserve(bytes)
                .map_err(|_| GpuError::Budget {
                    domain: "offscreen targets",
                    requested: bytes,
                    available: self
                        .gpu
                        .limits
                        .target_bytes
                        .saturating_sub(self.budget_scope.targets.usage().bytes),
                })?;
            self.tickets.push(crate::common::renderer::Reservation {
                _shared: None,
                _local: local,
            });
            let mut owners = std::mem::take(&mut self.retained_inputs);
            let held =
                crate::common::scopes::push_stack(&mut owners, hit.allocation.clone(), |bytes| {
                    self.transient(bytes)
                });
            self.retained_inputs = owners;
            held?;
            let mut pins = std::mem::take(&mut self.effect_pins);
            let held = crate::common::scopes::push_stack(&mut pins, hit.target.clone(), |bytes| {
                self.transient(bytes)
            });
            self.effect_pins = pins;
            held?;
            self.effect_admission.retain(bytes);
            return Ok(Some(hit.target));
        }
        Ok(None)
    }
}

impl Context<'_> {
    /// The common group proof established that this effect is its isolated
    /// parent's sole source, with identical source grids and ordinary blending.
    pub(super) fn cached_source(
        &mut self,
        current: &mut Stream<'_>,
        group: &reel_ui::frame::FrameGroup,
        parent: (Affine, [u32; 2], Scissor),
        region: Scissor,
        crop: Option<crate::common::source_crop::Crop>,
    ) -> Result<crate::common::effect_result::chain::Lookup, GpuError> {
        let mut lookup = crate::common::effect_result::chain::Lookup::default();
        if !self.effect_admission.enabled() || self.capture {
            return Ok(lookup);
        }
        let Some(offset) = current.offset else {
            return Ok(lookup);
        };
        let Some(end) =
            crate::common::effect_result::source_end(current.commands, current.next - 1)
        else {
            return Ok(lookup);
        };
        let effect = group.effect.as_ref().expect("forwarded effect");
        let effect_transform = effect_plan::output_transform(
            group,
            current
                .scopes
                .last()
                .group
                .zip(current.scopes.last().source_effect_transform),
            parent.0,
        );
        let output_scissor = current.scopes.last().scissor.intersect(parent.2);
        if output_scissor.empty()
            || effect_plan::context(effect, effect_transform, parent.1, self.size).is_none()
        {
            return Ok(lookup);
        }
        let (source_transform, effect_transform) =
            Grid::transforms(current.transform, effect_transform);
        let mut grid = Grid {
            source_transform,
            effect_transform,
            effect_context: None,
            source_size: self.size,
            source_storage: self.offscreen.storage_size(self.size),
            source_scissor: [region.x, region.y, region.width, region.height],
            output_size: parent.1,
            output_storage: self.offscreen.storage_size(parent.1),
            scissor: [
                output_scissor.x,
                output_scissor.y,
                output_scissor.width,
                output_scissor.height,
            ],
            quality: [
                match self.quality.format {
                    wgpu::TextureFormat::Rgba8Unorm => 0,
                    wgpu::TextureFormat::Rgba16Float => 1,
                    _ => return Ok(lookup),
                },
                self.quality.samples,
            ],
        };
        if let Some(crop) = crop {
            grid = crop.original_grid(grid);
        }
        let canonical_source = crop.is_none_or(|crop| crop.original_coordinates());
        if canonical_source
            && effect_plan::shader(
                current.drawing,
                effect,
                current.commands[current.next - 1].origin.at,
            )?
            .descriptor
            .output
                == reel_ui::stage::effect::EffectType::Vec4
        {
            grid = grid.color_context(effect);
        }
        // The complete source proof includes neutral wrappers and their actual
        // scissors; additional draws, masks and effect dependencies keep replay.
        let range = offset + current.next - 1..offset + end;
        let probe = if canonical_source {
            let Some(source) = crate::common::effect_result::chain::source(
                current.drawing,
                range,
                grid,
                &current.groups,
                &|size| self.offscreen.storage_size(size),
                &self.budgets.transient,
            ) else {
                return Ok(lookup);
            };
            lookup.early_reusable = true;
            source.resolve(self.effect_results)
        } else {
            Probe::new(current.drawing, range, effect, grid, false)
        };
        let Some(probe) = probe else {
            return Ok(lookup);
        };
        let Some(hit) = self.cached_effect(&probe)? else {
            return Ok(lookup);
        };
        #[cfg(feature = "profiling")]
        crate::profiling::record_effect(crate::profiling::EffectSample {
            definition: effect.definition,
            variant: 3,
            hits: 1,
            early_hits: 1,
            ..Default::default()
        });
        let unused = current.scopes.last_mut().target.replace(hit);
        // A deferred crop borrowed the ancestor's live target as a placeholder.
        // Only an actual ordinary source allocation can become frame-local idle.
        if crop.is_none()
            && let Some(unused) = unused
        {
            self.recycle_target(unused)?;
        }
        current.next = end;
        current.transform = parent.0;
        current.size = parent.1;
        self.size = parent.1;
        self.scissor = output_scissor;
        self.mesh_clip = current.scopes.last().mesh_clip;
        lookup.hit = true;
        Ok(lookup)
    }
}
