//! Optional raster capture, independent of surfaces and input presentation.
use crate::BackendError;
use reel_core::{
    budget::{BudgetPool, OwnedBytes},
    wake::WakeSignal,
};
use reel_ui::frame::RenderFrame;

pub struct CapturedPixels {
    pub size: [u32; 2],
    pub rgba: OwnedBytes,
}
pub trait CaptureBackend {
    type Error: BackendError + 'static;
    type Capture;
    fn capture(
        &mut self,
        frame: &RenderFrame,
        size: [u32; 2],
        output: &BudgetPool,
        wake: WakeSignal,
    ) -> Result<Self::Capture, Self::Error>;
    fn poll_capture(
        &mut self,
        capture: &mut Self::Capture,
    ) -> Result<Option<CapturedPixels>, Self::Error>;
}
