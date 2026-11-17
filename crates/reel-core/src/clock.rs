//! Time values have fixed units and contain no process-local `Instant`.

use std::time::Duration;

/// Monotonic nanoseconds from an adapter's fixed origin.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct MonoTime(pub u64);

/// UTC milliseconds; a clock may have no wall-clock source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct UnixMillis(pub i64);

pub trait Clock {
    fn monotonic_now(&self) -> MonoTime;
    fn wall_time(&self) -> Option<UnixMillis>;
}

/// Delay to the next fixed animation sample after `elapsed`, at a nonzero rate in millihertz.
pub fn animation_sample_delay(elapsed: Duration, rate_millihertz: u32) -> Duration {
    let rate = u128::from(rate_millihertz);
    let elapsed = elapsed.as_nanos();
    let frame = elapsed * rate / 1_000_000_000_000 + 1;
    let next = (frame * 1_000_000_000_000).div_ceil(rate);
    Duration::from_nanos((next - elapsed) as u64)
}
