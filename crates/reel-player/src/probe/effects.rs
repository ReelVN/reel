//! Format bounded renderer work counters outside the graphics thread.
use reel_render::profiling::{EffectSample, drain_effects};
use std::fmt::Write as _;

pub(super) fn print(rows: &mut Vec<EffectSample>, line: &mut String, elapsed_ms: u128) {
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
        eprintln!("{line}");
    }
    if dropped != 0 {
        eprintln!("[reel-effect-loss] t_ms={elapsed_ms} dropped={dropped}");
    }
}
