//! Resolve one frozen effect-input plan, then adopt only the selected owners.
use super::*;
use crate::common::{effect_inputs, target_plan};

#[derive(Default)]
pub(in crate::gpu) struct Inputs {
    plan: effect_inputs::Plan,
    views: Vec<(
        crate::common::effect_result::Token,
        super::super::retained::Output,
    )>,
    tickets: Vec<Reservation>,
}

impl Context<'_> {
    #[expect(
        clippy::too_many_arguments,
        reason = "one frozen schedule and its backend cache/metadata owners"
    )]
    pub(in crate::gpu) fn collect_effect_inputs(
        input: &target_plan::Input<'_>,
        cache: &crate::common::effect_result::Cache<super::super::retained::Output>,
        quality: Quality,
        budgets: &Budgets,
        local: &Budgets,
        limits: super::super::GpuLimits,
        enabled: bool,
        ordinary_bytes: u64,
    ) -> Result<(u64, Inputs), GpuError> {
        if !enabled || input.capture || !cache.has_outputs() {
            return Ok((ordinary_bytes, Inputs::default()));
        }
        let limit = limits.transient_bytes;
        let mut views: Vec<(
            crate::common::effect_result::Token,
            super::super::retained::Output,
        )> = Vec::new();
        let mut view_tickets = Vec::new();
        let mut plan_tickets = Vec::new();
        let mut metadata_failed = false;
        let mut lookup = |proof: crate::common::effect_result::chain::SourceProof<'_>| {
            let probe = proof.resolve(cache)?;
            let (token, output) = cache.lookup(&probe)?;
            let owner = views
                .iter()
                .position(|(_, held)| held.target.same_allocation(&output.target))
                .unwrap_or(views.len());
            let bytes = target_bytes(output.target.storage_size(), quality).ok()?;
            let stored = push_stack(&mut views, (token, output), |bytes| {
                view_tickets.push(reserve(
                    &budgets.transient,
                    &local.transient,
                    bytes,
                    limit,
                    "transient",
                )?);
                Ok(())
            });
            if stored.is_err() {
                metadata_failed = true;
                return None;
            }
            Some(effect_inputs::Hit {
                token,
                owner,
                bytes,
            })
        };
        let result = input.collect_effect_inputs(
            [
                match quality.format {
                    wgpu::TextureFormat::Rgba8Unorm => 0,
                    wgpu::TextureFormat::Rgba16Float => 1,
                    _ => return Ok((ordinary_bytes, Inputs::default())),
                },
                quality.samples,
            ],
            limits.target_bytes,
            &budgets.transient,
            &mut lookup,
            |bytes| {
                plan_tickets.push(reserve(
                    &budgets.transient,
                    &local.transient,
                    bytes,
                    limit,
                    "transient",
                )?);
                Ok(())
            },
        );
        let plan = match result {
            Ok(plan) if !metadata_failed => plan,
            Ok(_)
            | Err(GpuError::Budget {
                domain: "transient",
                ..
            }) => {
                return Ok((ordinary_bytes, Inputs::default()));
            }
            Err(error) => return Err(error),
        };
        if plan.is_empty() {
            return Ok((ordinary_bytes, Inputs::default()));
        }
        view_tickets.extend(plan_tickets);
        Ok((
            plan.bytes(),
            Inputs {
                plan,
                views,
                tickets: view_tickets,
            },
        ))
    }

    pub(in crate::gpu) fn adopt_effect_inputs(&mut self, inputs: Inputs) -> Result<(), GpuError> {
        let Inputs {
            plan,
            mut views,
            mut tickets,
        } = inputs;
        let local = &self.budget_scope;
        let budgets = self.budgets;
        let limit = self.gpu.limits.transient_bytes;
        let mut pins = Vec::new();
        let mut allocations = Vec::new();
        for &owner in plan.owners() {
            let output = &views[owner].1;
            let bytes = target_bytes(output.target.storage_size(), self.quality)?;
            let ticket = local.targets.reserve(bytes).map_err(|_| GpuError::Budget {
                domain: "targets",
                requested: bytes,
                available: self
                    .gpu
                    .limits
                    .target_bytes
                    .saturating_sub(local.targets.usage().bytes),
            })?;
            tickets.push(Reservation {
                _shared: None,
                _local: ticket,
            });
            let mut metadata = |bytes| {
                tickets.push(reserve(
                    &budgets.transient,
                    &local.transient,
                    bytes,
                    limit,
                    "transient",
                )?);
                Ok(())
            };
            push_stack(&mut pins, output.target.clone(), &mut metadata)?;
            push_stack(&mut allocations, output.allocation.clone(), &mut metadata)?;
        }
        views.retain(|(token, _)| plan.entries().iter().any(|entry| entry.token == *token));
        self.effect_inputs = views;
        self.effect_pins = pins;
        self.retained_inputs = allocations;
        self.tickets.extend(tickets);
        self.offscreen.set_effect_inputs(plan);
        self.effect_results.release_tail_outputs(|token| {
            self.effect_inputs.iter().any(|(kept, _)| *kept == token)
        });
        Ok(())
    }

    pub(super) fn effect_input(&self, entry: &effect_inputs::Entry) -> Target {
        let output = &self
            .effect_inputs
            .iter()
            .find(|(token, _)| *token == entry.token)
            .expect("selected source owner")
            .1;
        debug_assert_eq!(output.target.size, entry.grid.output_size);
        debug_assert_eq!(
            output
                .target
                .source_crop
                .map_or(output.target.storage_size(), |crop| {
                    crop.canonical_storage()
                }),
            entry.grid.output_storage
        );
        output.target.clone()
    }
}
