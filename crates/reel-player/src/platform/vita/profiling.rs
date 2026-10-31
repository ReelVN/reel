//! Sample native state in memory; the session writes it after closing playback.
use crate::probe::Recording;
use reel_render::gxm::GxmBackend;
use std::time::Duration;
use vita_native::app::wake::WaitResult;

#[cfg(feature = "thread-profiling")]
std::thread_local! {
    static CPU_COUNTER: std::cell::RefCell<Option<(vita_native::app::time::Pmon, u64)>> = const {
        std::cell::RefCell::new(None)
    };
}

#[cfg(feature = "thread-profiling")]
fn read_thread_clock() -> Result<reel_core::profile::ThreadTime, i32> {
    CPU_COUNTER.with(|state| {
        let state = state.borrow();
        let (counter, max_frequency_hz) = state.as_ref().ok_or(-1)?;
        let observed = counter.read().map_err(|error| error.code)?;
        Ok(reel_core::profile::ThreadTime {
            thread: observed.thread_id as u64,
            counter: reel_core::profile::ThreadCounter::Cycles32 {
                ticks: observed.raw_cycle_ticks,
                max_frequency_hz: *max_frequency_hz,
            },
        })
    })
}

#[cfg(feature = "thread-profiling")]
fn validate_thread_counter(
    counter: &vita_native::app::time::Pmon,
    max_frequency_hz: u64,
) -> Result<(), vita_native::app::Error> {
    let before = counter.read()?;
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_millis(2) {
        std::hint::spin_loop();
    }
    let busy = counter.read()?;
    let busy_wall = start.elapsed().as_micros() as u64;
    vita_native::app::time::sleep(Duration::from_millis(2))?;
    let after = counter.read()?;
    let busy_ticks = busy.raw_cycle_ticks.wrapping_sub(before.raw_cycle_ticks);
    let sleep_ticks = after.raw_cycle_ticks.wrapping_sub(busy.raw_cycle_ticks);
    eprintln!(
        "[reel-thread-counter-check] busy_wall_us={busy_wall} busy_ticks={busy_ticks} sleep_ticks={sleep_ticks} max_frequency_hz={max_frequency_hz} units=raw_cycle_ticks"
    );
    if busy_ticks == 0
        || u128::from(busy_ticks) * 1_000_000 > u128::from(busy_wall) * u128::from(max_frequency_hz)
        || u64::from(sleep_ticks) * 2 >= u64::from(busy_ticks)
    {
        return Err(vita_native::app::Error {
            operation: "thread cycle counter startup check",
            code: -1,
        });
    }
    Ok(())
}

#[cfg(feature = "thread-profiling")]
pub(super) fn install_thread_clock() {
    if CPU_COUNTER.with(|state| state.borrow().is_some()) {
        return;
    }
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
    let initialized = (|| {
        let max_frequency_hz = u64::from(clocks.arm_mhz?) * 1_000_000;
        let counter = vita_native::app::time::Pmon::start()?;
        validate_thread_counter(&counter, max_frequency_hz)?;
        CPU_COUNTER.with(|state| *state.borrow_mut() = Some((counter, max_frequency_hz)));
        Ok::<(), vita_native::app::Error>(())
    })();
    if let Err(error) = initialized {
        eprintln!("[reel-thread-clock] initialization_error={error}");
        return;
    }
    reel_core::profile::install_thread_clock(read_thread_clock);
    match vita_native::app::time::thread_runtime() {
        Ok(value) => eprintln!(
            "[reel-thread-clock] source=pmon_cycle32 thread={} priority={} affinity={} cpu={} kernel_run_us={} vm_stride=127 replay_stride=1 present_stride=1",
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
    recording.record_device([
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
    ]);
}
