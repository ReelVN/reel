//! Sample native state in memory; the session writes it after closing playback.
use crate::performance::recording::Recording;
use reel_render::gxm::GxmBackend;
use std::time::Duration;
use vita_native::app::wake::WaitResult;

#[cfg(feature = "thread-profiling")]
pub(super) fn install_thread_clock() {
    let clocks = vita_native::app::power::clock_frequencies();
    let raw = |value: Result<u32, vita_native::app::Error>| {
        value.map_or_else(|error| i64::from(error.code), i64::from)
    };
    eprintln!(
        "[reel-clock-frequencies] arm_mhz={} gpu_mhz={} bus_mhz={} gpu_xbar_mhz={} negative_values=native_query_errors",
        raw(clocks.arm_mhz),
        raw(clocks.gpu_mhz),
        raw(clocks.bus_mhz),
        raw(clocks.gpu_xbar_mhz)
    );
    let checked = (|| {
        let before = vita_native::app::time::thread_runtime()?;
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_millis(1) {
            std::hint::spin_loop();
        }
        let after = vita_native::app::time::thread_runtime()?;
        let wall = start.elapsed().as_micros() as u64;
        let run = after.run_micros.checked_sub(before.run_micros);
        if before.thread_id != after.thread_id || !run.is_some_and(|run| run > 0 && run <= wall) {
            return Err(vita_native::app::Error {
                operation: "kernel thread counter does not support short-scope observation",
                code: -1,
            });
        }
        Ok::<(), vita_native::app::Error>(())
    })();
    if let Err(error) = checked {
        eprintln!("[reel-thread-clock] unavailable={error}");
        return;
    }
    reel_core::profile::install_thread_clock(|| {
        vita_native::app::time::thread_runtime()
            .map(|value| reel_core::profile::ThreadTime {
                thread: value.thread_id as u64,
                run_us: value.run_micros,
            })
            .map_err(|error| error.code)
    });
    match vita_native::app::time::thread_runtime() {
        Ok(value) => eprintln!(
            "[reel-thread-clock] source=kernel_runClocks thread={} priority={} affinity={} cpu={} run_us={} vm_stride=127 replay_stride=1 present_stride=1",
            value.thread_id, value.priority, value.affinity, value.cpu, value.run_micros
        ),
        Err(error) => eprintln!("[reel-thread-clock] initialization_error={error}"),
    }
}

/// Cumulative timed waits; elapsed time includes kernel scheduling after wakeup.
#[derive(Default)]
pub(super) struct Waits {
    calls: u64,
    requested_us: u64,
    notified_calls: u64,
    notified_us: u64,
    timed_out_calls: u64,
    timed_out_us: u64,
    deadline_overrun_us: u64,
    max_deadline_overrun_us: u64,
    immediate_calls: u64,
}

impl Waits {
    pub(super) fn record(&mut self, requested: Duration, elapsed: Duration, result: WaitResult) {
        let requested = requested.as_micros() as u64;
        let elapsed = elapsed.as_micros() as u64;
        self.calls += 1;
        self.requested_us += requested;
        self.immediate_calls += u64::from(requested == 0);
        match result {
            WaitResult::Notified => {
                self.notified_calls += 1;
                self.notified_us += elapsed;
            }
            WaitResult::TimedOut => {
                self.timed_out_calls += 1;
                self.timed_out_us += elapsed;
            }
        }
        let overrun = elapsed.saturating_sub(requested);
        self.deadline_overrun_us += overrun;
        self.max_deadline_overrun_us = self.max_deadline_overrun_us.max(overrun);
    }
}

