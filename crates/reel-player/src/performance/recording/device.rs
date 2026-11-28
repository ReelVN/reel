//! Native counters published to the existing live sampler without file IO.
use std::io::{self, Write};
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
};

#[cfg(test)]
const CAPACITY: usize = 256;
pub(super) const NAMES: &[&str] = &[
    "renderer_resources",
    "renderer_targets",
    "renderer_transient",
    "cached_images",
    "retired_images",
    "retired_image_readers",
    "pixels_reserved",
    "pixels_used",
    "pixel_blocks",
    "pixels_idle",
    "buffers_reserved",
    "buffers_used",
    "buffer_blocks",
    "buffers_idle",
    "parameter_cdram",
    "context_rings",
    "patcher_pool",
    "patcher_host",
    "target_driver",
    "imported_video",
    "render_targets",
    "display_sync_objects",
    "scene_submissions",
    "notification_capacity_waits",
    "display_queue_waits",
    "display_flips",
    "scene_begin_calls",
    "scene_begin_cpu_us",
    "scene_end_calls",
    "scene_end_cpu_us",
    "uniform_reserve_calls",
    "uniform_reserve_cpu_us",
    "draw_calls",
    "draw_cpu_us",
    "notification_capacity_wait_calls",
    "notification_capacity_wait_cpu_us",
    "upload_capacity_wait_calls",
    "upload_capacity_wait_cpu_us",
    "resource_replacement_wait_calls",
    "resource_replacement_wait_cpu_us",
    "main_textures_reserved",
    "main_textures_used",
    "main_texture_blocks",
    "main_textures_idle",
    "display_reserved",
    "main_physical_committed",
    "main_physical_capacity",
    "contiguous_kernel_admitted",
    "contiguous_kernel_peak_admitted",
    "cdram_physical_committed",
    "cdram_physical_capacity",
    "host_wait_calls",
    "host_wait_requested_us",
    "host_wait_notified_calls",
    "host_wait_notified_us",
    "host_wait_timed_out_calls",
    "host_wait_timed_out_us",
    "host_wait_deadline_overrun_us",
    "host_wait_max_deadline_overrun_us",
    "host_wait_immediate_calls",
    "sample_monotonic_us",
    "scene_end_thread_samples",
    "scene_end_sampled_wall_us",
    "scene_end_thread_run_us",
    "thread_query_calls",
    "thread_query_wall_us",
    "thread_query_failures",
    "thread_query_last_error",
    "thread_clock_regressions",
    "scenes_per_target",
    "scene_end_slow_calls",
    "scene_end_max_wall_us",
    "scene_clock_regressions",
    "notification_submissions",
    "notification_observations",
    "notification_observed_age_us",
    "notification_max_observed_age_us",
    "notification_last_submit_us",
    "notification_last_submit_generation",
    "notification_last_observed_us",
    "notification_last_observed_generation",
    "notification_pending",
    "notification_oldest_pending_us",
    "notification_peak_pending",
    "notification_peak_same_target",
    "display_callback_calls",
    "display_callback_us",
    "display_set_calls",
    "display_set_us",
    "display_wait_calls",
    "display_wait_us",
    "display_max_callback_us",
    "display_last_entered_us",
    "display_last_completed_us",
    "slow_scene_present",
    "slow_scene_generation",
    "slow_scene_target",
    "slow_scene_width",
    "slow_scene_height",
    "slow_scene_draw_calls",
    "slow_scene_indices",
    "slow_scene_triangles",
    "slow_scene_entered_us",
    "slow_scene_exited_us",
    "slow_scene_thread_sampled",
    "slow_scene_thread_run_us",
    "slow_scene_sampled_wall_us",
    "slow_scene_query_error",
    "slow_scene_before_pending",
    "slow_scene_before_same_target",
    "slow_scene_before_oldest_us",
    "slow_scene_after_pending",
    "slow_scene_after_same_target",
    "slow_scene_after_oldest_us",
    "slow_scene_result",
    "thread_runtime_observed",
    "thread_runtime_id",
    "thread_runtime_run_us",
    "thread_runtime_observed_at_us",
    "thread_runtime_priority",
    "thread_runtime_affinity",
    "thread_runtime_cpu",
    "thread_runtime_error",
];

pub(crate) type DeviceSample = [u64; NAMES.len()];

static SEQUENCE: AtomicU32 = AtomicU32::new(0);
static AT_US: AtomicU64 = AtomicU64::new(0);
static SAMPLE: [AtomicU64; NAMES.len()] = [const { AtomicU64::new(0) }; NAMES.len()];

