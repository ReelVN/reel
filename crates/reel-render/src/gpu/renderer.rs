use super::{
    Gpu, GpuError, Quality,
    pipeline::{CompositeOperation, PipelineSlots},
    replay::Context,
};
#[cfg(test)]
#[path = "texture_tests.rs"]
mod texture_tests;
use reel_core::budget::{BudgetCategory, BudgetPool, BudgetTicket, QueueLimits};
use reel_ui::frame::RenderFrame;
use reel_ui::kurbo::Affine;
use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Copy, Debug, Default)]
pub struct Usage {
    pub targets: u64,
    pub resources: u64,
    pub transient: u64,
    /// Completed geometry, staging and allocation metadata retained as an
    /// evictable subset of `transient`; in-flight storage is excluded.
    pub cached_geometry: u64,
}
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
    _admission: BudgetTicket,
    geometry: Vec<super::geometry::Storage>,
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
    geometry: super::geometry::Cache,
    retained: super::retained::Cache,
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
            geometry: Default::default(),
            retained: Default::default(),
        }
    }
    /// Prepare the device's supported quality tiers and built-in image masks
    /// before showing the first frame. Repeated calls reuse the same slots.
    /// This creates no frame targets or decoded image buffers.
    pub fn prepare_pipelines(&self) -> Result<(), GpuError> {
        for quality in self.gpu.supported_qualities() {
            self.pipelines
                .get(&self.gpu, quality)?
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
    pub fn usage(&self) -> Usage {
        Usage {
            targets: self.budgets.targets.usage().bytes,
            resources: self.budgets.resources.usage().bytes,
            transient: self.budgets.transient.usage().bytes,
            cached_geometry: self.geometry.bytes(),
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
                self.geometry.recycle(&mut s.geometry);
                limit -= 1;
                false
            } else {
                true
            }
        });
        self.images.retire(&[]);
        self.glyphs.retire(&[]);
        self.pipelines.prune_masks();
        Ok(resources_available)
    }
    pub(super) fn retire(&self, resources: &[reel_core::identity::ResourceVersion]) {
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
        let mut epoch = self.retained.epoch.lock().unwrap();
        if *epoch != Some(frame.surface_epoch()) {
            self.retained.inputs.clear();
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
        }
        let policy = super::geometry::UploadPolicy::new(!capture);
        let result = self.prepare_cached(frame, second, size, capture, &policy, false);
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
            let resident = self.budgets.transient.usage().bytes;
            self.geometry.clear();
            let released_uploads = self.images.clear_idle();
            if fallback
                || self.budgets.transient.usage().bytes < resident
                || released_uploads
                || matches!(result, Err(GpuError::Pressure { .. }))
                    && self.retained.inputs.quality().is_some()
            {
                #[cfg(test)]
                self.gpu
                    .upload_creations
                    .geometry_retries
                    .fetch_add(1, Ordering::Relaxed);
                // The failed encoder is already dropped. Geometry uploads only
                // belong to that encoder, so immutable replay cannot publish it.
                return self.prepare_cached(
                    frame,
                    second,
                    size,
                    capture,
                    &super::geometry::UploadPolicy::new(!capture && !fallback),
                    matches!(result, Err(GpuError::Pressure { .. })),
                );
            }
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
        discard_unused: bool,
    ) -> Result<Prepared, GpuError> {
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
            if discard_unused && selection.has_cached() {
                self.images.retain_demand(&plan.images);
                self.geometry.glyphs.lock().unwrap().clear();
                self.glyphs.prune_idle();
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
            let images = self.images.prepare(
                &self.gpu,
                &plan.images,
                &self.budgets,
                &budget_scope,
                &self.admission,
                &self.glyphs,
            )?;
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
            let mut offscreen = crate::common::offscreen::Plan::default();
            let mut offscreen_tickets = Vec::new();
            let mut target_plan = |quality: Quality| {
                offscreen = Default::default();
                offscreen_tickets.clear();
                let (schedule, targets, retained) = crate::common::offscreen::Plan::choose(
                    frame,
                    second.map(|(frame, _)| frame),
                    size,
                    capture,
                    quality.samples,
                    self.gpu.device.limits().max_texture_dimension_2d,
                    &glyph_plan,
                    self.gpu.limits.target_bytes / super::targets::target_bytes([1, 1], quality)?,
                    crate::common::target_plan::MaskExecution::Inline,
                    &selection,
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
                offscreen_tickets = retained;
                Ok(targets)
            };
            let quality = if let Some(quality) = preferred {
                let pixels = target_plan(quality)?;
                let bytes = pixels.saturating_mul(super::targets::target_bytes([1, 1], quality)?);
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
                            return Ok(self.gpu.limits.target_bytes
                                / super::targets::target_bytes([1, 1], quality)?
                                + 1);
                        }
                        target_plan(quality)
                    })?
                    .0
            };
            let pipelines = self.pipelines.get(&self.gpu, quality)?;
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
                retained_cache: &self.retained.inputs,
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
                geometry_policy,
                geometry_cache: &self.geometry,
                geometry: Vec::new(),
            };
            let transform = Affine::scale_non_uniform(
                f64::from(size[0]) / frame.info().logical_size[0],
                f64::from(size[1]) / frame.info().logical_size[1],
            );
            let mut output = ctx.stream(frame.drawing(), transform)?;
            ctx.offscreen = Default::default();
            drop(offscreen_tickets);
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
                self.retained.inputs.stage(
                    token,
                    allocation,
                    u64::from(output.size[0]) * u64::from(output.size[1]),
                );
                Some(token)
            } else {
                None
            };
            let prepared = Prepared {
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
                geometry: ctx.geometry,
                budget_scope: ctx.budget_scope,
                admission,
                generation: self.gpu.generation,
                owner: self.owner.clone(),
            };
            if let Some(token) = retained {
                let done = Arc::new(AtomicBool::new(false));
                self.gpu.queue.submit([prepared.encoder.finish()]);
                let signal = done.clone();
                self.gpu
                    .queue
                    .on_submitted_work_done(move || signal.store(true, Ordering::Release));
                self.targets.clear();
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
    pub fn submit(&mut self, prepared: Prepared) -> Result<u64, GpuError> {
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
        self.gpu.queue.submit([prepared.encoder.finish()]);
        self.targets
            .publish(prepared.size, prepared.quality, &prepared.targets);
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
            _admission: prepared.admission,
            geometry: prepared.geometry,
        });
        self.gpu.check()?;
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
#[path = "pipeline_tests.rs"]
mod pipeline_tests;
#[cfg(test)]
mod tests;
