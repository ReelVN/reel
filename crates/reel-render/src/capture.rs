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

/// Capture requests use the same borrowed device as presentation.
impl<B: CaptureBackend + ?Sized> CaptureBackend for &mut B {
    type Error = B::Error;
    type Capture = B::Capture;
    fn capture(
        &mut self,
        frame: &RenderFrame,
        size: [u32; 2],
        output: &BudgetPool,
        wake: WakeSignal,
    ) -> Result<Self::Capture, Self::Error> {
        (**self).capture(frame, size, output, wake)
    }
    fn poll_capture(
        &mut self,
        capture: &mut Self::Capture,
    ) -> Result<Option<CapturedPixels>, Self::Error> {
        (**self).poll_capture(capture)
    }
}
