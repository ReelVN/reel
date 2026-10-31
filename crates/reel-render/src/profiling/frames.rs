//! Whole-frame completion observations without splitting drawing passes.
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

static ENABLED: AtomicBool = AtomicBool::new(false);
static TIMING: Mutex<FrameCompletionTiming> = Mutex::new(FrameCompletionTiming::ZERO);
static DROPPED: AtomicU64 = AtomicU64::new(0);

/// Enable GXM's diagnostic wait for the submitted frame's final fragment fence,
/// before display enqueue. Disabled by default. Pass batching stays intact, but
/// waiting reduces CPU/GPU and inter-frame overlap. Earlier in-flight work may
/// contribute. These CPU wall intervals are not GPU hardware timestamps.
pub fn set_frame_completion(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

/// Bounded interval totals for final-fragment completion observations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameCompletionTiming {
    /// Frames whose final fragment completion was observed before the deadline.
    pub completed: u64,
    /// Waits that reached the two-second deadline; excluded from successful sums.
    pub timeouts: u64,
    /// Native wait errors; excluded from successful sums.
    pub errors: u64,
    /// Successful wall spans from render preparation start to observed completion.
    /// Includes CPU preparation/submission, driver waits and earlier queued work.
    pub prepare_to_completion_wall_us: u64,
    /// Successful wall spans from submit return to observed completion, including
    /// notification polling and scheduling overhead; excludes display enqueue.
    pub submit_tail_wait_wall_us: u64,
    /// Largest successful preparation-to-completion wall span.
    pub max_prepare_to_completion_wall_us: u64,
    /// Largest successful submit-tail wall span.
    pub max_submit_tail_wait_wall_us: u64,
    /// Wall time spent in timed-out or failed observations, including preparation.
    pub failed_prepare_to_observation_wall_us: u64,
    /// Wall time after submit return in timed-out or failed observations.
    pub failed_submit_tail_wait_wall_us: u64,
}
impl FrameCompletionTiming {
    const ZERO: Self = Self {
        completed: 0,
        timeouts: 0,
        errors: 0,
        prepare_to_completion_wall_us: 0,
        submit_tail_wait_wall_us: 0,
        max_prepare_to_completion_wall_us: 0,
        max_submit_tail_wait_wall_us: 0,
        failed_prepare_to_observation_wall_us: 0,
        failed_submit_tail_wait_wall_us: 0,
    };
    #[cfg(any(all(feature = "gxm", target_os = "vita"), test))]
    fn observe(&mut self, span: u64, tail: u64, result: Result<bool, ()>) {
        match result {
            Ok(true) => {
                self.completed += 1;
                self.prepare_to_completion_wall_us += span;
                self.submit_tail_wait_wall_us += tail;
                self.max_prepare_to_completion_wall_us =
                    self.max_prepare_to_completion_wall_us.max(span);
                self.max_submit_tail_wait_wall_us = self.max_submit_tail_wait_wall_us.max(tail);
            }
            failed => {
                if failed.is_err() {
                    self.errors += 1;
                } else {
                    self.timeouts += 1;
                }
                self.failed_prepare_to_observation_wall_us += span;
                self.failed_submit_tail_wait_wall_us += tail;
            }
        }
    }
}

/// Drain one fixed-size aggregate and the number of lost observations. A busy
/// producer leaves counters pending. Producers use try_lock, allocate no storage
/// and write no logs; a busy consumer increments the loss count.
pub fn drain_frame_completion() -> Option<(FrameCompletionTiming, u64)> {
    let mut timing = TIMING.try_lock().ok()?;
    Some((
        std::mem::replace(&mut *timing, FrameCompletionTiming::ZERO),
        DROPPED.swap(0, Ordering::Relaxed),
    ))
}

#[cfg(all(feature = "gxm", target_os = "vita"))]
pub(crate) struct FrameCompletion(std::time::Instant);
#[cfg(all(feature = "gxm", target_os = "vita"))]
impl FrameCompletion {
    pub(crate) fn start() -> Option<Self> {
        ENABLED
            .load(Ordering::Relaxed)
            .then(|| Self(std::time::Instant::now()))
    }
    pub(crate) fn complete<E>(
        self,
        wait: impl FnOnce(std::time::Duration) -> Result<bool, E>,
    ) -> Result<(), E> {
        // Existing RenderFence phase accounts the wait in the exclusive wall ledger.
        let _scope = reel_core::profile::Scope::new(reel_core::profile::Phase::RenderFence);
        let tail = std::time::Instant::now();
        let result = wait(std::time::Duration::from_secs(2));
        let end = std::time::Instant::now();
        if let Ok(mut timing) = TIMING.try_lock() {
            timing.observe(
                end.duration_since(self.0).as_micros() as u64,
                end.duration_since(tail).as_micros() as u64,
                result.as_ref().map(|done| *done).map_err(|_| ()),
            );
        } else {
            DROPPED.fetch_add(1, Ordering::Relaxed);
        }
        // Timeout remains a failed observation while normal presentation proceeds.
        result.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failure_observations_keep_successful_intervals_and_maxima_clean() {
        let mut timing = FrameCompletionTiming::default();
        timing.observe(23, 11, Ok(true));
        timing.observe(19, 7, Ok(true));
        timing.observe(2_000_100, 2_000_000, Ok(false));
        timing.observe(50, 30, Err(()));
        assert_eq!(
            (timing.completed, timing.timeouts, timing.errors),
            (2, 1, 1)
        );
        assert_eq!(
            (
                timing.prepare_to_completion_wall_us,
                timing.submit_tail_wait_wall_us
            ),
            (42, 18)
        );
        assert_eq!(
            (
                timing.max_prepare_to_completion_wall_us,
                timing.max_submit_tail_wait_wall_us
            ),
            (23, 11)
        );
        assert_eq!(
            (
                timing.failed_prepare_to_observation_wall_us,
                timing.failed_submit_tail_wait_wall_us
            ),
            (2_000_150, 2_000_030)
        );
    }
}
