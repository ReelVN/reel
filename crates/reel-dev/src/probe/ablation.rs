//! Identical rendering workloads under controlled queue and display settings.
use super::{DeviceBackend, Result, log, native, render_cases};

pub fn run(renderer: &mut DeviceBackend, frames: usize, trace: bool) -> Result {
    for case in ["empty", "solid", "rgba", "bc1", "text", "offscreen"] {
        for (vsync, flights) in [(true, 1), (true, 2), (false, 2)] {
            render_cases(renderer, case, frames, trace, vsync, flights)?;
        }
    }
    Ok(())
}

pub fn native_delta(case: &str, before: native::SubmissionTiming, after: native::SubmissionTiming) {
    for (name, before, after) in [
        ("scene_begin", before.scene_begin, after.scene_begin),
        ("scene_end", before.scene_end, after.scene_end),
        (
            "uniform_reserve",
            before.uniform_reserve,
            after.uniform_reserve,
        ),
        ("draw", before.draw, after.draw),
        (
            "notification_wait",
            before.notification_capacity_wait,
            after.notification_capacity_wait,
        ),
        (
            "upload_wait",
            before.upload_capacity_wait,
            after.upload_capacity_wait,
        ),
        (
            "replacement_wait",
            before.resource_replacement_wait,
            after.resource_replacement_wait,
        ),
        (
            "display_callback",
            before.display.callback,
            after.display.callback,
        ),
        (
            "display_flip",
            before.display.set_framebuffer,
            after.display.set_framebuffer,
        ),
        (
            "display_wait",
            before.display.wait_framebuffer,
            after.display.wait_framebuffer,
        ),
    ] {
        log(format_args!(
            "NATIVE {case} {name} calls={} wall_us={}",
            after.calls.saturating_sub(before.calls),
            after.cpu_us.saturating_sub(before.cpu_us)
        ));
    }
    let before = before.notifications;
    let after = after.notifications;
    log(format_args!(
        "NATIVE {case} notifications submitted={} observed={} observed_age_us={} pending={}",
        after.submissions.saturating_sub(before.submissions),
        after.observations.saturating_sub(before.observations),
        after.observed_age_us.saturating_sub(before.observed_age_us),
        after.pending.count
    ));
}
