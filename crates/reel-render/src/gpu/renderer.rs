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
struct Inflight {
    capture: Option<Arc<super::capture::ReadbackStorage>>,
    id: u64,
    done: Arc<AtomicBool>,
    _frames: Vec<RenderFrame>,
    _tickets: Vec<Reservation>,
    _images: HashMap<reel_ui::resources::ResourceHandle, Arc<super::resources::Image>>,
    _glyphs: super::glyph_cache::FrameGlyphs,
    _effects: Vec<Arc<super::effect::Compiled>>,
    _targets: Vec<Arc<super::target_cache::Allocation>>,
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
        !self.inflight.is_empty() || self.has_pending_image_uploads()
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
        let resources_available = limit != 0 && self.images.poll();
        if resources_available {
            limit -= 1;
        }
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
    pub fn prepare(&self, frame: &RenderFrame) -> Result<Prepared, GpuError> {
        self.prepare_frames(frame, None, frame.info().physical_size, false)
    }
    pub(super) fn prepare_capture(
        &self,
        frame: &RenderFrame,
        size: [u32; 2],
    ) -> Result<Prepared, GpuError> {
        self.prepare_frames(frame, None, size, true)
    }
    /// Both immutable input frames stay leased through queue completion. No input acceptance.
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
        // Capture reserves its output and readback after replay. Keep that
        // original admission path free of optional reusable staging.
        if capture {
            self.geometry.clear();
        }
        let policy = super::geometry::UploadPolicy::new(!capture);
        let result = self.prepare_cached(frame, second, size, capture, &policy);
        let fallback = policy.staged.get()
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
            if fallback || self.budgets.transient.usage().bytes < resident {
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
    ) -> Result<Prepared, GpuError> {
        self.gpu.check()?;
        self.images.retire(&[]);
        self.glyphs.retire(&[]);
        let budget_scope = Budgets::new(self.gpu.limits);
        let mut tickets = Vec::new();
        let plan = super::image_plan::collect(
            std::iter::once(frame).chain(second.map(|(frame, _)| frame)),
            self.gpu.device.limits().max_texture_dimension_2d,
            capture,
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
        let mut previous_plan = None;
        let (quality, _) = self.gpu.quality_for_targets(size, |samples| {
            if let Some((previous_samples, targets)) = previous_plan
                && previous_samples == samples
            {
                return Ok(targets);
            }
            // Planning storage is released when the traversal finishes. Candidate
            // sample counts can need different clip targets and must be admitted
            // using the same coverage policy as replay.
            let mut plan_tickets = Vec::new();
            let targets = super::targets::target_pixels(
                frame,
                second.map(|(frame, _)| frame),
                size,
                capture,
                samples,
                self.gpu.device.limits().max_texture_dimension_2d,
                &glyph_plan,
                |bytes| {
                    plan_tickets.push(reserve(
                        &self.budgets.transient,
                        &budget_scope.transient,
                        bytes,
                        self.gpu.limits.transient_bytes,
                        "transient",
                    )?);
                    Ok(())
                },
            )?;
            previous_plan = Some((samples, targets));
            Ok(targets)
        })?;
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
            effects: Vec::new(),
            white: None,
            target_cache: &self.targets,
            target_allocations: Vec::new(),
            available_targets: Vec::new(),
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
        let mut frames = vec![frame.clone()];
        if let Some((second, progress)) = second {
            let b = ctx.stream(second.drawing(), transform)?;
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
            frames.push(second.clone());
        }
        self.gpu.check()?;
        Ok(Prepared {
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
            geometry: ctx.geometry,
            budget_scope: ctx.budget_scope,
            admission,
            generation: self.gpu.generation,
            owner: self.owner.clone(),
        })
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
#[path = "white_texture_tests.rs"]
mod white_texture_tests;
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_demand_and_shared_pressure_release_independently() {
        let limits = super::super::GpuLimits {
            target_bytes: 10,
            ..Default::default()
        };
        let shared = Budgets::new(limits);
        let first = Budgets::new(limits);
        let second = Budgets::new(limits);
        let old = reserve(&shared.targets, &first.targets, 8, 10, "targets").unwrap();
        assert!(matches!(
            reserve(&shared.targets, &second.targets, 8, 10, "targets"),
            Err(GpuError::Pressure { available: 2, .. })
        ));
        assert_eq!(second.targets.usage().bytes, 0);
        assert!(matches!(
            reserve(&shared.targets, &second.targets, 11, 10, "targets"),
            Err(GpuError::Budget { available: 10, .. })
        ));
        drop(old);
        let target = reserve(&shared.targets, &second.targets, 8, 10, "targets").unwrap();
        assert!(matches!(
            reserve(&shared.targets, &second.targets, 3, 10, "targets"),
            Err(GpuError::Budget { available: 2, .. })
        ));
        drop(target);
        assert_eq!(shared.targets.usage().bytes, 0);
        assert_eq!(second.targets.usage().bytes, 0);
    }
}
