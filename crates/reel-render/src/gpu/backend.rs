use super::{Gpu, GpuError, Renderer, SurfacePass, device::SurfaceFailure, surface::negotiate};
use crate::*;

/// WGPU presentation adapter. The host supplies a surface; no window system is required.
pub struct WgpuBackend {
    surface: Option<wgpu::Surface<'static>>,
    renderer: Renderer,
    output: Option<SurfacePass>,
    config: Option<wgpu::SurfaceConfiguration>,
    info: Option<SurfaceInfo>,
    vsync: bool,
    unsynchronized: Option<wgpu::PresentMode>,
}

impl WgpuBackend {
    pub fn new(gpu: Gpu, surface: wgpu::Surface<'static>) -> Self {
        Self {
            surface: Some(surface),
            renderer: Renderer::new(gpu),
            output: None,
            config: None,
            info: None,
            vsync: true,
            unsynchronized: None,
        }
    }
    pub fn renderer(&self) -> &Renderer {
        &self.renderer
    }
    /// Preserve a user's choice when replacing the device/surface adapter.
    pub fn with_vsync(mut self, enabled: bool) -> Self {
        self.vsync = enabled;
        self
    }
    pub fn vsync_enabled(&self) -> bool {
        self.vsync
    }
    pub fn renderer_mut(&mut self) -> &mut Renderer {
        &mut self.renderer
    }
    /// Whether submitted image copies can unblock preparation of the candidate.
    pub fn has_pending_image_uploads(&self) -> bool {
        self.renderer.has_pending_image_uploads()
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
    fn device_generation(&self) -> u64 {
        self.renderer.gpu().generation().0
    }
    type Error = GpuError;
    fn vsync(&self) -> Option<bool> {
        self.unsynchronized.map(|_| self.vsync)
    }
    fn set_vsync(&mut self, enabled: bool) -> Result<Option<bool>, GpuError> {
        let Some(mode) = self.unsynchronized else {
            return Ok(None);
        };
        if enabled == self.vsync {
            return Ok(Some(enabled));
        }
        let config = self.config.as_mut().ok_or(GpuError::InvalidSurface)?;
        let surface = self.surface.as_ref().ok_or(GpuError::InvalidSurface)?;
        config.present_mode = if enabled {
            wgpu::PresentMode::Fifo
        } else {
            mode
        };
        surface.configure(&self.renderer.gpu().device, config);
        self.renderer.gpu().check()?;
        self.vsync = enabled;
        Ok(Some(enabled))
    }
    fn capabilities(&self) -> RenderCaps {
        let max = self.renderer.gpu().device.limits().max_texture_dimension_2d;
        RenderCaps {
            consumer: ConsumerKind::Device,
            rasterizes: true,
            formats: if self
                .renderer
                .gpu()
                .device
                .features()
                .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
            {
                &[
                    PixelFormat::Rgba8,
                    PixelFormat::Bgra8,
                    PixelFormat::LinearRgba16Float,
                    PixelFormat::Bc1Rgb,
                    PixelFormat::Bc3Rgba,
                ]
            } else {
                &[
                    PixelFormat::Rgba8,
                    PixelFormat::Bgra8,
                    PixelFormat::LinearRgba16Float,
                ]
            },
            effects: &[
                RenderEffect::Groups,
                RenderEffect::Clips,
                RenderEffect::AlphaMask,
                RenderEffect::LuminanceMask,
                RenderEffect::ImageMask,
                RenderEffect::ShaderEffect,
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
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::SurfaceConfigure);
        self.renderer.cancel_preparations();
        self.info = None;
        self.output = None;
        self.config = None;
        validate_surface(info)?;
        let surface = self.surface.as_ref().ok_or(GpuError::InvalidSurface)?;
        let (config, output, unsynchronized) =
            negotiate(self.renderer.gpu(), surface, info.physical_size, self.vsync)?;
        // First configuration happens before the host exposes its initial view.
        // Device recovery gets fresh slots; resize only reuses initialized ones.
        self.renderer.prepare_pipelines()?;
        self.vsync = config.present_mode == wgpu::PresentMode::Fifo;
        self.unsynchronized = unsynchronized;
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
        let mut prepared = match self.renderer.prepare(frame) {
            Err(GpuError::Preparing) => return Ok(PresentOutcome::Preparing),
            result => result?,
        };
        let acquired = {
            let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::SurfaceAcquire);
            surface.get_current_texture()
        };
        let texture = match acquired {
            wgpu::CurrentSurfaceTexture::Success(texture)
            | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => texture,
            status => {
                // Device loss may surface as Validation/Lost. Preserve the
                // device's actual cause before choosing surface-only recovery.
                self.renderer.gpu().check()?;
                return match status {
                    wgpu::CurrentSurfaceTexture::Timeout
                    | wgpu::CurrentSurfaceTexture::Occluded => {
                        Ok(PresentOutcome::SurfaceUnavailable)
                    }
                    wgpu::CurrentSurfaceTexture::Outdated => {
                        Err(GpuError::Surface(SurfaceFailure::Outdated))
                    }
                    wgpu::CurrentSurfaceTexture::Lost => {
                        Err(GpuError::Surface(SurfaceFailure::Lost))
                    }
                    wgpu::CurrentSurfaceTexture::Validation => {
                        Err(GpuError::Surface(SurfaceFailure::Validation))
                    }
                    _ => unreachable!("successful acquisition handled above"),
                };
            }
        };
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
        let present = reel_core::profile::Scope::new(reel_core::profile::Phase::Present);
        self.renderer.gpu().queue.present(texture);
        let completed = self.renderer.gpu().check();
        present.finish_counted(completed.is_ok());
        completed?;
        Ok(PresentOutcome::PresentAccepted(frame.frame_id()))
    }
    fn retire(&mut self, resources: &[ResourceVersion]) {
        self.renderer.retire(resources);
    }
    fn cancel_preparation(&mut self, frame: &RenderFrame) {
        self.renderer.cancel_preparation(frame);
    }
    fn poll(&mut self, out: &mut Vec<RenderEvent<GpuError>>, limit: usize) -> Result<(), GpuError> {
        let resources_available = self.renderer.poll_completed(limit, |id, frame| {
            out.push(RenderEvent::RenderFinished {
                id: SubmissionId(id),
                frame: frame.frame_id(),
            })
        })?;
        if resources_available {
            out.push(RenderEvent::ResourcesAvailable);
        }
        Ok(())
    }
    fn has_pending(&self) -> bool {
        self.renderer.has_pending()
    }
    fn suspend(&mut self) {
        self.renderer.cancel_preparations();
        // Native surface lifetime ends before the host releases its window. Submitted
        // frame leases stay in Renderer until completion is polled or device retirement.
        let _ = self
            .renderer
            .gpu()
            .device
            .poll(wgpu::PollType::wait_indefinitely());
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
