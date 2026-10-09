use super::{Gpu, GpuError, Quality, pipeline::Pipelines, replay::Context};
use masonry_core::kurbo::Affine;
use reel_core::budget::{BudgetCategory, BudgetPool, BudgetTicket, QueueLimits};
use reel_stage::RenderFrame;
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
    pub(super) encoder: wgpu::CommandEncoder,
    pub(super) output: wgpu::TextureView,
    pub(super) texture: wgpu::Texture,
    pub(super) quality: Quality,
    pub(super) size: [u32; 2],
    frames: Vec<RenderFrame>,
    tickets: Vec<BudgetTicket>,
    admission: BudgetTicket,
    generation: super::DeviceGeneration,
    owner: Arc<()>,
}
impl Prepared {
    pub fn quality(&self) -> Quality {
        self.quality
    }
    pub fn size(&self) -> [u32; 2] {
        self.size
    }
}
struct Inflight {
    id: u64,
    done: Arc<AtomicBool>,
    _frames: Vec<RenderFrame>,
    _tickets: Vec<BudgetTicket>,
    _admission: BudgetTicket,
}
pub struct Renderer {
    pub(super) gpu: Gpu,
    inflight: Vec<Inflight>,
    next: u64,
    budgets: Budgets,
    admission: BudgetPool,
    owner: Arc<()>,
}
pub(super) struct Budgets {
    pub(super) targets: BudgetPool,
    pub(super) resources: BudgetPool,
    pub(super) transient: BudgetPool,
}
pub(super) fn reserve(
    pool: &BudgetPool,
    bytes: u64,
    limit: u64,
    domain: &'static str,
) -> Result<BudgetTicket, GpuError> {
    pool.reserve(bytes).map_err(|_| GpuError::Budget {
        domain,
        requested: bytes,
        available: limit.saturating_sub(pool.usage().bytes),
    })
}
impl Renderer {
    pub fn gpu(&self) -> &Gpu {
        &self.gpu
    }
    pub fn new(gpu: Gpu) -> Self {
        let pool = |category, max_bytes| {
            BudgetPool::new(
                category,
                QueueLimits {
                    max_requests: 65_536,
                    max_bytes,
                },
            )
        };
        let budgets = Budgets {
            targets: pool(BudgetCategory::Offscreen, gpu.limits.target_bytes),
            resources: pool(BudgetCategory::GpuResource, gpu.limits.resource_bytes),
            transient: pool(BudgetCategory::Temporary, gpu.limits.transient_bytes),
        };
        let admission = BudgetPool::new(
            BudgetCategory::Temporary,
            QueueLimits {
                max_requests: gpu.limits.max_inflight.min(u32::MAX as usize) as u32,
                max_bytes: 0,
            },
        );
        Self {
            gpu,
            inflight: Vec::new(),
            next: 1,
            budgets,
            admission,
            owner: Arc::new(()),
        }
    }
    pub(super) fn reserve_readback(&self, bytes: u64) -> Result<BudgetTicket, GpuError> {
        reserve(
            &self.budgets.transient,
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
        self.gpu.check()?;
        let result = self.gpu.device.poll(wgpu::PollType::Poll);
        self.gpu.check()?;
        result.map_err(|e| GpuError::Device(e.to_string()))?;
        let mut completed = Vec::new();
        self.inflight.retain(|s| {
            if s.done.load(Ordering::Acquire) {
                completed.push(s.id);
                false
            } else {
                true
            }
        });
        Ok(completed)
    }
    pub fn prepare(&self, frame: &RenderFrame) -> Result<Prepared, GpuError> {
        self.prepare_frames(frame, None)
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
        self.prepare_frames(from, Some((to, progress)))
    }
    fn prepare_frames(
        &self,
        frame: &RenderFrame,
        second: Option<(&RenderFrame, f32)>,
    ) -> Result<Prepared, GpuError> {
        self.gpu.check()?;
        let admission = self.admission.reserve(0).map_err(|_| GpuError::Busy)?;
        let size = frame.info().physical_size;
        let quality = self.gpu.quality(size)?;
        let mut ctx = Context {
            gpu: &self.gpu,
            encoder: self.gpu.device.create_command_encoder(&Default::default()),
            pipelines: Pipelines::new(&self.gpu.device, quality),
            quality,
            size,
            budgets: &self.budgets,
            tickets: Vec::new(),
            images: HashMap::new(),
        };
        let transform = Affine::scale_non_uniform(
            f64::from(size[0]) / frame.info().logical_size[0],
            f64::from(size[1]) / frame.info().logical_size[1],
        );
        let mut output = ctx.stream(frame, frame.commands(), transform, 0)?;
        let mut frames = vec![frame.clone()];
        if let Some((second, progress)) = second {
            let b = ctx.stream(second, second.commands(), transform, 0)?;
            let mixed = ctx.target()?;
            ctx.combine(&mixed, &output.view, &b.view, 1., 3, progress)?;
            output = mixed;
            frames.push(second.clone());
        }
        self.gpu.check()?;
        Ok(Prepared {
            encoder: ctx.encoder,
            output: output.view,
            texture: output.texture,
            quality,
            size,
            frames,
            tickets: ctx.tickets,
            admission,
            generation: self.gpu.generation,
            owner: self.owner.clone(),
        })
    }
    pub(super) fn validate_prepared(&self, prepared: &Prepared) -> Result<(), GpuError> {
        self.gpu.check()?;
        if !Arc::ptr_eq(&prepared.owner, &self.owner) || prepared.generation != self.gpu.generation
        {
            return Err(GpuError::DeviceLost);
        }
        Ok(())
    }
    pub fn submit(&mut self, prepared: Prepared) -> Result<u64, GpuError> {
        self.gpu.check()?;
        if !Arc::ptr_eq(&prepared.owner, &self.owner) || prepared.generation != self.gpu.generation
        {
            return Err(GpuError::DeviceLost);
        }
        if self.inflight.len() >= self.gpu.limits.max_inflight {
            return Err(GpuError::Busy);
        }
        let id = self.next;
        self.next = id.checked_add(1).ok_or(GpuError::IdentityExhausted)?;
        let done = Arc::new(AtomicBool::new(false));
        self.gpu.queue.submit([prepared.encoder.finish()]);
        let signal = done.clone();
        self.gpu
            .queue
            .on_submitted_work_done(move || signal.store(true, Ordering::Release));
        self.inflight.push(Inflight {
            id,
            done,
            _frames: prepared.frames,
            _tickets: prepared.tickets,
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
        let _ = self.gpu.device.poll(wgpu::PollType::Wait);
        self.inflight.clear();
    }
}