#[cfg(target_os = "vita")]
fn publish(elapsed_us: u64, values: DeviceSample) {
    SEQUENCE.fetch_add(1, Ordering::AcqRel);
    for (slot, value) in SAMPLE.iter().zip(values) {
        slot.store(value, Ordering::Relaxed);
    }
    AT_US.store(elapsed_us, Ordering::Relaxed);
    SEQUENCE.fetch_add(1, Ordering::Release);
}

/// Copy scalar observations; the sampler never accesses the native graphics context.
pub(super) fn append_live(line: &mut String) -> bool {
    use std::fmt::Write as _;
    let sequence = SEQUENCE.load(Ordering::Acquire);
    if sequence == 0 || !sequence.is_multiple_of(2) {
        return false;
    }
    let values: DeviceSample = std::array::from_fn(|i| SAMPLE[i].load(Ordering::Relaxed));
    let at = AT_US.load(Ordering::Relaxed);
    std::sync::atomic::fence(Ordering::Acquire);
    if sequence != SEQUENCE.load(Ordering::Acquire) {
        return false;
    }
    line.clear();
    let _ = write!(
        line,
        "[reel-device] sample={} elapsed_us={at}",
        sequence / 2
    );
    for (name, value) in NAMES.iter().zip(values) {
        let _ = write!(line, " {name}={value}");
    }
    true
}

#[derive(Default)]
pub(super) struct DeviceSamples {
    rows: VecDeque<(u64, DeviceSample)>,
    samples: u64,
    main_peak_bytes: u64,
    cdram_peak_bytes: u64,
}

impl DeviceSamples {
    #[cfg(any(target_os = "vita", test))]
    pub(super) fn record(
        &mut self,
        elapsed_us: u64,
        values: DeviceSample,
        main_bytes: u64,
        cdram_bytes: u64,
    ) {
        self.samples += 1;
        self.main_peak_bytes = self.main_peak_bytes.max(main_bytes);
        self.cdram_peak_bytes = self.cdram_peak_bytes.max(cdram_bytes);
        #[cfg(target_os = "vita")]
        {
            publish(elapsed_us, values);
        }
        #[cfg(not(target_os = "vita"))]
        {
            if self.rows.is_empty() {
                self.rows.reserve_exact(CAPACITY);
            }
            if self.rows.len() == CAPACITY {
                self.rows.pop_front();
            }
            self.rows.push_back((elapsed_us, values));
        }
    }

    pub(super) fn bytes(&self) -> usize {
        self.rows.capacity() * std::mem::size_of::<(u64, DeviceSample)>()
    }

    pub(super) fn write_memory_peaks(&self, out: &mut impl Write) -> io::Result<()> {
        if self.samples != 0 {
            writeln!(
                out,
                "vita_memory_peaks\t{}\t{}\t{}",
                self.samples, self.main_peak_bytes, self.cdram_peak_bytes
            )?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn write(&self, output: &mut impl Write) -> io::Result<()> {
        if self.rows.is_empty() {
            return Ok(());
        }
        writeln!(
            output,
            "timing\tCumulative SDK CPU microseconds, including blocking; not GPU execution time"
        )?;
        write!(output, "elapsed_us")?;
        for name in NAMES {
            write!(output, "\t{name}")?;
        }
        writeln!(output)?;
        for (elapsed, values) in &self.rows {
            write!(output, "{elapsed}")?;
            for value in values {
                write!(output, "\t{value}")?;
            }
            writeln!(output)?;
        }
        output.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_history_is_bounded_and_retains_the_latest_observation() {
        let mut samples = DeviceSamples::default();
        assert_eq!(samples.bytes(), 0);
        for index in 0..=CAPACITY {
            samples.record(
                index as u64,
                [index as u64; NAMES.len()],
                (CAPACITY - index) as u64,
                index as u64,
            );
        }
        assert_eq!(samples.rows.len(), CAPACITY);
        assert_eq!(samples.rows.front().unwrap().0, 1);
        assert_eq!(samples.rows.back().unwrap().1[0], CAPACITY as u64);
        assert_eq!(samples.rows.capacity(), CAPACITY);
        let mut report = Vec::new();
        samples.write_memory_peaks(&mut report).unwrap();
        assert_eq!(report, b"vita_memory_peaks\t257\t256\t256\n");
    }
}
