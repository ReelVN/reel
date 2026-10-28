//! Bounded native snapshots written after playback, including failed sessions.
use std::{
    collections::VecDeque,
    io::{self, Write},
    path::Path,
};

#[cfg(any(target_os = "vita", test))]
const CAPACITY: usize = 256;
pub(super) const NAMES: [&str; 40] = [
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
];

pub(crate) type DeviceSample = [u64; NAMES.len()];

#[derive(Default)]
pub(super) struct DeviceSamples {
    rows: VecDeque<(u64, DeviceSample)>,
}

impl DeviceSamples {
    #[cfg(any(target_os = "vita", test))]
    pub(super) fn record(&mut self, elapsed_us: u64, values: DeviceSample) {
        if self.rows.is_empty() {
            self.rows.reserve_exact(CAPACITY);
        }
        if self.rows.len() == CAPACITY {
            self.rows.pop_front();
        }
        self.rows.push_back((elapsed_us, values));
    }

    pub(super) fn bytes(&self) -> usize {
        self.rows.capacity() * std::mem::size_of::<(u64, DeviceSample)>()
    }

    pub(super) fn write(&self, session_path: &Path) -> io::Result<()> {
        if self.rows.is_empty() {
            return Ok(());
        }
        let mut output = io::BufWriter::new(std::fs::File::create(
            session_path.with_extension("device.tsv"),
        )?);
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
            samples.record(index as u64, [index as u64; NAMES.len()]);
        }
        assert_eq!(samples.rows.len(), CAPACITY);
        assert_eq!(samples.rows.front().unwrap().0, 1);
        assert_eq!(samples.rows.back().unwrap().1[0], CAPACITY as u64);
        assert_eq!(samples.rows.capacity(), CAPACITY);
    }
}
