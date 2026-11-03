//! Thread execution observations share complete presentation boundaries.
use reel_core::profile::{NAMES, THREAD_METRICS, THREAD_PHASES, ThreadSnapshot};
use std::io::{self, Write};

pub(super) fn columns(out: &mut impl Write) -> io::Result<()> {
    write!(out, "\tthread_profile_valid")?;
    for phase in THREAD_PHASES {
        let phase = NAMES[phase as usize];
        for metric in THREAD_METRICS {
            write!(out, "\tthread_{phase}_{metric}")?;
        }
        write!(out, "\tthread_{phase}_last_error_code")?;
    }
    Ok(())
}
pub(super) fn values(out: &mut impl Write, sample: Option<&ThreadSnapshot>) -> io::Result<()> {
    let valid = sample.is_some_and(|value| value.available);
    write!(out, "\t{}", u8::from(valid))?;
    let empty = ThreadSnapshot::default();
    let sample = sample.filter(|_| valid).unwrap_or(&empty);
    for cost in sample.costs {
        for value in cost.values {
            write!(out, "\t{value}")?;
        }
        write!(out, "\t{}", cost.last_error)?;
    }
    Ok(())
}
