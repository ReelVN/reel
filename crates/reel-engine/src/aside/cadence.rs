//! Drift-free animation sampling on the session's logical clock.
use std::time::Duration;

/// The rate is validated at configuration/host admission. Fractional refresh
/// rates retain their phase across seconds; late samples skip elapsed frames.
pub(super) fn poll_after(time: f64, millihertz: u32) -> Duration {
    let rate = u128::from(millihertz);
    let Ok(time) = Duration::try_from_secs_f64(time) else {
        // Such clocks exceed Duration's range; preserve a finite polling period.
        return Duration::from_nanos(1_000_000_000_000u64.div_ceil(u64::from(millihertz)));
    };
    let now = time.as_nanos();
    let frame = now * rate / 1_000_000_000_000 + 1;
    let next = (frame * 1_000_000_000_000).div_ceil(rate);
    Duration::from_nanos((next - now) as u64)
}
