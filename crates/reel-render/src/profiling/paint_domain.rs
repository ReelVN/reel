//! Prospective GXM paint admission observed through ordinary host preparation.
#[cfg(feature = "wgpu")]
use std::sync::OnceLock;
use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};

/// Input coverage of the direct-interior proof. This reports proposed scissors,
/// not submitted work or a GPU timing. Input categories may overlap.
#[derive(Clone, Copy, Debug, Default)]
pub struct PaintDomain {
    /// Regional paint inputs examined after the existing whole-draw proof.
    pub candidates: u64,
    /// Inputs with one four-vertex, six-index rectangle.
    pub single_quads: u64,
    /// Inputs with mip chains, excluded by the current proof.
    pub mipmapped: u64,
    /// Original scissors smaller than the current admission threshold.
    pub small_scissors: u64,
    /// All source mesh vertices examined, retaining the ordinary coverage mesh.
    pub vertices: u64,
    /// Candidate scissor area before proposing an interior.
    pub original_pixels: u64,
    /// Inputs admitted by the complete per-triangle domain proof.
    pub admitted: u64,
    /// Scissor area proposed for direct sampling.
    pub direct_pixels: u64,
    /// First single-quad paints on a target that still needs transparent clear.
    pub transparent_quads: u64,
    /// Scissor area of those first paints; geometry may cover fewer pixels.
    pub transparent_pixels: u64,
    /// Observations lost to a simultaneous collector drain.
    pub dropped: u64,
}
static COUNTS: Mutex<Option<PaintDomain>> = Mutex::new(None);
static DROPPED: AtomicU64 = AtomicU64::new(0);

#[cfg(feature = "wgpu")]
pub(crate) fn paint_domain_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("REEL_PROFILE_PAINT_DOMAIN").is_some())
}

#[cfg(feature = "wgpu")]
pub(crate) fn record_paint_domain(row: PaintDomain) {
    if let Ok(mut slot) = COUNTS.try_lock() {
        let total = slot.get_or_insert_with(PaintDomain::default);
        total.candidates += row.candidates;
        total.single_quads += row.single_quads;
        total.mipmapped += row.mipmapped;
        total.small_scissors += row.small_scissors;
        total.vertices += row.vertices;
        total.original_pixels += row.original_pixels;
        total.admitted += row.admitted;
        total.direct_pixels += row.direct_pixels;
        total.transparent_quads += row.transparent_quads;
        total.transparent_pixels += row.transparent_pixels;
    } else {
        DROPPED.fetch_add(1, Ordering::Relaxed);
    }
}

/// Drain prospective paint admission from host profiling. Enable with
/// `REEL_PROFILE_PAINT_DOMAIN` before ordinary WGPU preparation. Host rendering
/// retains its existing draws; these observations establish input coverage only.
pub fn drain_paint_domain() -> Option<PaintDomain> {
    let mut counts = COUNTS.try_lock().ok()?;
    let dropped = DROPPED.swap(0, Ordering::Relaxed);
    let mut row = counts
        .take()
        .or_else(|| (dropped != 0).then(PaintDomain::default))?;
    row.dropped = dropped;
    Some(row)
}
