//! Plan physical storage and assemble one native replay work owner.
use super::*;
#[expect(
    clippy::too_many_arguments,
    reason = "one immutable frame and its native preparation owners"
)]
pub(in crate::gxm) fn prepare(
    device: DeviceResources<'_>,
    programs: &programs::Programs,
    caches: Caches<'_>,
    images: HashMap<ResourceHandle, Rc<Image>>,
    frame: &RenderFrame,
    size: [u32; 2],
    capture: bool,
    selection: std::sync::Arc<crate::common::retained::Plan>,
    warm: Option<usize>,
) -> Result<Prepared, (GpuError, Work)> {
    let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Replay);
    let DeviceResources {
        gpu,
        budgets,
        limits,
    } = device;
    let Caches {
        images: image_cache,
        glyphs: glyph_cache,
        effects: effect_cache,
        effect_results,
        effect_results_enabled,
        masks: mask_cache,
        targets: target_cache,
        coverage_targets: coverage_target_cache,
        geometry: geometry_cache,
        retained: retained_cache,
    } = caches;
    glyph_cache.begin_preparation();
    geometry_cache.begin_replay();
    let mut context = Context {
        capture,
        gpu,
        programs,
        limits,
        size,
        budgets,
        budget_scope: Budgets::new(limits),
        tickets: Vec::new(),
        failed_native: Vec::new(),
        images,
        image_cache,
        glyph_cache,
        glyphs: Vec::new(),
        glyph_uploads: Default::default(),
        glyph_plan: Default::default(),
        offscreen: Default::default(),
        effect_cache,
        mask_cache,
        effects: Vec::new(),
        effect_results,
        effect_admission: Default::default(),
        effect_pending: Vec::new(),
        effect_tails: Vec::new(),
        effect_pins: Vec::new(),
        effect_inputs: Vec::new(),
        effect_replacements: Default::default(),
        macrotile_outputs: Vec::new(),
        macrotile_sources: Vec::new(),
        planned_targets: None,
        preparing_targets: false,
        staged_targets: Vec::new(),
        target_cache,
        coverage_target_cache,
        retained_cache,
        selection: selection.clone(),
        retained_inputs: Vec::new(),
        warm,
        warmed: false,
        targets: Vec::new(),
        available_targets: Vec::new(),
        target_writes: Default::default(),
        target_storage: crate::gxm::target_storage(),
        geometry_cache,
        buffers: Vec::new(),
        paint_batch: None,
        instance_batch: None,
        scissor: Scissor::full(size),
        mesh_clip: None,
        at: None,
    };
    let transform = Affine::scale_non_uniform(
        f64::from(size[0]) / frame.info().logical_size[0],
        f64::from(size[1]) / frame.info().logical_size[1],
    );
    let mut stream_started = false;
    let output = crate::common::glyph_plan::Plan::collect(
        std::iter::once(frame),
        transform,
        gpu.capabilities().max_texture_size,
        capture,
        &selection,
        &mut context,
    )
    .and_then(|plan| {
        context.glyph_plan = plan;
        // Glyph preparation has established this frame's non-target CDRAM
        // owners. Existing target payload can be reused; other committed pages
        // remain occupied while choosing the scratch schedule and cache lifetime.
        let memory = gpu.memory_state();
        let target_capacity = memory
            .cdram
            .available_bytes()
            .saturating_add(budgets.targets.usage().bytes)
            .min(limits.target_bytes);
        context.limits.target_bytes = target_capacity;
        let groups = context
            .target_cache
            .groups
            .get(frame.drawing(), capture, &budgets.transient);
        let color_cost = |size| super::targets::cost(gpu, size, false);
        let coverage_cost = |size| super::targets::cost(gpu, size, true);
        let mut resident_tickets = Vec::new();
        let mut resident_reserve = |bytes| {
            resident_tickets.push(reserve(
                &budgets.transient,
                &context.budget_scope.transient,
                bytes,
                limits.transient_bytes,
                "target capacities",
            )?);
            Ok(())
        };
        let direct = context.target_cache.capacities(
            context.target_storage,
            &context.targets,
            &mut resident_reserve,
        )?;
        let sampled = (context.target_storage != native::TargetStorage::Linear)
            .then(|| {
                context.target_cache.capacities(
                    native::TargetStorage::Linear,
                    &context.targets,
                    &mut resident_reserve,
                )
            })
            .transpose()?;
        let (schedule, bytes, retained, inputs) =
            crate::common::offscreen::Plan::choose_with_groups(
                frame,
                None,
                size,
                capture,
                1,
                gpu.capabilities().max_texture_size,
                &context.glyph_plan,
                target_capacity,
                &color_cost,
                Some(&coverage_cost),
                crate::common::target_plan::MaskExecution::Sampled,
                crate::gxm::destination_access(),
                &selection,
                groups,
                Some(crate::common::offscreen::ResidentTargets {
                    direct: &direct,
                    sampled: sampled.as_ref().unwrap_or(&direct),
                }),
                |schedule, bytes| {
                    context.collect_effect_inputs(frame, schedule, bytes, effect_results_enabled)
                },
                |bytes| {
                    reserve(
                        &budgets.transient,
                        &context.budget_scope.transient,
                        bytes,
                        limits.transient_bytes,
                        "transient",
                    )
                },
            )?;
        if bytes > target_capacity {
            return Err(GpuError::Budget {
                domain: "offscreen targets",
                requested: bytes,
                available: target_capacity,
            });
        }
        context.offscreen = schedule;
        if context.offscreen.sampled_backdrop() {
            context.target_storage = native::TargetStorage::Linear;
        }
        context.adopt_effect_inputs(inputs);
        let bytes = context.prepare_target_plan(frame, bytes)?;
        context.effect_admission = crate::common::effect_result::Admission::new(
            bytes,
            target_capacity,
            effect_results_enabled && !capture,
        );
        context.prepare_effect_replacements()?;
        stream_started = true;
        let result = context.stream(frame.drawing(), transform);
        if result.is_ok() {
            context.seal_effect_tails();
        }
        drop(std::mem::take(&mut context.offscreen));
        drop(retained);
        result
    });
    drop(std::mem::take(&mut context.effect_replacements));
    // Keep the compatible high-water pool across effect endpoints. Residency
    // pressure and output-size changes retire it at the admission boundary.
    context.glyphs.extend(context.glyph_cache.take_uploads());
    let work = Work {
        stream_started,
        _tickets: context.tickets,
        _failed_native: context.failed_native,
        _images: context.images,
        _glyphs: context.glyphs,
        _effects: context.effects,
        effect_pending: std::cell::RefCell::new(context.effect_pending),
        _effect_pins: context.effect_pins,
        _effect_inputs: context.effect_inputs,
        _targets: context.targets,
        _retained_inputs: context.retained_inputs,
        buffers: std::cell::RefCell::new(context.buffers),
    };
    match output {
        Ok(output) => Ok(Prepared {
            output,
            retained: context.warmed.then_some(context.warm).flatten(),
            _work: work,
        }),
        Err(error) => Err((error, work)),
    }
}
