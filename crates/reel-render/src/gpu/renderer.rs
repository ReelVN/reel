use super::{
    Gpu, GpuError, Quality,
    pipeline::{CompositeOperation, PipelineSlots},
    replay::Context,
};
use crate::Usage;
use reel_core::budget::{BudgetCategory, BudgetPool, BudgetTicket, QueueLimits};
use reel_ui::frame::RenderFrame;
use reel_ui::kurbo::Affine;
use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub struct Prepared {
    pub(super) capture: Option<Arc<super::capture::ReadbackStorage>>,
    pub(super) encoder: wgpu::CommandEncoder,
    pub(super) output: wgpu::TextureView,
    pub(super) texture: wgpu::Texture,
    pub(super) quality: Quality,
    pub(super) size: [u32; 2],
    frames: Vec<RenderFrame>,
    tickets: Vec<Reservation>,
    images: HashMap<reel_ui::resources::ResourceHandle, Arc<super::resources::Image>>,
    glyphs: super::glyph_cache::FrameGlyphs,
    effects: Vec<Arc<super::effect::Compiled>>,
    targets: Vec<Arc<super::target_cache::Allocation>>,
    retained_inputs: Vec<Arc<super::target_cache::Allocation>>,
    effect_pending: Vec<crate::common::effect_result::Pending<super::retained::Output>>,
    effect_pins: Vec<super::targets::Target>,
    effect_uniform_slots: Vec<super::effect_uniform::Storage>,
    geometry: Vec<super::geometry::Storage>,
    budget_scope: Budgets,
    admission: BudgetTicket,
    generation: super::DeviceGeneration,
    owner: Arc<()>,
}
impl Prepared {
    pub(super) fn retain_capture_target(&mut self, reservation: Reservation) {
        self.tickets.push(reservation);
    }
    pub fn quality(&self) -> Quality {
        self.quality
    }
    pub fn size(&self) -> [u32; 2] {
        self.size
    }
}
pub(super) struct Inflight {
    capture: Option<Arc<super::capture::ReadbackStorage>>,
    id: u64,
    done: Arc<AtomicBool>,
    _frames: Vec<RenderFrame>,
    _tickets: Vec<Reservation>,
    _images: HashMap<reel_ui::resources::ResourceHandle, Arc<super::resources::Image>>,
    _glyphs: super::glyph_cache::FrameGlyphs,
    _effects: Vec<Arc<super::effect::Compiled>>,
    _targets: Vec<Arc<super::target_cache::Allocation>>,
    _retained_inputs: Vec<Arc<super::target_cache::Allocation>>,
    _effect_pins: Vec<super::targets::Target>,
    effect_uniform_slots: Vec<super::effect_uniform::Storage>,
    _admission: BudgetTicket,
    geometry: Vec<super::geometry::Storage>,
}
struct CacheRetry {
    cause: GpuError,
    released: bool,
}
fn pressured_usage(cause: &GpuError, usage: Usage) -> Option<u64> {
    match pressure_pool(cause)? {
        BudgetCategory::Offscreen => Some(usage.targets),
        BudgetCategory::GpuResource => Some(usage.resources),
        BudgetCategory::Temporary => Some(usage.transient),
        _ => unreachable!("renderer pressure uses its three allocation domains"),
    }
}
fn pressure_pool(cause: &GpuError) -> Option<BudgetCategory> {
    let GpuError::Pressure { domain, .. } = cause else {
        return None;
    };
    match *domain {
        "targets" => Some(BudgetCategory::Offscreen),
        "image upload" | "texture upload" | "glyph atlas" | "glyph upload" => {
            Some(BudgetCategory::GpuResource)
        }
        "transient"
        | "mesh scratch"
        | "geometry"
        | "retained plan"
        | "source plan"
        | "retained cache"
        | "retained dependencies"
        | "retained submission"
        | "image plan"
        | "image upload metadata"
        | "glyph preparation" => Some(BudgetCategory::Temporary),
        _ => None,
    }
}
pub struct Renderer {
    pub(super) gpu: Gpu,
    pub(super) capture_output: Option<super::SurfacePass>,
    inflight: Vec<Inflight>,
    next: u64,
    budgets: Budgets,
    admission: BudgetPool,
    owner: Arc<()>,
    pipelines: PipelineSlots,
    images: super::image_cache::ImageCache,
    glyphs: super::glyph_cache::GlyphCache,
    targets: super::target_cache::TargetCache,
    coverage_targets: super::target_cache::TargetCache,
    geometry: super::geometry::Cache,
    #[cfg(test)]
    sprite_instances_enabled: bool,
    retained: super::retained::Cache,
    pub(super) effect_results: crate::common::effect_result::Cache<super::retained::Output>,
    pub(super) effect_uniforms: super::effect_uniform::Cache,
}
pub(super) use crate::common::renderer::{Budgets, Reservation, reserve};

