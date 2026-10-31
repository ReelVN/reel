//! Format bounded renderer work counters outside the graphics thread.
use reel_render::profiling::{CACHE_MISS_LABELS, EffectSample, drain_effects};
use std::fmt::Write as _;

pub(super) fn print(rows: &mut Vec<EffectSample>, line: &mut String, elapsed_ms: u128) {
    if let Some((passes, dropped)) = reel_render::profiling::drain_serial_passes() {
        for pass in passes {
            if pass.calls != 0 {
                eprintln!(
                    "[reel-pass] t_ms={elapsed_ms} kind={} calls={} viewport_pixels={} storage_pixels={} scissor_pixels={} geometry_references={} prior_drain_us={} submit_wall_us={} completion_drain_us={} max_completion_drain_us={}",
                    pass.label,
                    pass.calls,
                    pass.viewport_pixels,
                    pass.storage_pixels,
                    pass.scissor_pixels,
                    pass.geometry_references,
                    pass.prior_drain_us,
                    pass.submit_wall_us,
                    pass.completion_drain_us,
                    pass.max_completion_drain_us
                );
            }
        }
        if dropped != 0 {
            eprintln!("[reel-pass-loss] t_ms={elapsed_ms} dropped={dropped}");
        }
    }
    let Some(dropped) = drain_effects(rows) else {
        return;
    };
    for row in rows.iter() {
        line.clear();
        let _ = write!(line, "[reel-effect] t_ms={elapsed_ms} definition=");
        for byte in row.definition {
            let _ = write!(line, "{byte:02x}");
        }
        let _ = write!(
            line,
            " variant={} draws={} hits={} early_hits={} source_pixels={} source_storage_pixels={} output_pixels={} output_storage_pixels={} scissor_pixels={} sampler_bindings={} submit_wall_us={} max_source={}x{} max_output={}x{} largest_owner={}",
            row.variant,
            row.draws,
            row.hits,
            row.early_hits,
            row.source_pixels,
            row.source_storage_pixels,
            row.output_pixels,
            row.output_storage_pixels,
            row.scissor_pixels,
            row.sampler_bindings,
            row.submit_wall_us,
            row.max_source[0],
            row.max_source[1],
            row.max_output[0],
            row.max_output[1],
            row.largest_owner
        );
        for (label, value) in CACHE_MISS_LABELS.iter().zip(row.cache_misses) {
            if value != 0 {
                let _ = write!(line, " cache_{label}={value}");
            }
        }
        if row.serial_draws != 0 {
            let _ = write!(
                line,
                " serial_draws={} serial_prior_drain_us={} serial_completion_drain_us={} serial_max_completion_drain_us={}",
                row.serial_draws,
                row.serial_prior_drain_us,
                row.serial_completion_drain_us,
                row.serial_max_completion_drain_us
            );
        }
        eprintln!("{line}");
    }
    if dropped != 0 {
        eprintln!("[reel-effect-loss] t_ms={elapsed_ms} dropped={dropped}");
    }
}
