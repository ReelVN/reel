//! Cumulative calling-thread SDK counters observed at presentation boundaries.
//! Counters are independent of the wall ledger and may overlap enclosing phases.
use std::io::{self, Write};
pub(super) const NAMES: [&str; 19] = [
    "scene_begin_calls",
    "scene_begin_us",
    "scene_end_calls",
    "scene_end_us",
    "uniform_reserve_calls",
    "uniform_reserve_us",
    "draw_calls",
    "draw_us",
    "notification_wait_calls",
    "notification_wait_us",
    "upload_wait_calls",
    "upload_wait_us",
    "replacement_wait_calls",
    "replacement_wait_us",
    "notification_submissions",
    "notification_observations",
    "notification_observed_age_us",
    "pending_count",
    "pending_oldest_age_us",
];
pub(super) const FIELDS: usize = NAMES.len();
#[cfg(any(target_os = "vita", test))]
pub(super) fn delta(after: &[u64; FIELDS], before: &[u64; FIELDS]) -> Option<[u64; FIELDS]> {
    let mut result = *after;
    // Last two fields are gauges; all others are cumulative counters.
    for i in 0..FIELDS - 2 {
        result[i] = after[i].checked_sub(before[i])?;
    }
    Some(result)
}
pub(super) fn columns(out: &mut impl Write) -> io::Result<()> {
    write!(out, "\tnative_observed")?;
    for name in NAMES {
        write!(out, "\tnative_{name}")?;
    }
    Ok(())
}
pub(super) fn values(out: &mut impl Write, sample: Option<&[u64; FIELDS]>) -> io::Result<()> {
    write!(out, "\t{}", u8::from(sample.is_some()))?;
    for value in sample.copied().unwrap_or([0; FIELDS]) {
        write!(out, "\t{value}")?;
    }
    Ok(())
}
