//! Counts for direct interiors, collected without completion waits or clocks.
use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};

/// Work issued by regional paint batches split into disjoint scissors.
/// Areas describe scissor coverage; geometry can cover fewer pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PaintRegions {
    /// Successfully issued original batches that were split.
    pub batches: u64,
    /// Additional draw calls beyond one per original batch.
    pub extra_draws: u64,
    /// Original scissor area, including all direct interiors and borders.
    pub original_pixels: u64,
    /// Scissor area issued with the existing direct sampling program.
    pub direct_pixels: u64,
    /// First nonoverlapping paints issued with replacement after transparent clear.
    pub transparent_replacements: u64,
    /// Scissor area of first paints whose destination blend was removed.
    pub transparent_pixels: u64,
}
static COUNTS: Mutex<PaintRegions> = Mutex::new(PaintRegions {
    batches: 0,
    extra_draws: 0,
    original_pixels: 0,
    direct_pixels: 0,
    transparent_replacements: 0,
    transparent_pixels: 0,
});
static DROPPED: AtomicU64 = AtomicU64::new(0);

#[cfg(any(all(feature = "gxm", target_os = "vita"), test))]
pub(crate) fn record_paint_regions(sample: PaintRegions) {
    if let Ok(mut counts) = COUNTS.try_lock() {
        counts.batches += sample.batches;
        counts.extra_draws += sample.extra_draws;
        counts.original_pixels += sample.original_pixels;
        counts.direct_pixels += sample.direct_pixels;
        counts.transparent_replacements += sample.transparent_replacements;
        counts.transparent_pixels += sample.transparent_pixels;
    } else {
        DROPPED.fetch_add(1, Ordering::Relaxed);
    }
}

/// Drain paint-region counts and the number of dropped observations.
/// Returns `None` while a producer owns the bounded collector.
pub fn drain_paint_regions() -> Option<(PaintRegions, u64)> {
    let mut counts = COUNTS.try_lock().ok()?;
    Some((
        std::mem::take(&mut *counts),
        DROPPED.swap(0, Ordering::Relaxed),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_counts_preserve_areas_and_report_contended_records() {
        let held = COUNTS.lock().unwrap();
        record_paint_regions(PaintRegions::default());
        drop(held);
        let sample = PaintRegions {
            batches: 1,
            extra_draws: 4,
            original_pixels: 100,
            direct_pixels: 80,
            transparent_replacements: 1,
            transparent_pixels: 50,
        };
        record_paint_regions(sample);
        record_paint_regions(sample);
        assert_eq!(
            drain_paint_regions(),
            Some((
                PaintRegions {
                    batches: 2,
                    extra_draws: 8,
                    original_pixels: 200,
                    direct_pixels: 160,
                    transparent_replacements: 2,
                    transparent_pixels: 100,
                },
                1
            ))
        );
        assert_eq!(drain_paint_regions(), Some((PaintRegions::default(), 0)));
    }
}
