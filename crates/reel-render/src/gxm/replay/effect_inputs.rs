//! Freeze cached source views before acquiring the remaining scratch owners.
use super::*;
use crate::common::{
    effect_inputs::{Hit, Key, Raster},
    effect_result::Token,
    target_plan,
};

#[derive(Default)]
pub(super) struct Inputs {
    plan: crate::common::effect_inputs::Plan,
    held: Vec<(Token, Target)>,
    pins: Vec<Target>,
    tickets: Vec<Reservation>,
}

impl Context<'_> {
    pub(super) fn adopt_effect_inputs(&mut self, inputs: Inputs) {
        self.effect_results.release_tail_outputs(|token| {
            inputs.held.iter().any(|(selected, _)| *selected == token)
        });
        self.effect_inputs = inputs.held;
        self.effect_pins = inputs.pins;
        self.tickets.extend(inputs.tickets);
        self.offscreen.set_effect_inputs(inputs.plan);
    }

    pub(super) fn collect_effect_inputs(
        &self,
        frame: &RenderFrame,
        schedule: &crate::common::offscreen::Plan,
        ordinary_bytes: u64,
        enabled: bool,
    ) -> Result<(u64, Inputs), GpuError> {
        if !enabled || self.capture || !self.effect_results.has_outputs() {
            return Ok((ordinary_bytes, Inputs::default()));
        }
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
            schedule,
            masks: target_plan::MaskExecution::Sampled,
            selection: &self.selection,
            cost: &color,
            coverage_cost: schedule.coverage_enabled().then_some(&coverage),
        };
        let mut held: Vec<(Token, Target)> = Vec::new();
        let mut lookup_tickets = Vec::new();
        let mut plan_tickets = Vec::new();
        let mut lookup_failed = false;
        let reserve_metadata = |bytes| {
            reserve(
                &self.budgets.transient,
                &self.budget_scope.transient,
                bytes,
                self.limits.transient_bytes,
                "transient",
            )
        };
        let mut lookup = |proof: crate::common::effect_result::chain::SourceProof<'_>| {
            let probe = proof.resolve(self.effect_results)?;
            let (token, target) = self.effect_results.lookup(&probe)?;
            let owner = held
                .iter()
                .position(|(_, old)| old.same_allocation(&target))
                .unwrap_or(held.len());
            let bytes = target.view.allocated_bytes() as u64;
            if !held.iter().any(|(old, _)| *old == token)
                && push_stack(&mut held, (token, target), |bytes| {
                    lookup_tickets.push(reserve_metadata(bytes)?);
                    Ok(())
                })
                .is_err()
            {
                lookup_failed = true;
                return None;
            }
            Some(Hit {
                token,
                owner,
                bytes,
            })
        };
        let plan = match input.collect_effect_inputs(
            [0, 1],
            self.limits.target_bytes,
            &self.budgets.transient,
            &mut lookup,
            |bytes| {
                plan_tickets.push(reserve_metadata(bytes)?);
                Ok(())
            },
        ) {
            Ok(plan) => plan,
            Err(GpuError::Budget {
                domain: "transient",
                ..
            })
            | Err(GpuError::Pressure {
                domain: "transient",
                ..
            }) => return Ok((ordinary_bytes, Inputs::default())),
            Err(error) => return Err(error),
        };
        if lookup_failed || plan.is_empty() {
            return Ok((ordinary_bytes, Inputs::default()));
        }
        // The common plan already includes each selected physical allocation.
        // Hold the local quota and pixel pin once; later hits reuse this guard
        // without admitting the same bytes a second time.
        let count = plan.owners().len();
        let Ok(metadata) =
            reserve_metadata((count * (size_of::<Target>() + size_of::<Reservation>())) as u64)
        else {
            return Ok((ordinary_bytes, Inputs::default()));
        };
        lookup_tickets.push(metadata);
        let mut pins = Vec::with_capacity(count);
        let mut owner_tickets = Vec::with_capacity(count);
        for &owner in plan.owners() {
            let target = held[owner].1.clone();
            let bytes = target.view.allocated_bytes() as u64;
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
            owner_tickets.push(Reservation {
                _shared: None,
                _local: local,
            });
            pins.push(target);
        }
        held.retain(|(token, _)| plan.entries().iter().any(|entry| entry.token == *token));
        lookup_tickets.extend(owner_tickets);
        lookup_tickets.extend(plan_tickets);
        let bytes = plan.bytes();
        Ok((
            bytes,
            Inputs {
                plan,
                held,
                pins,
                tickets: lookup_tickets,
            },
        ))
    }

    /// Only the frozen root or its proved forwarding descendants can defer a
    /// target. The selected consumer installs a readonly view and keeps its Pop.
    pub(super) fn selected_effect_input(
        &self,
        current: &mut Stream<'_>,
        forward: bool,
        region: Scissor,
    ) -> (Option<usize>, Option<Target>) {
        let plan = self.offscreen.effect_inputs();
        if plan.is_empty() {
            return (None, None);
        }
        let Some(offset) = current.offset else {
            return (None, None);
        };
        let key = Key::new(
            current.drawing,
            offset + current.next - 1,
            Raster::new(
                current.transform,
                self.size,
                self.offscreen.storage_size(self.size),
                region,
            ),
        );
        let selected = current
            .scopes
            .last()
            .effect_input
            .filter(|_| forward)
            .or_else(|| plan.root(&key));
        let Some(index) = selected else {
            return (None, None);
        };
        // This path skips the raw producer, including a deferred crop leaf.
        // Its pending placement must not escape into a later source scope.
        current.source_crop = None;
        let entry = plan.entry(index);
        if entry.input != key {
            return (selected, None);
        }
        let target = self
            .effect_inputs
            .iter()
            .find(|(token, _)| *token == entry.token)
            .expect("selected cached input retained during preparation")
            .1
            .clone();
        debug_assert_eq!(target.size, entry.grid.output_size);
        debug_assert_eq!(
            target
                .source_crop
                .map_or_else(|| target.view.size(), |crop| crop.canonical_storage()),
            entry.grid.output_storage
        );
        #[cfg(feature = "profiling")]
        if let FrameOp::PushGroup(producer) = &current.drawing.commands()[entry.commands.start].op
            && let Some(effect) = &producer.effect
        {
            crate::profiling::record_effect(crate::profiling::EffectSample {
                definition: effect.definition,
                variant: 3,
                hits: 1,
                early_hits: 1,
                ..Default::default()
            });
        }
        current.next = entry.resume - offset;
        (selected, Some(target))
    }
}
