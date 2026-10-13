use super::{Gpu, GpuError, Renderer, SurfacePass, surface::negotiate};
use crate::*;

/// WGPU presentation adapter. The host supplies a surface; no window system is required.
pub struct WgpuBackend {
    surface: Option<wgpu::Surface<'static>>,
    renderer: Renderer,
    output: Option<SurfacePass>,
    config: Option<wgpu::SurfaceConfiguration>,
    info: Option<SurfaceInfo>,
}

impl WgpuBackend {
    pub fn new(gpu: Gpu, surface: wgpu::Surface<'static>) -> Self {
        Self {
            surface: Some(surface),
            renderer: Renderer::new(gpu),
            output: None,
            config: None,
            info: None,
        }
    }
    pub fn renderer(&self) -> &Renderer {
        &self.renderer
    }
    pub fn renderer_mut(&mut self) -> &mut Renderer {
        &mut self.renderer
    }
    pub fn surface(&self) -> Option<&wgpu::Surface<'static>> {
        self.surface.as_ref()
    }
    pub fn configuration(&self) -> Option<&wgpu::SurfaceConfiguration> {
        self.config.as_ref()
    }
    /// A resumed native surface must be configured before presenting.
    pub fn resume(&mut self, surface: wgpu::Surface<'static>) {
        self.surface = Some(surface);
        self.info = None;
        self.output = None;
        self.config = None;
    }
}

impl RenderBackend for WgpuBackend {
    type Error = GpuError;
    fn capabilities(&self) -> RenderCaps {
        let max = self.renderer.gpu().device.limits().max_texture_dimension_2d;
        RenderCaps {
            consumer: ConsumerKind::Device,
            rasterizes: true,
            formats: &[
                PixelFormat::Rgba8,
                PixelFormat::Bgra8,
                PixelFormat::LinearRgba16Float,
            ],
            effects: &[
                RenderEffect::Groups,
                RenderEffect::Clips,
                RenderEffect::AlphaMask,
                RenderEffect::LuminanceMask,
                RenderEffect::Filters,
            ],
            max_texture_size: max,
            max_offscreen_size: max,
            upload_limits: QueueLimits {
                max_requests: 0,
                max_bytes: 0,
            },
        }
    }
    fn configure(&mut self, info: SurfaceInfo) -> Result<(), GpuError> {
        self.info = None;
        self.output = None;
        self.config = None;
        validate_surface(info)?;
        let surface = self.surface.as_ref().ok_or(GpuError::InvalidSurface)?;
        let (config, output) = negotiate(self.renderer.gpu(), surface, info.physical_size)?;
        self.config = Some(config);
        self.output = Some(output);
        self.info = Some(info);
        Ok(())
    }
    fn upload(&mut self, batch: UploadBatch) -> Result<UploadId, Rejected<UploadBatch>> {
        Err(Rejected {
            request: batch,
            reason: RejectReason::new(
                RejectCode::Unsupported,
                "WGPU uploads resources from the immutable frame during preparation",
            ),
        })
    }
    fn render_present(
        &mut self,
        frame: &RenderFrame,
        before_present: impl FnOnce(),
    ) -> Result<PresentOutcome, GpuError> {
        let Some(surface) = &self.surface else {
            return Ok(PresentOutcome::Skipped(SkipReason::Suspended));
        };
        let info = self.info.ok_or(GpuError::InvalidSurface)?;
        if frame.surface_epoch() != info.surface_epoch
            || frame.info().physical_size != info.physical_size
        {
            return Err(GpuError::WrongSurface);
        }
        if info.physical_size.contains(&0) {
            return Ok(PresentOutcome::SurfaceUnavailable);
        }
        let texture = match surface.get_current_texture() {
            Ok(texture) => texture,
            Err(wgpu::SurfaceError::Timeout) => return Ok(PresentOutcome::SurfaceUnavailable),
            Err(error) => return Err(GpuError::Surface(error)),
        };
        let mut prepared = self.renderer.prepare(frame)?;
        self.output
            .as_ref()
            .expect("configured surface output")
            .encode(
                &self.renderer.gpu().device,
                &mut prepared,
                &texture.texture.create_view(&Default::default()),
            );
        self.renderer.submit(prepared)?;
        before_present();
        texture.present();
        Ok(PresentOutcome::PresentAccepted(frame.frame_id()))
    }
    fn retire(&mut self, resources: &[ResourceVersion]) {
        self.renderer.retire(resources);
    }
    fn poll(&mut self, out: &mut Vec<RenderEvent<GpuError>>, limit: usize) -> Result<(), GpuError> {
        self.renderer.poll_completed(limit, |id, frame| {
            out.push(RenderEvent::RenderFinished {
                id: SubmissionId(id),
                frame: frame.frame_id(),
            })
        })
    }
    fn has_pending(&self) -> bool {
        self.renderer.has_pending()
    }
    fn suspend(&mut self) {
        // Native surface lifetime ends before the host releases its window. Submitted
        // frame leases stay in Renderer until completion is polled or device retirement.
        let _ = self.renderer.gpu().device.poll(wgpu::PollType::Wait);
        self.surface = None;
        self.info = None;
        self.output = None;
        self.config = None;
    }
}

impl Drop for WgpuBackend {
    fn drop(&mut self) {
        self.suspend();
    }
}

fn validate_surface(info: SurfaceInfo) -> Result<(), GpuError> {
    if !info.scale_factor.is_finite()
        || info.scale_factor <= 0.
        || info.surface_epoch.0 == 0
        || info.color_encoding != ColorEncoding::Srgb
        || info.alpha_mode != AlphaMode::Opaque
    {
        return Err(GpuError::InvalidSurface);
    }
    Ok(())
}