impl Renderer {
    pub fn gpu(&self) -> &Gpu {
        &self.gpu
    }
    pub fn new(gpu: Gpu) -> Self {
        let budgets = Budgets::new(gpu.limits);
        let admission = BudgetPool::new(
            BudgetCategory::Temporary,
            QueueLimits {
                max_requests: gpu.limits.max_inflight.min(u32::MAX as usize) as u32,
                max_bytes: 0,
            },
        );
        Self {
            gpu,
            capture_output: None,
            inflight: Vec::new(),
            next: 1,
            budgets,
            admission,
            owner: Arc::new(()),
            pipelines: PipelineSlots::default(),
            images: Default::default(),
            glyphs: Default::default(),
            targets: Default::default(),
            coverage_targets: Default::default(),
            geometry: Default::default(),
            #[cfg(test)]
            sprite_instances_enabled: true,
            retained: Default::default(),
            effect_results: Default::default(),
            effect_uniforms: Default::default(),
        }
    }
    /// Prepare the device's supported quality tiers, coverage and built-in image masks
    /// before showing the first frame. Repeated calls reuse the same slots.
    /// This creates no frame targets or decoded image buffers.
    pub fn prepare_pipelines(&self) -> Result<(), GpuError> {
        // Start desktop upload workers before the first interactive frame.
        rayon::current_num_threads();
        for quality in self.gpu.supported_qualities() {
            let pipelines = self.pipelines.get(&self.gpu, quality)?;
            if self.gpu.coverage_quality(quality).is_some() {
                self.pipelines
                    .get_coverage(&self.gpu, quality, &pipelines)?;
            }
            pipelines
                .masks
                .prepare_builtin(&self.gpu, quality, &self.budgets.resources)?;
        }
        Ok(())
    }
    pub(super) fn reserve_readback(
        &self,
        prepared: &Prepared,
        bytes: u64,
    ) -> Result<Reservation, GpuError> {
        reserve(
            &self.budgets.transient,
            &prepared.budget_scope.transient,
            bytes,
            self.gpu.limits.transient_bytes,
            "readback",
        )
    }
    /// Current renderer-owned residency across the shared accounting domains.
    pub fn usage(&self) -> Usage {
        let [cached_images, retired_images, retired_image_readers] = self.images.usage();
        Usage {
            targets: self.budgets.targets.usage().bytes,
            resources: self.budgets.resources.usage().bytes,
            cached_images,
            retired_images,
            retired_image_readers,
            transient: self.budgets.transient.usage().bytes,
            cached_geometry: self.geometry.bytes(),
            shared: None,
        }
    }
    pub fn poll(&mut self) -> Result<Vec<u64>, GpuError> {
        let mut completed = Vec::new();
        self.poll_completed(usize::MAX, |id, _| completed.push(id))?;
        Ok(completed)
    }
    pub fn has_pending(&self) -> bool {
        !self.inflight.is_empty()
            || self.has_pending_image_uploads()
            || !self.retained.pending.lock().unwrap().entries.is_empty()
    }
    /// Submitted image uploads still own staging and may unblock a preparation
    /// slice. Hosts can poll these short-lived completions more frequently.
    pub fn has_pending_image_uploads(&self) -> bool {
        self.images.has_pending()
    }
    pub(super) fn poll_completed(
        &mut self,
        mut limit: usize,
        mut completed: impl FnMut(u64, &RenderFrame),
    ) -> Result<bool, GpuError> {
        self.gpu.check()?;
        self.images.retire(&[]);
        self.glyphs.retire(&[]);
        let result = self.gpu.device.poll(wgpu::PollType::Poll);
        self.gpu.check()?;
        result.map_err(|e| GpuError::Device(e.to_string()))?;
        let mut resources_available = limit != 0 && self.images.poll();
        if resources_available {
            limit -= 1;
        }
        for pending in &self.retained.pending.lock().unwrap().entries {
            if pending.flight.done.load(Ordering::Acquire) {
                for slot in &pending.flight.geometry {
                    slot.storage.ready()?;
                }
            }
        }
        self.retained
            .pending
            .lock()
            .unwrap()
            .entries
            .retain_mut(|pending| {
                if limit != 0
                    && pending.flight.done.load(Ordering::Acquire)
                    && pending
                        .flight
                        .geometry
                        .iter()
                        .all(|slot| slot.storage.ready().unwrap_or(false))
                {
                    self.geometry.recycle(&mut pending.flight.geometry);
                    self.effect_uniforms.recycle(
                        &mut pending.flight.effect_uniform_slots,
                        &self.budgets.transient,
                    );
                    self.retained.inputs.complete(pending.token);
                    resources_available = true;
                    limit -= 1;
                    false
                } else {
                    true
                }
            });
        for submission in &self.inflight {
            if submission.done.load(Ordering::Acquire) {
                for slot in &submission.geometry {
                    slot.storage.ready()?;
                }
            }
        }
        self.inflight.retain_mut(|s| {
            if limit != 0
                && s.done.load(Ordering::Acquire)
                && s.geometry
                    .iter()
                    .all(|slot| slot.storage.ready().unwrap_or(false))
            {
                if s.capture.is_none() {
                    completed(
                        s.id,
                        s._frames.last().expect("submission owns its output frame"),
                    );
                }
                self.images.discard_uploaded_pixels(&s._images);
                self.geometry.recycle(&mut s.geometry);
                self.effect_uniforms
                    .recycle(&mut s.effect_uniform_slots, &self.budgets.transient);
                limit -= 1;
                false
            } else {
                true
            }
        });
        self.images.retire(&[]);
        self.glyphs.retire(&[]);
        self.pipelines.prune_masks();
        self.targets.prune_cold();
        self.coverage_targets.prune_cold();
        Ok(resources_available)
    }
    pub(super) fn retire(&self, resources: &[reel_core::identity::ResourceVersion]) {
        if !resources.is_empty() {
            self.effect_uniforms.clear();
        }
        self.images.retire(resources);
        self.glyphs.retire(resources);
        for submission in &self.inflight {
            for frame in &submission._frames {
                for lease in frame.resources().leases() {
                    if resources.contains(lease.version()) {
                        lease.prohibit_new_leases();
                    }
                }
            }
        }
    }
    /// Prepare an immutable candidate. `Preparing` means internal GPU dependencies
    /// were submitted; poll completions and retry this same candidate. Cancellation
    /// releases cache ownership while submitted owners remain until their fence.
    pub fn prepare(&self, frame: &RenderFrame) -> Result<Prepared, GpuError> {
        self.prepare_frames(frame, None, frame.info().physical_size, false)
    }
    /// Cancel exact-candidate continuation while submitted owners await their fence.
    pub fn cancel_preparation(&mut self, frame: &RenderFrame) {
        self.retained.inputs.cancel(&frame.key());
        let mut ceiling = self.retained.ceiling.lock().unwrap();
        if ceiling.as_ref().is_some_and(|(key, _)| *key == frame.key()) {
            *ceiling = None;
        }
    }
    pub(super) fn cancel_preparations(&mut self) {
        self.retained.inputs.clear();
        *self.retained.ceiling.lock().unwrap() = None;
    }
    pub(super) fn prepare_capture(
        &self,
        frame: &RenderFrame,
        size: [u32; 2],
    ) -> Result<Prepared, GpuError> {
        self.prepare_frames(frame, None, size, true)
    }
    /// Both immutable input frames stay leased through queue completion. No input acceptance.
    /// On `Preparing`, poll completions and continue with the same input frames.
    pub fn crossfade(
        &self,
        from: &RenderFrame,
        to: &RenderFrame,
        progress: f32,
    ) -> Result<Prepared, GpuError> {
        if !progress.is_finite()
            || !(0. ..=1.).contains(&progress)
            || from.info() != to.info()
            || from.surface_epoch() != to.surface_epoch()
        {
            return Err(GpuError::InvalidSize);
        }
        self.prepare_frames(from, Some((to, progress)), from.info().physical_size, false)
    }
    fn prepare_frames(
        &self,
        frame: &RenderFrame,
        second: Option<(&RenderFrame, f32)>,
        size: [u32; 2],
        capture: bool,
    ) -> Result<Prepared, GpuError> {
        let result = self.prepare_frames_inner(frame, second, size, capture);
        #[cfg(feature = "profiling")]
        {
            use crate::profiling::EffectInputEvent;
            let event = match &result {
                Err(GpuError::Busy) => Some(EffectInputEvent::BusyReturns),
                Err(GpuError::Preparing) => Some(EffectInputEvent::PreparingReturns),
                Err(GpuError::Pressure { .. }) => Some(EffectInputEvent::PressureReturns),
                _ => None,
            };
            if let Some(event) = event {
                crate::profiling::record_effect_input_event(event, 1);
            }
        }
        result
    }
    fn prepare_frames_inner(
        &self,
        frame: &RenderFrame,
        second: Option<(&RenderFrame, f32)>,
        size: [u32; 2],
        capture: bool,
    ) -> Result<Prepared, GpuError> {
        let mut epoch = self.retained.epoch.lock().unwrap();
        if *epoch != Some(frame.surface_epoch()) {
            self.retained.inputs.clear();
            self.effect_results.clear();
            self.effect_uniforms.clear();
            *self.retained.ceiling.lock().unwrap() = None;
            *epoch = Some(frame.surface_epoch());
        }
        drop(epoch);
        if !capture && self.retained.inputs.pending(&frame.key()) {
            return Err(GpuError::Preparing);
        }
        // Capture reserves its output and readback after replay. Keep that
        // original admission path free of optional reusable staging.
        if capture {
            self.geometry.clear();
            self.images.clear_idle();
            self.effect_results.clear();
            self.effect_uniforms.clear();
        }
        let policy = super::geometry::UploadPolicy::new(!capture);
        let result = self.prepare_cached(frame, second, size, capture, &policy, None);
        let fallback = policy.optional.get()
            && matches!(
                result,
                Err(GpuError::Budget {
                    domain: "transient" | "mesh scratch" | "geometry",
                    ..
                } | GpuError::Pressure {
                    domain: "transient" | "mesh scratch" | "geometry",
                    ..
                })
            );
        if fallback || matches!(result, Err(GpuError::Pressure { .. })) {
            let cause = result.err().unwrap();
            #[cfg(feature = "profiling")]
            {
                use crate::profiling::{EffectInputEvent, record_effect_input_event};
                record_effect_input_event(EffectInputEvent::PrepareRetries, 1);
                if pressure_pool(&cause) == Some(BudgetCategory::Offscreen) {
                    record_effect_input_event(EffectInputEvent::OffscreenRetries, 1);
                }
            }
            let resident = pressured_usage(&cause, self.usage());
            if pressure_pool(&cause) == Some(BudgetCategory::Offscreen) {
                self.effect_results.release_outputs();
            } else {
                self.effect_results.clear();
            }
            self.effect_uniforms.clear();
            let metadata_pressure =
                fallback || pressure_pool(&cause) == Some(BudgetCategory::Temporary);
            if metadata_pressure {
                self.targets.1.lock().unwrap().disable();
                self.coverage_targets.1.lock().unwrap().disable();
                self.geometry.clear();
                self.targets.release_idle();
                self.coverage_targets.release_idle();
            }
            match pressure_pool(&cause) {
                Some(BudgetCategory::Offscreen) => {
                    self.targets.release_idle();
                    self.coverage_targets.release_idle();
                }
                Some(BudgetCategory::GpuResource) => {
                    self.images.clear_idle();
                }
                _ => {}
            }
            #[cfg(test)]
            self.gpu
                .upload_creations
                .geometry_retries
                .fetch_add(1, Ordering::Relaxed);
            // Rebuild demand before replaying. If optional cache reclamation
            // releases no capacity, preserve the original pressure result.
            let result = self.prepare_cached(
                frame,
                second,
                size,
                capture,
                &super::geometry::UploadPolicy::new(!capture && !fallback),
                Some(CacheRetry {
                    released: fallback
                        || resident.is_some_and(|before| {
                            pressured_usage(&cause, self.usage())
                                .is_some_and(|after| after < before)
                        }),
                    cause,
                }),
            );
            if metadata_pressure {
                self.targets.1.lock().unwrap().clear();
                self.coverage_targets.1.lock().unwrap().clear();
            }
            return result;
        }
        result
    }
    fn prepare_cached(
        &self,
        frame: &RenderFrame,
        second: Option<(&RenderFrame, f32)>,
        size: [u32; 2],
        capture: bool,
        geometry_policy: &super::geometry::UploadPolicy,
        mut retry: Option<CacheRetry>,
    ) -> Result<Prepared, GpuError> {
        let effect_results_enabled = retry.is_none();
        loop {
            self.gpu.check()?;
            self.images.retire(&[]);
            self.glyphs.retire(&[]);
            let budget_scope = Budgets::new(self.gpu.limits);
            let mut tickets = Vec::new();
            let cached_quality = self.retained.inputs.quality();
            let selection = Arc::new(crate::common::retained::Plan::collect(
                std::iter::once(frame).chain(second.map(|(frame, _)| frame)),
                size,
                capture,
                &self.budgets,
                &budget_scope,
                self.gpu.limits.transient_bytes,
                |key| {
                    cached_quality
                        .and_then(|quality| self.retained.inputs.lookup(key, quality, &frame.key()))
                },
                |bytes| {
                    tickets.push(reserve(
                        &self.budgets.transient,
                        &budget_scope.transient,
                        bytes,
                        self.gpu.limits.transient_bytes,
                        "retained plan",
                    )?);
                    Ok(())
                },
            )?);
            self.retained.inputs.retain(&selection);
            let preferred = cached_quality.filter(|_| selection.has_cached());
            let plan = super::image_plan::collect(
                std::iter::once(frame).chain(second.map(|(frame, _)| frame)),
                self.gpu.device.limits().max_texture_dimension_2d,
                capture,
                &selection,
                |bytes| {
                    tickets.push(reserve(
                        &self.budgets.transient,
                        &budget_scope.transient,
                        bytes,
                        self.gpu.limits.transient_bytes,
                        "image plan",
                    )?);
                    Ok(())
                },
            )?;
            if let Some(retry) = retry.take()
                && matches!(retry.cause, GpuError::Pressure { .. })
            {
                let resident = pressured_usage(&retry.cause, self.usage());
                if pressure_pool(&retry.cause) == Some(BudgetCategory::GpuResource) {
                    self.images.retain_demand(&plan.images);
                    self.geometry.glyphs.lock().unwrap().clear();
                    self.glyphs.prune_idle();
                }
                if !retry.released
                    && !resident.is_some_and(|before| {
                        pressured_usage(&retry.cause, self.usage())
                            .is_some_and(|after| after < before)
                    })
                {
                    return Err(retry.cause);
                }
            }
            for mask in &plan.masks {
                if mask.definition.program.sources().len() + 3
                    > self
                        .gpu
                        .device
                        .limits()
                        .max_sampled_textures_per_shader_stage as usize
                {
                    return Err(GpuError::Unsupported {
                        at: mask.at,
                        record: "mask exceeds device sampled texture capacity",
                    });
                }
            }
            // Cold image uploads may yield Busy. Complete that admission before
            // preparing glyphs so retries cannot discard and rerasterize their work.
            let mut images = self.images.prepare(
                &self.gpu,
                &plan.images,
                &self.budgets,
                &budget_scope,
                &self.admission,
                &self.glyphs,
            )?;
            if !plan.aliases.is_empty() {
                let bytes = (plan.aliases.len() * 128) as u64;
                tickets.push(reserve(
                    &self.budgets.transient,
                    &budget_scope.transient,
                    bytes,
                    self.gpu.limits.transient_bytes,
                    "image plan",
                )?);
                images.reserve(plan.aliases.len());
                for alias in &plan.aliases {
                    let backing = images
                        .get(&alias.backing.handle())
                        .ok_or(GpuError::InvalidSize)?
                        .clone();
                    let storage = reserve(
                        &self.budgets.transient,
                        &budget_scope.transient,
                        (size_of::<super::resources::Image>() + 2 * size_of::<usize>()) as u64,
                        self.gpu.limits.transient_bytes,
                        "image plan",
                    )?;
                    let image = super::resources::Image::atlas(backing, alias.source, storage)?;
                    images.insert(alias.source.handle(), Arc::new(image));
                }
            }
            let transform = Affine::scale_non_uniform(
                f64::from(size[0]) / frame.info().logical_size[0],
                f64::from(size[1]) / frame.info().logical_size[1],
            );
            let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
            let mut glyphs = super::glyph_cache::FrameGlyphs::default();
            let glyph_plan = crate::common::glyph_plan::Plan::collect(
                std::iter::once(frame).chain(second.map(|(frame, _)| frame)),
                transform,
                self.gpu.device.limits().max_texture_dimension_2d,
                capture,
                &selection,
                &mut super::glyph_cache::Preparation {
                    uploads: Default::default(),
                    gpu: &self.gpu,
                    encoder: &mut encoder,
                    cache: &self.glyphs,
                    frame: &mut glyphs,
                    budgets: &self.budgets,
                    local: &budget_scope,
                    tickets: &mut tickets,
                    images: &self.images,
                },
            )?;
            let groups = self.targets.1.lock().unwrap().get(
                frame.drawing(),
                capture,
                &self.budgets.transient,
            );
            let mut offscreen = crate::common::offscreen::Plan::default();
            let mut offscreen_tickets = Vec::new();
            let mut required_target_bytes = 0;
            let mut selected_effect_inputs = None;
            let mut target_plan = |quality: Quality| {
                selected_effect_inputs = None;
                offscreen = Default::default();
                offscreen_tickets.clear();
                let coverage_quality = self.gpu.coverage_quality(quality);
                let coverage_cost = |extent| {
                    super::targets::target_bytes(extent, coverage_quality.expect("coverage format"))
                };
                let mut resident_tickets = Vec::new();
                let resident = self.targets.capacities(size, quality, |bytes| {
                    resident_tickets.push(reserve(
                        &self.budgets.transient,
                        &budget_scope.transient,
                        bytes,
                        self.gpu.limits.transient_bytes,
                        "target capacities",
                    )?);
                    Ok(())
                })?;
                let (schedule, targets, retained, inputs) =
                    crate::common::offscreen::Plan::choose_with_groups(
                        frame,
                        second.map(|(frame, _)| frame),
                        size,
                        capture,
                        quality.samples,
                        self.gpu.device.limits().max_texture_dimension_2d,
                        &glyph_plan,
                        self.gpu.limits.target_bytes,
                        &|extent| super::targets::target_bytes(extent, quality),
                        coverage_quality
                            .map(|_| &coverage_cost as &dyn Fn([u32; 2]) -> Result<u64, GpuError>),
                        crate::common::target_plan::MaskExecution::Inline,
                        crate::common::blend::DestinationAccess::Sampled,
                        &selection,
                        groups.clone(),
                        Some(crate::common::offscreen::ResidentTargets {
                            direct: &resident,
                            sampled: &resident,
                        }),
                        |schedule, bytes| {
                            Context::collect_effect_inputs(
                                &crate::common::target_plan::Input {
                                    frame,
                                    second: second.map(|(frame, _)| frame),
                                    size,
                                    capture,
                                    samples: quality.samples,
                                    max_dimension: self
                                        .gpu
                                        .device
                                        .limits()
                                        .max_texture_dimension_2d,
                                    glyphs: &glyph_plan,
                                    schedule,
                                    masks: crate::common::target_plan::MaskExecution::Inline,
                                    selection: &selection,
                                    cost: &|extent| super::targets::target_bytes(extent, quality),
                                    coverage_cost: coverage_quality.map(|_| {
                                        &coverage_cost as &dyn Fn([u32; 2]) -> Result<u64, GpuError>
                                    }),
                                },
                                &self.effect_results,
                                quality,
                                &self.budgets,
                                &budget_scope,
                                self.gpu.limits,
                                effect_results_enabled,
                                bytes,
                            )
                        },
                        |bytes| {
                            reserve(
                                &self.budgets.transient,
                                &budget_scope.transient,
                                bytes,
                                self.gpu.limits.transient_bytes,
                                "transient",
                            )
                        },
                    )?;
                offscreen = schedule;
                required_target_bytes = targets;
                offscreen_tickets = retained;
                selected_effect_inputs = Some(inputs);
                Ok(targets)
            };
            let quality = if let Some(quality) = preferred {
                let bytes = target_plan(quality)?;
                if bytes > self.gpu.limits.target_bytes {
                    let mut qualities = self
                        .gpu
                        .supported_qualities()
                        .skip_while(|candidate| *candidate != quality);
                    qualities.next();
                    if let Some(lower) = qualities.next() {
                        self.retained.inputs.clear();
                        *self.retained.ceiling.lock().unwrap() = Some((frame.key(), lower));
                        continue;
                    }
                    return Err(GpuError::Budget {
                        domain: "targets",
                        requested: bytes,
                        available: self.gpu.limits.target_bytes,
                    });
                }
                quality
            } else {
                let ceiling = self
                    .retained
                    .ceiling
                    .lock()
                    .unwrap()
                    .as_ref()
                    .filter(|(key, _)| *key == frame.key())
                    .map(|(_, quality)| *quality);
                let mut allowed = ceiling.is_none();
                self.gpu
                    .quality_for_targets(size, |quality| {
                        allowed |= Some(quality) == ceiling;
                        if !allowed {
                            return Ok(self.gpu.limits.target_bytes.saturating_add(1));
                        }
                        target_plan(quality)
                    })?
                    .0
            };
            let pipelines = self.pipelines.get(&self.gpu, quality)?;
            let coverage_pipeline = if offscreen.coverage_requirements().iter().len() != 0 {
                Some(
                    self.pipelines
                        .get_coverage(&self.gpu, quality, &pipelines)?,
                )
            } else {
                None
            };
            for mask in plan.masks {
                pipelines.masks.get(
                    &self.gpu,
                    quality,
                    &mask.definition.program,
                    mask.definition.parameters,
                    &self.budgets.resources,
                )?;
            }
            for image in images.values() {
                if image.backing.is_some() {
                    continue;
                }
                tickets.push(Reservation {
                    _shared: None,
                    _local: super::image_cache::reserve_local(
                        &budget_scope.resources,
                        image.residency.as_ref().unwrap().bytes(),
                        self.gpu.limits.resource_bytes,
                    )?,
                });
            }
            // Upload batches own their own admission. Reserving this earlier would
            // deadlock cold frames on a host configured for one in-flight submission.
            let admission = self.admission.reserve(0).map_err(|_| GpuError::Busy)?;
            self.targets.configure(size, quality);
            let coverage_quality = self.gpu.coverage_quality(quality);
            if let Some(coverage_quality) = coverage_quality {
                self.coverage_targets.configure(size, coverage_quality);
            } else {
                self.coverage_targets.clear();
            }
            let warm = selection
                .warm
                .map(|index| {
                    self.retained.inputs.start(
                        selection.entries[index].key.clone(),
                        quality,
                        frame.key(),
                        &self.budgets.transient,
                        self.gpu.limits.transient_bytes,
                    )
                })
                .transpose()?;
            let mut ctx = Context {
                capture,
                gpu: &self.gpu,
                encoder,
                pipelines,
                quality,
                size,
                budgets: &self.budgets,
                budget_scope,
                tickets,
                images,
                image_cache: &self.images,
                glyph_cache: &self.glyphs,
                glyphs,
                glyph_plan,
                offscreen,
                effects: Vec::new(),
                white: None,
                target_cache: &self.targets,
                coverage_target_cache: &self.coverage_targets,
                coverage_quality,
                coverage_pipeline,
                retained_cache: &self.retained.inputs,
                effect_results: &self.effect_results,
                effect_admission: crate::common::effect_result::Admission::new(
                    required_target_bytes,
                    self.gpu.limits.target_bytes,
                    effect_results_enabled && !capture,
                ),
                effect_pending: Vec::new(),
                effect_pins: Vec::new(),
                effect_tails: Vec::new(),
                effect_inputs: Vec::new(),
                effect_uniform_cache: &self.effect_uniforms,
                effect_uniform_slots: Vec::new(),
                selection,
                retained_inputs: Vec::new(),
                warm,
                warmed: false,
                target_allocations: Vec::new(),
                available_targets: Vec::new(),
                target_writes: Default::default(),
                scissor: super::targets::Scissor::full(size),
                mesh_clip: None,
                paint_batch: None,
                instance_batch: None,
                #[cfg(test)]
                sprite_instances_enabled: self.sprite_instances_enabled,
                geometry_policy,
                geometry_cache: &self.geometry,
                geometry: Vec::new(),
            };
            let transform = Affine::scale_non_uniform(
                f64::from(size[0]) / frame.info().logical_size[0],
                f64::from(size[1]) / frame.info().logical_size[1],
            );
            ctx.adopt_effect_inputs(
                selected_effect_inputs.expect("selected target plan owns its resolved inputs"),
            )?;
            let mut output = ctx.stream(frame.drawing(), transform)?;
            ctx.offscreen.disable_batches();
            let mut frames = vec![frame.clone()];
            if let Some((second, progress)) = second
                && !ctx.warmed
            {
                let b = ctx.stream(second.drawing(), transform)?;
                if ctx.warmed {
                    output = b;
                } else {
                    let mixed = ctx.target()?;
                    ctx.combine(
                        &mixed,
                        &output.view,
                        &b.view,
                        1.,
                        0.,
                        CompositeOperation::Crossfade(progress),
                    )?;
                    output = mixed;
                }
                frames.push(second.clone());
            }
            ctx.seal_effect_tails();
            drop(ctx.offscreen);
            drop(offscreen_tickets);
            self.gpu.check()?;
            let retained = if ctx.warmed {
                self.retained
                    .pending
                    .lock()
                    .unwrap()
                    .reserve(&self.budgets.transient, self.gpu.limits.transient_bytes)?;
                ctx.tickets.push(Reservation {
                    _shared: None,
                    _local: ctx
                        .budget_scope
                        .transient
                        .reserve(size_of::<super::retained::Pending>() as u64)
                        .map_err(|_| GpuError::Budget {
                            domain: "retained submission",
                            requested: size_of::<super::retained::Pending>() as u64,
                            available: self
                                .gpu
                                .limits
                                .transient_bytes
                                .saturating_sub(ctx.budget_scope.transient.usage().bytes),
                        })?,
                });
                let token = ctx
                    .warm
                    .expect("cold retained phase has an admitted output");
                let allocation = ctx
                    .target_allocations
                    .iter()
                    .find(|allocation| allocation.target.view == output.view)
                    .expect("cold retained output belongs to this preparation")
                    .clone();
                let storage = output.storage_size();
                self.retained.inputs.stage(
                    token,
                    super::retained::Output {
                        allocation,
                        target: output.clone(),
                    },
                    super::targets::target_bytes(storage, quality)?,
                );
                Some(token)
            } else {
                None
            };
            let mut prepared = Prepared {
                capture: None,
                encoder: ctx.encoder,
                output: output.view,
                texture: output.texture,
                quality,
                size,
                frames,
                tickets: ctx.tickets,
                images: ctx.images,
                glyphs: ctx.glyphs,
                effects: ctx.effects,
                targets: ctx.target_allocations,
                retained_inputs: ctx.retained_inputs,
                effect_pending: ctx.effect_pending,
                effect_pins: ctx.effect_pins,
                effect_uniform_slots: ctx.effect_uniform_slots,
                geometry: ctx.geometry,
                budget_scope: ctx.budget_scope,
                admission,
                generation: self.gpu.generation,
                owner: self.owner.clone(),
            };
            if let Some(token) = retained {
                let done = Arc::new(AtomicBool::new(false));
                for slot in &mut prepared.effect_uniform_slots {
                    slot.upload(&self.gpu);
                }
                self.gpu.queue.submit([prepared.encoder.finish()]);
                self.effect_results.submitted();
                self.effect_uniforms.submitted();
                self.glyphs.publish(&prepared.glyphs);
                for pending in prepared.effect_pending {
                    self.effect_results
                        .publish(pending, &self.budgets.transient);
                }
                let signal = done.clone();
                self.gpu
                    .queue
                    .on_submitted_work_done(move || signal.store(true, Ordering::Release));
                self.targets.clear();
                self.coverage_targets.clear();
                self.retained
                    .pending
                    .lock()
                    .unwrap()
                    .entries
                    .push(super::retained::Pending {
                        token,
                        flight: Inflight {
                            capture: None,
                            id: 0,
                            done,
                            _frames: prepared.frames,
                            _tickets: prepared.tickets,
                            _images: prepared.images,
                            _glyphs: prepared.glyphs,
                            _effects: prepared.effects,
                            _targets: prepared.targets,
                            _retained_inputs: prepared.retained_inputs,
                            _effect_pins: prepared.effect_pins,
                            effect_uniform_slots: prepared.effect_uniform_slots,
                            _admission: prepared.admission,
                            geometry: prepared.geometry,
                        },
                    });
                return Err(GpuError::Preparing);
            }
            return Ok(prepared);
        }
    }
    pub(super) fn validate_prepared(&self, prepared: &Prepared) -> Result<(), GpuError> {
        self.gpu.check()?;
        if !Arc::ptr_eq(&prepared.owner, &self.owner) || prepared.generation != self.gpu.generation
        {
            return Err(GpuError::WrongDevice);
        }
        Ok(())
    }
    pub fn submit(&mut self, mut prepared: Prepared) -> Result<u64, GpuError> {
        self.gpu.check()?;
        if !Arc::ptr_eq(&prepared.owner, &self.owner) || prepared.generation != self.gpu.generation
        {
            return Err(GpuError::WrongDevice);
        }
        if self.inflight.len() >= self.gpu.limits.max_inflight {
            return Err(GpuError::Busy);
        }
        let id = self.next;
        self.next = id.checked_add(1).ok_or(GpuError::IdentityExhausted)?;
        let done = Arc::new(AtomicBool::new(false));
        self.targets.reserve_metadata(
            prepared.size,
            prepared.quality,
            &prepared.targets,
            &self.budgets.transient,
            self.gpu.limits.transient_bytes,
        );
        if let Some(coverage_quality) = self.gpu.coverage_quality(prepared.quality) {
            self.coverage_targets.reserve_metadata(
                prepared.size,
                coverage_quality,
                &prepared.targets,
                &self.budgets.transient,
                self.gpu.limits.transient_bytes,
            );
        }
        for slot in &mut prepared.effect_uniform_slots {
            slot.upload(&self.gpu);
        }
        self.gpu.queue.submit([prepared.encoder.finish()]);
        self.effect_results.submitted();
        self.effect_uniforms.submitted();
        for pending in prepared.effect_pending {
            self.effect_results
                .publish(pending, &self.budgets.transient);
        }
        self.glyphs.publish(&prepared.glyphs);
        let signal = done.clone();
        self.gpu
            .queue
            .on_submitted_work_done(move || signal.store(true, Ordering::Release));
        self.inflight.push(Inflight {
            capture: prepared.capture,
            id,
            done,
            _frames: prepared.frames,
            _tickets: prepared.tickets,
            _images: prepared.images,
            _glyphs: prepared.glyphs,
            _effects: prepared.effects,
            _targets: prepared.targets,
            _retained_inputs: prepared.retained_inputs,
            _effect_pins: prepared.effect_pins,
            effect_uniform_slots: prepared.effect_uniform_slots,
            _admission: prepared.admission,
            geometry: prepared.geometry,
        });
        self.gpu.check()?;
        self.images.submitted();
        self.targets.publish(
            prepared.size,
            prepared.quality,
            &self.inflight.last().unwrap()._targets,
        );
        if let Some(coverage_quality) = self.gpu.coverage_quality(prepared.quality) {
            self.coverage_targets.publish(
                prepared.size,
                coverage_quality,
                &self.inflight.last().unwrap()._targets,
            );
        }
        Ok(id)
    }
}
impl Drop for Renderer {
    fn drop(&mut self) {
        // The queue may own the last references to buffers and images. Keep frame
        // leases and accounting live until it has completed or reported device loss.
        let _ = self.gpu.device.poll(wgpu::PollType::wait_indefinitely());
        self.inflight.clear();
    }
}
#[cfg(test)]
mod tests;
