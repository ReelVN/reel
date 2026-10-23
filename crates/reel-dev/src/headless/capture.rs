//! Author-requested PNG capture through the production offscreen renderer.

use reel_core::{budget::BudgetPool, wake::WakeSignal};
use reel_player::{aside_session::AsideSessionUi, capture::CaptureService};
use reel_render::{
    capture::{CaptureBackend, CapturedPixels},
    gpu::{DeviceGeneration, Gpu, GpuCapture, GpuError, GpuLimits, Renderer},
};
use reel_ui::frame::RenderFrame;

const CAPTURE_BYTES: u64 = 128 << 20;

pub(super) struct CaptureProgress {
    pub changed: bool,
    pub busy: bool,
}

pub(super) struct FrameCapture {
    backend: Offscreen,
    service: CaptureService<GpuCapture>,
    wake: WakeSignal,
}

// Device creation belongs to actual readback admission. The production service
// can reject a request without an accepted frame before touching a GPU adapter.
struct Offscreen {
    renderer: Option<Renderer>,
}

impl CaptureBackend for Offscreen {
    type Error = GpuError;
    type Capture = GpuCapture;

    fn capture(
        &mut self,
        frame: &RenderFrame,
        size: [u32; 2],
        output: &BudgetPool,
        wake: WakeSignal,
    ) -> Result<GpuCapture, GpuError> {
        if self.renderer.is_none() {
            let gpu = pollster::block_on(Gpu::request(
                &wgpu::Instance::default(),
                None,
                DeviceGeneration(1),
                GpuLimits::default(),
            ))?;
            self.renderer = Some(Renderer::new(gpu));
        }
        self.renderer
            .as_mut()
            .unwrap()
            .capture(frame, size, output, wake)
    }

    fn poll_capture(
        &mut self,
        capture: &mut GpuCapture,
    ) -> Result<Option<CapturedPixels>, GpuError> {
        self.renderer.as_mut().unwrap().poll_capture(capture)
    }
}

impl FrameCapture {
    pub fn new(wake: WakeSignal) -> Self {
        Self {
            backend: Offscreen { renderer: None },
            service: CaptureService::new(CAPTURE_BYTES, wake.clone()),
            wake,
        }
    }

    pub fn wake(&self) -> WakeSignal {
        self.wake.clone()
    }

    pub fn drive(
        &mut self,
        driver: &mut AsideSessionUi,
        accepted: Option<&RenderFrame>,
    ) -> Result<CaptureProgress, String> {
        let pending_gpu = self
            .backend
            .renderer
            .as_ref()
            .is_some_and(Renderer::has_pending);
        if !self.service.busy() && !pending_gpu && !driver.adapter().has_capture_requests() {
            return Ok(CaptureProgress {
                changed: false,
                busy: false,
            });
        }
        if let Some(renderer) = &mut self.backend.renderer {
            renderer.poll().map_err(|error| error.to_string())?;
        }
        let changed = self
            .service
            .drive(&mut self.backend, driver, accepted)
            .map_err(|error| error.to_string())?;
        Ok(CaptureProgress {
            changed,
            busy: self.service.busy()
                || self
                    .backend
                    .renderer
                    .as_ref()
                    .is_some_and(Renderer::has_pending),
        })
    }

    /// Keep virtual story time fixed while GPU readback or PNG encoding completes.
    pub fn wait(&mut self) -> Result<(), String> {
        if let Some(renderer) = &mut self.backend.renderer
            && renderer.has_pending()
        {
            renderer
                .gpu()
                .device
                .poll(wgpu::PollType::wait_indefinitely())
                .map_err(|error| error.to_string())?;
            renderer.poll().map_err(|error| error.to_string())?;
        } else if self.service.busy() && !self.wake.acknowledge() {
            std::thread::park();
        }
        Ok(())
    }

    /// Cancel old request/encoding ownership and release completed GPU leases.
    /// The device and bounded renderer caches remain available to the next session.
    pub fn reset(&mut self) -> Result<(), String> {
        self.service = CaptureService::new(CAPTURE_BYTES, self.wake.clone());
        if let Some(renderer) = &mut self.backend.renderer {
            if renderer.has_pending() {
                renderer
                    .gpu()
                    .device
                    .poll(wgpu::PollType::wait_indefinitely())
                    .map_err(|error| error.to_string())?;
            }
            renderer.poll().map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::headless::{RunOptions, RunOutcome, run};

    #[test]
    #[ignore = "requires an actual Vulkan/Metal/DX12 adapter; run explicitly"]
    fn authored_capture_round_trips_through_save_thumbnails() {
        let story = reel_engine::aside::source::compile_story(
            r##"
            fn main() {
                game.configure(width:32, height:24);
                stage.replace(stack(width:32, height:24, background:"#336699"));
                await stage.presented();
                let preview = await stage.capture(width:16);
                await saves.save("preview", thumbnail:preview);
                let slots = await saves.list();
                if slots.len() != 1 || !slots[0].has_thumbnail { throw "capture thumbnail missing"; }
                let restored = await saves.thumbnail("preview");
                if restored != preview { throw "capture thumbnail changed"; }
                stage.replace(image(restored, width:32, height:24));
                await stage.presented();
            }
            "##,
            "capture_test",
            1,
            "main",
        )
        .expect("compile authored capture");
        let resources = tempfile::tempdir().unwrap();
        let report = run(story, resources.path(), &RunOptions::default(), &[]).unwrap();
        assert_eq!(report.outcome, RunOutcome::Finished, "{report:?}");
    }
}
