//! Host planning diagnostics; attempts can repeat across quality/preparation walks.
#[cfg(any(feature = "wgpu", feature = "gxm"))]
use std::sync::OnceLock;
use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};

#[cfg(any(feature = "wgpu", feature = "gxm"))]
#[derive(Clone, Copy)]
pub(crate) enum CropDecision {
    Boundary,
    Macro,
    Footprint,
    Window,
    Brush,
    Axis,
    Grid,
    Bounds,
    Producer,
    Accepted,
}
static COUNTERS: [AtomicU64; 10] = [const { AtomicU64::new(0) }; 10];
static WINDOWS: Mutex<([[u32; 6]; 8], usize)> = Mutex::new(([[0; 6]; 8], 0));
#[cfg(any(feature = "wgpu", feature = "gxm"))]
fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("REEL_PROFILE_SOURCE_CROP").is_some())
}
#[cfg(any(feature = "wgpu", feature = "gxm"))]
pub(crate) fn record_source_crop_plan(decision: CropDecision) {
    if enabled() {
        COUNTERS[decision as usize].fetch_add(1, Ordering::Relaxed);
    }
}
#[cfg(any(feature = "wgpu", feature = "gxm"))]
pub(crate) fn record_source_crop_window(source: [u32; 2], demand: [u32; 4]) {
    if enabled()
        && let Ok(mut windows) = WINDOWS.try_lock()
    {
        let row = [
            source[0], source[1], demand[0], demand[1], demand[2], demand[3],
        ];
        if windows.1 < 8 && !windows.0[..windows.1].contains(&row) {
            let index = windows.1;
            windows.0[index] = row;
            windows.1 += 1;
        }
    }
}

/// Planning decisions, including repeated walks. These are prospective counts,
/// distinct from successful cropped effect draws in `EffectSample`.
#[derive(Clone, Copy, Debug, Default)]
pub struct SourceCropPlans {
    /// Capture, transition, mask or unsupported extent/surface boundary.
    pub boundary: u64,
    /// Existing eligible macro chain retains its original cells.
    pub macrotile: u64,
    /// Shader lacks a proved ordinary color-source footprint.
    pub footprint: u64,
    /// No smaller dyadic window covers the complete demand.
    pub window: u64,
    /// Source appearance or gradient reads raster position.
    pub brush: u64,
    /// Producer map is not exactly axis aligned.
    pub axis: u64,
    /// Producer corners do not satisfy the current exact raster-grid proof.
    pub grid: u64,
    /// Source geometry lies outside the admitted logical raster.
    pub bounds: u64,
    /// Source is not the supported single raw rectangle producer.
    pub producer: u64,
    /// Planning proof accepted a smaller backing.
    pub accepted: u64,
    /// First distinct rejected windows: source width/height then demand x/y/w/h.
    pub window_examples: [[u32; 6]; 8],
}
/// Drain host planning diagnostics enabled by `REEL_PROFILE_SOURCE_CROP`.
pub fn drain_source_crop_plans() -> SourceCropPlans {
    let n = COUNTERS.each_ref().map(|c| c.swap(0, Ordering::Relaxed));
    let window_examples = std::mem::take(&mut *WINDOWS.lock().unwrap()).0;
    SourceCropPlans {
        boundary: n[0],
        macrotile: n[1],
        footprint: n[2],
        window: n[3],
        brush: n[4],
        axis: n[5],
        grid: n[6],
        bounds: n[7],
        producer: n[8],
        accepted: n[9],
        window_examples,
    }
}
