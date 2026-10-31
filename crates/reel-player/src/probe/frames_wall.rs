//! Optional main-thread exclusive ledger; raw nanoseconds retain exact closure.
use reel_core::profile::{NAMES, WallSnapshot};
use std::io::{self, Write};

pub(super) fn columns(out: &mut impl Write) -> io::Result<()> {
    write!(
        out,
        "\twall_observed\twall_phase_count\twall_span_ns\twall_unscoped_ns\twall_lost_ns\twall_errors\twall_clock_reads"
    )?;
    for name in NAMES {
        write!(out, "\twall_{name}_ns\twall_{name}_calls")?;
    }
    Ok(())
}

pub(super) fn values(out: &mut impl Write, sample: Option<&WallSnapshot>) -> io::Result<()> {
    write!(out, "\t{}\t{}", u8::from(sample.is_some()), NAMES.len())?;
    for value in [
        sample.map_or(0, |s| s.wall_ns),
        sample.map_or(0, |s| s.unscoped_ns),
        sample.map_or(0, |s| s.lost_ns),
        sample.map_or(0, |s| s.errors),
        sample.map_or(0, |s| s.clock_reads),
    ] {
        write!(out, "\t{value}")?;
    }
    for i in 0..NAMES.len() {
        write!(
            out,
            "\t{}\t{}",
            sample.map_or(0, |s| s.nanos[i]),
            sample.map_or(0, |s| s.calls[i])
        )?;
    }
    Ok(())
}
