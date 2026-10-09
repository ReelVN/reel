//! Time values have fixed units and contain no process-local `Instant`.

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
