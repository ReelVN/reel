//! Isolated clear/paint work using the existing serial effect diagnostic mode.
use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};

/// One interval of successful serial clear or paint operations. These drains
/// change GPU overlap and include submission/notification overhead, not GPU
/// timestamps. Pixel quantities are bounds, not measured shaded fragments.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SerialPassTiming {
    /// Operations completed between drains.
    pub calls: u64,
    /// Sum of logical viewport areas in pixels.
    pub viewport_pixels: u64,
    /// Sum of physical attachment areas in pixels.
    pub storage_pixels: u64,
    /// Wall microseconds draining work queued before each operation.
    pub prior_drain_us: u64,
    /// Wall microseconds issuing each operation, including implicit scene changes.
    pub submit_wall_us: u64,
    /// Wall microseconds completing each operation after submission returns.
    pub completion_drain_us: u64,
    /// Largest observed completion drain in microseconds.
    pub max_completion_drain_us: u64,
}
impl SerialPassTiming {
    const ZERO: Self = Self {
        calls: 0,
        viewport_pixels: 0,
        storage_pixels: 0,
        prior_drain_us: 0,
        submit_wall_us: 0,
        completion_drain_us: 0,
        max_completion_drain_us: 0,
    };
    #[cfg(any(all(feature = "gxm", target_os = "vita"), test))]
    fn merge(&mut self, other: Self) {
        self.calls += other.calls;
        self.viewport_pixels += other.viewport_pixels;
        self.storage_pixels += other.storage_pixels;
        self.prior_drain_us += other.prior_drain_us;
        self.submit_wall_us += other.submit_wall_us;
        self.completion_drain_us += other.completion_drain_us;
        self.max_completion_drain_us = self
            .max_completion_drain_us
            .max(other.max_completion_drain_us);
    }
}
static PASSES: Mutex<[SerialPassTiming; 2]> = Mutex::new([SerialPassTiming::ZERO; 2]);
static DROPPED: AtomicU64 = AtomicU64::new(0);

#[cfg(any(all(feature = "gxm", target_os = "vita"), test))]
pub(crate) fn record_serial_pass(paint: bool, timing: SerialPassTiming) {
    if let Ok(mut passes) = PASSES.try_lock() {
        passes[usize::from(paint)].merge(timing);
    } else {
        DROPPED.fetch_add(1, Ordering::Relaxed);
    }
}

/// Drain fixed clear/paint rows, in that order, and the dropped-record count.
/// A busy producer leaves all records pending and returns `None`. The producer
/// never allocates or waits on this collector; storage holds exactly two rows.
pub fn drain_serial_passes() -> Option<([SerialPassTiming; 2], u64)> {
    let mut passes = PASSES.try_lock().ok()?;
    Some((
        std::mem::take(&mut *passes),
        DROPPED.swap(0, Ordering::Relaxed),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clear_and_paint_stay_separate_and_a_busy_consumer_reports_loss() {
        let held = PASSES.lock().unwrap();
        record_serial_pass(false, SerialPassTiming::default());
        drop(held);
        let sample = SerialPassTiming {
            calls: 1,
            viewport_pixels: 3,
            storage_pixels: 8,
            prior_drain_us: 5,
            submit_wall_us: 7,
            completion_drain_us: 11,
            max_completion_drain_us: 11,
        };
        record_serial_pass(false, sample);
        record_serial_pass(true, sample);
        record_serial_pass(true, sample);
        let (rows, dropped) = drain_serial_passes().unwrap();
        assert_eq!(dropped, 1);
        assert_eq!(rows[0], sample);
        assert_eq!(rows[1].calls, 2);
        assert_eq!(rows[1].storage_pixels, 16);
        assert_eq!(rows[1].completion_drain_us, 22);
        assert_eq!(rows[1].max_completion_drain_us, 11);
        assert_eq!(
            drain_serial_passes().unwrap(),
            ([SerialPassTiming::default(); 2], 0)
        );
    }
}
