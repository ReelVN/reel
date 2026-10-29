//! Linear pixel readback shared by the device color oracles.
use super::{DeviceBackend, Result, r01_fixture};
use reel_core::wake::WakeSignal;
use reel_render::{
    RenderBackend,
    capture::{CaptureBackend, CaptureEncoding},
};
use reel_ui::frame::RenderFrame;
use std::time::{Duration, Instant};

pub(super) fn capture(
    renderer: &mut DeviceBackend,
    frame: RenderFrame,
    size: [u32; 2],
) -> Result<Vec<u8>> {
    let pool = r01_fixture::pool();
    let mut capture = renderer.capture(&frame, size, &pool, WakeSignal::new(|| {}))?;
    let start = Instant::now();
    let pixels = loop {
        if let Some(pixels) = renderer.poll_capture(&mut capture, 64 << 10)? {
            break pixels;
        }
        if start.elapsed() > Duration::from_secs(15) {
            return Err("color capture timeout".into());
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    if pixels.encoding != CaptureEncoding::Linear {
        return Err("expected linear GXM readback".into());
    }
    let result = pixels.rgba.as_slice().to_vec();
    drop((pixels, capture, frame));
    renderer.poll(&mut Vec::new(), 32)?;
    Ok(result)
}