pub(super) fn record_device(
    recording: &mut Recording,
    backend: &GxmBackend,
    contiguous: &reel_core::budget::BudgetPool,
    waits: &Waits,
) {
    let _observer = reel_core::profile::Scope::new(reel_core::profile::Phase::Observer);
    let Ok(native) = backend.native_memory_usage() else {
        return;
    };
    let renderer = backend.usage();
    let timing = native.submission_timing;
    let thread = timing.scene_end_thread;
    let queue = timing.scene_queue;
    let notifications = timing.notifications;
    let display = timing.display;
    let slow = queue.last_slow.unwrap_or_default();
    let error_code = |code: Option<i32>| code.map_or(u64::MAX, |code| u64::from(code as u32));
    let memory = backend.native_memory_state();
    let main = memory.main;
    let contiguous = contiguous.usage();
    // Preserve cumulative observations even when the startup check rejects
    // short scopes. Long intervals can be compared after checking thread ID
    // and monotonicity; these values do not identify individual phase CPU time.
    #[cfg(feature = "thread-profiling")]
    let runtime = match vita_native::app::time::thread_runtime() {
        Ok(value) => [
            1,
            value.thread_id as u32 as u64,
            value.run_micros,
            vita_native::app::time::monotonic_micros(),
            value.priority as u32 as u64,
            value.affinity as u32 as u64,
            value.cpu as u32 as u64,
            u64::MAX,
        ],
        Err(error) => [0, 0, 0, 0, 0, 0, 0, error.code as u32 as u64],
    };
    #[cfg(not(feature = "thread-profiling"))]
    let runtime = [0, 0, 0, 0, 0, 0, 0, u64::MAX];
    let sample = [
        renderer.resources,
        renderer.targets,
        renderer.transient,
        renderer.cached_images,
        renderer.retired_images,
        renderer.retired_image_readers,
        native.pixels.reserved_bytes,
        native.pixels.used_bytes,
        u64::from(native.pixels.blocks),
        native.pixels.idle_bytes,
        native.buffers.reserved_bytes,
        native.buffers.used_bytes,
        u64::from(native.buffers.blocks),
        native.buffers.idle_bytes,
        native.parameter_cdram_bytes,
        native.context_ring_bytes,
        native.patcher_pool_bytes,
        native.patcher_host_bytes,
        native.render_target_driver_bytes,
        native.imported_video_bytes,
        u64::from(native.render_targets),
        u64::from(native.display_sync_objects),
        native.scene_submissions,
        native.notification_capacity_waits,
        native.display_queue_waits,
        native.display_flips,
        timing.scene_begin.calls,
        timing.scene_begin.cpu_us,
        timing.scene_end.calls,
        timing.scene_end.cpu_us,
        timing.uniform_reserve.calls,
        timing.uniform_reserve.cpu_us,
        timing.draw.calls,
        timing.draw.cpu_us,
        timing.notification_capacity_wait.calls,
        timing.notification_capacity_wait.cpu_us,
        timing.upload_capacity_wait.calls,
        timing.upload_capacity_wait.cpu_us,
        timing.resource_replacement_wait.calls,
        timing.resource_replacement_wait.cpu_us,
        native.main_textures.reserved_bytes,
        native.main_textures.used_bytes,
        u64::from(native.main_textures.blocks),
        native.main_textures.idle_bytes,
        native.display_reserved_bytes,
        main.committed_bytes,
        main.capacity_bytes,
        contiguous.bytes,
        contiguous.peak_bytes,
        memory.cdram.committed_bytes,
        memory.cdram.capacity_bytes,
        waits.calls,
        waits.requested_us,
        waits.notified_calls,
        waits.notified_us,
        waits.timed_out_calls,
        waits.timed_out_us,
        waits.deadline_overrun_us,
        waits.max_deadline_overrun_us,
        waits.immediate_calls,
        vita_native::app::time::monotonic_micros(),
        thread.samples,
        thread.wall_us,
        thread.run_us,
        thread.query_calls,
        thread.query_wall_us,
        thread.query_failures,
        error_code(thread.last_query_error),
        thread.clock_regressions,
        u64::from(queue.scenes_per_frame),
        queue.slow_calls,
        queue.max_wall_us,
        queue.clock_regressions,
        notifications.submissions,
        notifications.observations,
        notifications.observed_age_us,
        notifications.max_observed_age_us,
        notifications.last_submit_us,
        u64::from(notifications.last_submit_generation),
        notifications.last_observed_us,
        u64::from(notifications.last_observed_generation),
        u64::from(notifications.pending.count),
        notifications.pending.oldest_age_us,
        u64::from(notifications.peak_pending),
        u64::from(notifications.peak_same_target),
        display.callback.calls,
        display.callback.cpu_us,
        display.set_framebuffer.calls,
        display.set_framebuffer.cpu_us,
        display.wait_framebuffer.calls,
        display.wait_framebuffer.cpu_us,
        display.max_callback_us,
        display.last_entered_us,
        display.last_completed_us,
        u64::from(queue.last_slow.is_some()),
        u64::from(slow.notification),
        slow.target_id,
        u64::from(slow.target_extent[0]),
        u64::from(slow.target_extent[1]),
        slow.work.draw_calls,
        slow.work.indices,
        slow.work.triangles,
        slow.entered_us,
        slow.exited_us,
        u64::from(slow.thread_sampled),
        slow.thread_run_us.unwrap_or(u64::MAX),
        slow.thread_wall_us.unwrap_or(u64::MAX),
        error_code(slow.thread_query_error),
        u64::from(slow.before.count),
        u64::from(slow.before.same_target),
        slow.before.oldest_age_us,
        u64::from(slow.after.count),
        u64::from(slow.after.same_target),
        slow.after.oldest_age_us,
        u64::from(slow.result as u32),
        runtime[0],
        runtime[1],
        runtime[2],
        runtime[3],
        runtime[4],
        runtime[5],
        runtime[6],
        runtime[7],
    ];
    recording.record_device(sample, main.committed_bytes, memory.cdram.committed_bytes);
}

/// Read existing accounting without polling or adding a GPU synchronization.
pub(super) fn record_submission(recording: &mut Recording, backend: &GxmBackend) {
    let _observer = reel_core::profile::Scope::new(reel_core::profile::Phase::Observer);
    let sample = backend.native_memory_usage().ok().map(|usage| {
        let t = usage.submission_timing;
        [
            t.scene_begin.calls,
            t.scene_begin.cpu_us,
            t.scene_end.calls,
            t.scene_end.cpu_us,
            t.uniform_reserve.calls,
            t.uniform_reserve.cpu_us,
            t.draw.calls,
            t.draw.cpu_us,
            t.notification_capacity_wait.calls,
            t.notification_capacity_wait.cpu_us,
            t.upload_capacity_wait.calls,
            t.upload_capacity_wait.cpu_us,
            t.resource_replacement_wait.calls,
            t.resource_replacement_wait.cpu_us,
            t.notifications.submissions,
            t.notifications.observations,
            t.notifications.observed_age_us,
            u64::from(t.notifications.pending.count),
            t.notifications.pending.oldest_age_us,
        ]
    });
    recording.record_submission(sample);
}
