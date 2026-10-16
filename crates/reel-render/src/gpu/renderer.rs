use super::{
    Gpu, GpuError, Quality,
    pipeline::{CompositeOperation, PipelineSlots},
    replay::Context,
};
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
    targets: Vec<Arc<super::target_cache::Allocation>>,
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
    _targets: Vec<Arc<super::target_cache::Allocation>>,
    _admission: BudgetTicket,
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
}
pub(super) struct Budgets {
    pub(super) targets: BudgetPool,
    pub(super) resources: BudgetPool,
    pub(super) transient: BudgetPool,
}
pub(super) struct Reservation {
    pub(super) _shared: Option<BudgetTicket>,
    pub(super) _local: BudgetTicket,
}
impl Budgets {
    pub(super) fn new(limits: super::GpuLimits) -> Self {
        let pool = |category, max_bytes| {
            BudgetPool::new(
                category,
                QueueLimits {
                    max_requests: u32::MAX,
                    max_bytes,
                },
            )
        };
        Self {
            targets: pool(BudgetCategory::Offscreen, limits.target_bytes),
            resources: pool(BudgetCategory::GpuResource, limits.resource_bytes),
            transient: pool(BudgetCategory::Temporary, limits.transient_bytes),
        }
    }
}
pub(super) fn reserve(
    pool: &BudgetPool,
    local: &BudgetPool,
    bytes: u64,
    limit: u64,
    domain: &'static str,
) -> Result<Reservation, GpuError> {
    // Test the preparation's own live demand first. Other preparations cannot
    // hide a request which already exceeds this preparation's own quota.
    let local = local.reserve(bytes).map_err(|_| GpuError::Budget {
        domain,
        requested: bytes,
        available: limit.saturating_sub(local.usage().bytes),
    })?;
    let shared = pool.reserve(bytes).map_err(|_| GpuError::Pressure {
        domain,
        requested: bytes,
        available: limit.saturating_sub(pool.usage().bytes),
    })?;
    Ok(Reservation {
        _shared: Some(shared),
        _local: local,
    })
}
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
        }
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
        }
    }
    pub fn poll(&mut self) -> Result<Vec<u64>, GpuError> {
        let mut completed = Vec::new();
        self.poll_completed(usize::MAX, |id, _| completed.push(id))?;
        Ok(completed)
    }
    pub fn has_pending(&self) -> bool {
        !self.inflight.is_empty()
    }
    pub(super) fn poll_completed(
        &mut self,
        mut limit: usize,
        mut completed: impl FnMut(u64, &RenderFrame),
    ) -> Result<(), GpuError> {
        self.gpu.check()?;
        self.images.retire(&[]);
        self.glyphs.retire(&[]);
        let result = self.gpu.device.poll(wgpu::PollType::Poll);
        self.gpu.check()?;
        result.map_err(|e| GpuError::Device(e.to_string()))?;
        self.inflight.retain(|s| {
            if limit != 0 && s.done.load(Ordering::Acquire) {
                if s.capture.is_none() {
                    completed(
                        s.id,
                        s._frames.last().expect("submission owns its output frame"),
                    );
                }
                limit -= 1;
                false
            } else {
                true
            }
        });
        Ok(())
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
        self.prepare_frames(frame, None, frame.info().physical_size)
    }
    pub(super) fn prepare_capture(
        &self,
        frame: &RenderFrame,
        size: [u32; 2],
    ) -> Result<Prepared, GpuError> {
        self.prepare_frames(frame, None, size)
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
        self.prepare_frames(from, Some((to, progress)), from.info().physical_size)
    }
    fn prepare_frames(
        &self,
        frame: &RenderFrame,
        second: Option<(&RenderFrame, f32)>,
        size: [u32; 2],
    ) -> Result<Prepared, GpuError> {
        self.gpu.check()?;
        self.images.retire(&[]);
        self.glyphs.retire(&[]);
        let admission = self.admission.reserve(0).map_err(|_| GpuError::Busy)?;
        let budget_scope = Budgets::new(self.gpu.limits);
        let mut tickets = Vec::new();
        let mut plan = |frame| {
            super::targets::peak_targets(frame, size, |bytes| {
                tickets.push(reserve(
                    &self.budgets.transient,
                    &budget_scope.transient,
                    bytes,
                    self.gpu.limits.transient_bytes,
                    "transient",
                )?);
                Ok(())
            })
        };
        let mut targets = plan(frame)?;
        if let Some((second, _)) = second {
            targets = targets.max(1 + plan(second)?).max(3);
        }
        let quality = self.gpu.quality_for_targets(size, targets)?;
        self.targets.configure(size, quality, targets);
        let mut ctx = Context {
            gpu: &self.gpu,
            encoder: self.gpu.device.create_command_encoder(&Default::default()),
            pipelines: self.pipelines.get(&self.gpu, quality)?,
            quality,
            size,
            budgets: &self.budgets,
            budget_scope,
            tickets,
            images: HashMap::new(),
            image_cache: &self.images,
            glyph_cache: &self.glyphs,
            glyphs: Default::default(),
            white: None,
            target_cache: &self.targets,
            target_allocations: Vec::new(),
            available_targets: Vec::new(),
            scissor: super::targets::Scissor::full(size),
        };
        let transform = Affine::scale_non_uniform(
            f64::from(size[0]) / frame.info().logical_size[0],
            f64::from(size[1]) / frame.info().logical_size[1],
        );
        let mut output = ctx.stream(frame, frame.commands(), transform)?;
        let mut frames = vec![frame.clone()];
        if let Some((second, progress)) = second {
            let b = ctx.stream(second, second.commands(), transform)?;
            let mixed = ctx.target()?;
            ctx.combine(
                &mixed,
                &output.view,
                &b.view,
                1.,
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
            targets: ctx.target_allocations,
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
        self.images.publish(&prepared.images);
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
            _targets: prepared.targets,
            _admission: prepared.admission,
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
