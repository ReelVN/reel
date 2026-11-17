//! Format bounded renderer work counters outside the graphics thread.
use reel_render::profiling::{CACHE_MISS_LABELS, EffectSample, drain_effects};
use std::fmt::Write as _;

pub(super) fn print(rows: &mut Vec<EffectSample>, line: &mut String, elapsed_ms: u128) {
    let inputs = reel_render::profiling::drain_effect_input_plans();
    if [
        inputs.preparations,
        inputs.candidates,
        inputs.selected_inputs,
        inputs.pinned_bytes,
        inputs.rejected_owners,
    ]
    .into_iter()
    .any(|value| value != 0)
    {
        eprintln!(
            "[reel-effect-inputs] t_ms={elapsed_ms} preparations={} candidates={} selected_inputs={} pinned_bytes={} rejected_owners={}",
            inputs.preparations,
            inputs.candidates,
            inputs.selected_inputs,
            inputs.pinned_bytes,
            inputs.rejected_owners,
        );
    }
    if let Some((paint, dropped)) = reel_render::profiling::drain_paint_regions()
        && (paint.batches != 0 || paint.transparent_replacements != 0 || dropped != 0)
    {
        eprintln!(
            "[reel-paint-regions] t_ms={elapsed_ms} batches={} extra_draws={} original_scissor_pixels={} direct_scissor_pixels={} transparent_replacements={} transparent_scissor_pixels={} dropped={dropped}",
            paint.batches,
            paint.extra_draws,
            paint.original_pixels,
            paint.direct_pixels,
            paint.transparent_replacements,
            paint.transparent_pixels,
        );
    }
    if let Some((frame, dropped)) = reel_render::profiling::drain_frame_completion()
        && (frame.completed != 0 || frame.timeouts != 0 || frame.errors != 0 || dropped != 0)
    {
        eprintln!(
            "[reel-frame-completion] t_ms={elapsed_ms} completed={} timeouts={} errors={} prepare_to_completion_wall_us={} submit_tail_wait_wall_us={} max_prepare_to_completion_wall_us={} max_submit_tail_wait_wall_us={} failed_prepare_to_observation_wall_us={} failed_submit_tail_wait_wall_us={} dropped={dropped} gpu_timestamp=false",
            frame.completed,
            frame.timeouts,
            frame.errors,
            frame.prepare_to_completion_wall_us,
            frame.submit_tail_wait_wall_us,
            frame.max_prepare_to_completion_wall_us,
            frame.max_submit_tail_wait_wall_us,
            frame.failed_prepare_to_observation_wall_us,
            frame.failed_submit_tail_wait_wall_us,
        );
    }
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
            " variant={} draws={} hits={} early_hits={} source_pixels={} source_storage_pixels={} crop_source_draws={} crop_source_original_storage_pixels={} output_pixels={} output_storage_pixels={} scissor_pixels={} sampler_bindings={} submit_wall_us={} max_source={}x{} max_output={}x{} largest_owner={}",
            row.variant,
            row.draws,
            row.hits,
            row.early_hits,
            row.source_pixels,
            row.source_storage_pixels,
            row.crop_source_draws,
            row.crop_source_original_storage_pixels,
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
