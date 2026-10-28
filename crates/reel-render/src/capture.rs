//! Optional raster capture, independent of surfaces and input presentation.
use crate::BackendError;
use reel_core::{
    budget::{BudgetPool, OwnedBytes},
    wake::WakeSignal,
};
use reel_ui::frame::RenderFrame;

/// Color representation of a backend's owned RGBA8 readback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureEncoding {
    /// Display-ready opaque sRGB pixels.
    Srgb,
    /// Linear RGBA8 pixels; output conversion writes opaque sRGB.
    Linear,
}
/// Readback bytes and their exact color representation, retaining the output budget.
pub struct CapturedPixels {
    /// Pixel dimensions in top-left row order.
    pub size: [u32; 2],
    /// Tightly packed RGBA8 pixels in `encoding`.
    pub rgba: OwnedBytes,
    /// Encoding and alpha contract of the owned bytes.
    pub encoding: CaptureEncoding,
}
/// Pixel conversion stopped before producing a complete result.
#[derive(Debug)]
pub struct CaptureCancelled;
impl std::fmt::Display for CaptureCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("capture conversion cancelled")
    }
}
impl std::error::Error for CaptureCancelled {}
impl CapturedPixels {
    /// Convert in place to display-ready sRGB, checking cancellation between chunks.
    /// Cancelling releases the input allocation and its reservation.
    pub fn into_srgb(self, mut cancelled: impl FnMut() -> bool) -> Result<Self, CaptureCancelled> {
        if self.encoding == CaptureEncoding::Srgb {
            return if cancelled() {
                Err(CaptureCancelled)
            } else {
                Ok(self)
            };
        }
        let Self { size, rgba, .. } = self;
        let (mut bytes, ticket) = rgba.into_parts();
        if !crate::capture_color::encode_rgba_cancellable(&mut bytes, cancelled) {
            return Err(CaptureCancelled);
        }
        let rgba = OwnedBytes::new(bytes, ticket).expect("capture conversion preserves capacity");
        Ok(Self {
            size,
            rgba,
            encoding: CaptureEncoding::Srgb,
        })
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use reel_core::budget::{BudgetCategory, QueueLimits};

    fn pool() -> BudgetPool {
        BudgetPool::new(
            BudgetCategory::Temporary,
            QueueLimits {
                max_requests: 8,
                max_bytes: 1 << 20,
            },
        )
    }
    fn pixels(pool: &BudgetPool, bytes: Vec<u8>, encoding: CaptureEncoding) -> CapturedPixels {
        let ticket = pool.reserve(bytes.capacity() as u64).unwrap();
        CapturedPixels {
            size: [(bytes.len() / 4) as u32, 1],
            rgba: OwnedBytes::new(bytes, ticket).unwrap(),
            encoding,
        }
    }
    #[test]
    fn worker_conversion_preserves_storage_and_existing_srgb() {
        let pool = pool();
        let input = (0..=255)
            .flat_map(|i| [i, 255 - i, i ^ 0x55, i])
            .collect::<Vec<u8>>();
        let pixels = pixels(&pool, input.clone(), CaptureEncoding::Linear);
        let ptr = pixels.rgba.as_slice().as_ptr();
        let pixels = pixels.into_srgb(|| false).unwrap();
        assert_eq!(pixels.encoding, CaptureEncoding::Srgb);
        assert_eq!(pixels.rgba.as_slice().as_ptr(), ptr);
        let mut expected = input;
        assert!(crate::capture_color::encode_rgba_cancellable(
            &mut expected,
            || false
        ));
        assert_eq!(pixels.rgba.as_slice(), expected);
        let unchanged = pixels.into_srgb(|| false).unwrap();
        assert_eq!(unchanged.rgba.as_slice().as_ptr(), ptr);
        assert_eq!(unchanged.rgba.as_slice(), expected);
        drop(unchanged);
        assert_eq!(pool.usage().bytes, 0);
    }
    #[test]
    fn cancelled_conversion_releases_the_existing_reservation() {
        let pool = pool();
        let pixels = pixels(&pool, vec![127; 32 * 1024], CaptureEncoding::Linear);
        let mut polls = 0;
        assert!(
            pixels
                .into_srgb(|| {
                    polls += 1;
                    polls >= 3
                })
                .is_err()
        );
        assert_eq!(polls, 3);
        assert_eq!(pool.usage().bytes, 0);
    }
}
