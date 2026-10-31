//! Accumulate every maintenance loop between presentation submissions. A short
//! VM slice is not a frame: retained samples include all intervening loop work.
//! These are submission intervals and wall times, not display/GPU timestamps.
use super::Row;
#[cfg(all(feature = "vm-execution-counts", any(target_os = "vita", test)))]
#[path = "frames_counts.rs"]
mod counts;
#[cfg(any(feature = "vm-execution-counts", feature = "vm-profiling"))]
#[path = "frames_vm.rs"]
mod vm;
use reel_core::profile::Phase;
#[path = "frames_thread.rs"]
mod thread;
use std::collections::VecDeque;
#[cfg(any(target_os = "vita", test))]
use std::io::{self, Write};

const CAPACITY: usize = 512;
const LABELS: usize = super::SEGMENTS;
const PHASES: [Phase; 28] = [
    Phase::VmSlice,
    Phase::HostCall,
    Phase::SessionGc,
    Phase::SessionActivity,
    Phase::SessionSchedule,
    Phase::StageProperties,
    Phase::Session,
    Phase::Tick,
    Phase::UiSync,
    Phase::Record,
    Phase::Compile,
    Phase::Replay,
    Phase::Present,
    Phase::Poll,
    Phase::HostWait,
    Phase::Input,
    Phase::ControlPoll,
    Phase::TargetCreate,
    Phase::SessionDiagnostics,
    Phase::RenderSubmission,
    Phase::SessionMaintenance,
    Phase::SessionGcBegin,
    Phase::Screen,
    Phase::ImagePlan,
    Phase::Upload,
    Phase::OffscreenPlan,
    Phase::RetainedPlan,
    Phase::UiImageRequests,
];

#[derive(Default)]
struct Pending {
    active: u64,
    loops: u64,
    max_loop: u64,
    micros: [u64; PHASES.len()],
    upload_bytes: u64,
}
struct Frame {
    segment: u64,
    index: u64,
    elapsed: u64,
    span: u64,
    partial: bool,
    presented: u64,
    work: Pending,
    thread: Option<reel_core::profile::ThreadSnapshot>,
    #[cfg(any(feature = "vm-execution-counts", feature = "vm-profiling"))]
    vm: Option<vm::Observation>,
}

#[derive(Default)]
pub(super) struct Frames {
    labels: VecDeque<(u64, String)>,
    rows: VecDeque<Frame>,
    segment: u64,
    index: u64,
    last_present: Option<u64>,
    pending: Pending,
    dropped: u64,
    discarded_loops: u64,
    previous_thread: Option<reel_core::profile::ThreadSnapshot>,
    #[cfg(any(feature = "vm-execution-counts", feature = "vm-profiling"))]
    observed_vm: Option<vm::Observation>,
    #[cfg(any(feature = "vm-execution-counts", feature = "vm-profiling"))]
    previous_vm: Option<vm::Observation>,
}
impl Frames {
    /// Capture only at submission boundaries. The cumulative snapshot includes
    /// every intervening maintenance loop and host-side frame-slot API access.
    #[cfg(any(feature = "vm-execution-counts", feature = "vm-profiling"))]
    pub(super) fn observe_vm(&mut self, story: Option<&super::StoryRuntime>) {
        self.observed_vm =
            story.map(|story| vm::Observation::capture(story.driver().adapter().vm()));
    }

    pub(super) fn record(&mut self, title: &str, row: &Row) {
        if self.labels.back().is_none_or(|(_, label)| label != title) {
            self.discarded_loops += self.pending.loops;
            self.pending = Pending::default();
            self.segment += 1;
            self.index = 0;
            self.last_present = None;
            self.previous_thread = None;
            #[cfg(any(feature = "vm-execution-counts", feature = "vm-profiling"))]
            {
                self.previous_vm = None;
            }
            if self.labels.capacity() == 0 {
                self.labels.reserve_exact(LABELS);
            }
            if self.labels.len() == LABELS {
                let (expired, _) = self.labels.pop_front().unwrap();
                while self
                    .rows
                    .front()
                    .is_some_and(|frame| frame.segment <= expired)
                {
                    self.rows.pop_front();
                    self.dropped += 1;
                }
            }
            self.labels.push_back((self.segment, title.into()));
        }
        self.pending.active += row.active;
        self.pending.loops += row.loops;
        self.pending.max_loop = self.pending.max_loop.max(row.max_loop);
        self.pending.upload_bytes += row.geometry.bytes;
        for (slot, phase) in self.pending.micros.iter_mut().zip(PHASES) {
            *slot += row.phases.micros[phase as usize];
        }
        let presented = row.phases.calls[Phase::Present as usize];
        if presented == 0 {
            return;
        }
        if self.rows.capacity() == 0 {
            self.rows.reserve_exact(CAPACITY);
        }
        if self.rows.len() == CAPACITY {
            self.rows.pop_front();
            self.dropped += 1;
        }
        let previous = self.last_present.replace(row.elapsed);
        let after = reel_core::profile::ThreadSnapshot::capture();
        let thread = self
            .previous_thread
            .as_ref()
            .and_then(|before| after.checked_delta(before));
        self.previous_thread = Some(after);
        #[cfg(any(feature = "vm-execution-counts", feature = "vm-profiling"))]
        let vm = {
            let observed = self.observed_vm.take();
            let delta = observed.and_then(|after| {
                self.previous_vm
                    .as_ref()
                    .and_then(|before| after.checked_delta(before))
            });
            self.previous_vm = observed;
            delta
        };
        self.rows.push_back(Frame {
            segment: self.segment,
            index: self.index,
            elapsed: row.elapsed,
            span: previous.map_or(0, |at| row.elapsed.saturating_sub(at)),
            partial: previous.is_none(),
            presented,
            work: std::mem::take(&mut self.pending),
            thread,
            #[cfg(any(feature = "vm-execution-counts", feature = "vm-profiling"))]
            vm,
        });
        self.index += presented;
    }

    pub(super) fn bytes(&self) -> usize {
        self.rows.capacity() * size_of::<Frame>()
            + self.labels.capacity() * size_of::<(u64, String)>()
            + self
                .labels
                .iter()
                .map(|(_, label)| label.capacity())
                .sum::<usize>()
    }

    #[cfg(any(target_os = "vita", test))]
    pub(super) fn report(&self, out: &mut impl Write) -> io::Result<()> {
        writeln!(out, "[reel-frame-report] begin")?;
        writeln!(
            out,
            "frame_metadata\tversion=1\tcapacity={CAPACITY}\tdropped={}\tdiscarded_segment_loops={}\tpending_loops={}\ttiming=submission_intervals_and_nested_wall_time\tvm_execution_counts={}\tvm_profiling={}\tthread_profiling={}\tthread_vm_stride=127",
            self.dropped,
            self.discarded_loops,
            self.pending.loops,
            cfg!(feature = "vm-execution-counts"),
            cfg!(feature = "vm-profiling"),
            cfg!(feature = "thread-profiling")
        )?;
        for (id, label) in &self.labels {
            writeln!(
                out,
                "frame_segment\t{id}\t{}",
                label.replace(['\t', '\r', '\n'], " ")
            )?;
        }
        write!(
            out,
            "frame_columns\tsegment\tindex\tpartial\telapsed_us\tspan_us\tactive_us\tloops\tmax_loop_us\tpresent_calls\tgeometry_upload_bytes"
        )?;
        for phase in PHASES {
            write!(out, "\t{}_us", reel_core::profile::NAMES[phase as usize])?;
        }
        thread::columns(out)?;
        #[cfg(any(feature = "vm-execution-counts", feature = "vm-profiling"))]
        vm::columns(out)?;
        writeln!(out)?;
        for frame in &self.rows {
            write!(
                out,
                "frame_sample\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                frame.segment,
                frame.index,
                u8::from(frame.partial),
                frame.elapsed,
                frame.span,
                frame.work.active,
                frame.work.loops,
                frame.work.max_loop,
                frame.presented,
                frame.work.upload_bytes
            )?;
            for value in frame.work.micros {
                write!(out, "\t{value}")?;
            }
            thread::values(out, frame.thread.as_ref())?;
            #[cfg(any(feature = "vm-execution-counts", feature = "vm-profiling"))]
            vm::values(out, frame.vm.as_ref())?;
            writeln!(out)?;
        }
        writeln!(out, "[reel-frame-report] end")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "vm-execution-counts")]
    use aside_vm::executor::ExecutionCounts;
    #[cfg(feature = "vm-execution-counts")]
    fn observation(counts: ExecutionCounts) -> vm::Observation {
        vm::Observation {
            counts,
            #[cfg(feature = "vm-profiling")]
            driver: Default::default(),
            #[cfg(feature = "vm-profiling")]
            steps: Default::default(),
        }
    }
    fn row(elapsed: u64, vm: u64, present: bool) -> Row {
        let mut row = Row {
            elapsed,
            active: 1000,
            loops: 1,
            max_loop: 1000,
            ..Default::default()
        };
        row.phases.micros[Phase::VmSlice as usize] = vm;
        row.phases.calls[Phase::Present as usize] = u64::from(present);
        row
    }
    #[test]
    fn small_slices_accumulate_until_present_and_span_includes_idle_time() {
        let mut frames = Frames::default();
        frames.record("animation", &row(1000, 1000, false));
        frames.record("animation", &row(2000, 2000, false));
        frames.record("animation", &row(3000, 3000, true));
        frames.record("animation", &row(20000, 1000, true));
        assert_eq!(
            (frames.rows[0].work.micros[0], frames.rows[0].work.loops),
            (6000, 3)
        );
        assert!(frames.rows[0].partial);
        assert!(!frames.rows[1].partial);
        assert_eq!(
            (frames.rows[1].span, frames.rows[1].work.active),
            (17000, 1000)
        );
    }
    #[test]
    fn label_boundaries_keep_partial_frames_and_report_unpresented_work() {
        let mut frames = Frames::default();
        frames.record("loading", &row(1000, 1000, false));
        frames.record("animation", &row(2000, 2000, true));
        frames.record("animation", &row(3000, 3000, false));
        assert_eq!(frames.discarded_loops, 1);
        assert_eq!(frames.rows[0].work.micros[0], 2000);
        assert_eq!(frames.pending.loops, 1);
        let mut output = Vec::new();
        frames.report(&mut output).unwrap();
        let text = String::from_utf8(output).unwrap();
        let width = text
            .lines()
            .find(|line| line.starts_with("frame_columns\t"))
            .unwrap()
            .split('\t')
            .count();
        assert!(
            text.lines()
                .filter(|line| line.starts_with("frame_sample\t"))
                .all(|line| line.split('\t').count() == width)
        );
    }
    #[test]
    fn retained_frames_have_labels_and_bounded_storage() {
        let mut frames = Frames::default();
        for index in 0..2000 {
            frames.record(&(index / 10).to_string(), &row(index * 1000, 0, true));
        }
        assert_eq!(frames.rows.len(), CAPACITY);
        assert_eq!(frames.labels.len(), LABELS);
        assert_eq!(frames.dropped, 2000 - CAPACITY as u64);
        assert!(
            frames
                .rows
                .iter()
                .all(|row| frames.labels.iter().any(|(id, _)| *id == row.segment))
        );
    }

    #[cfg(feature = "vm-execution-counts")]
    #[test]
    fn count_deltas_cross_maintenance_loops_and_reject_labels_and_vm_changes() {
        let mut frames = Frames::default();
        let mut counts = ExecutionCounts::default();
        counts.heap_id = 1;
        counts.epoch = 2;
        counts.script_entries = 10;
        frames.observed_vm = Some(observation(counts));
        frames.record("a", &row(1000, 0, true));
        assert!(frames.rows[0].vm.is_none());
        frames.record("a", &row(2000, 0, false));
        counts.script_entries = 17;
        counts.named_table_starts = 9;
        frames.observed_vm = Some(observation(counts));
        frames.record("a", &row(3000, 0, true));
        assert_eq!(frames.rows[1].vm.unwrap().counts.script_entries, 7);
        assert_eq!(frames.rows[1].vm.unwrap().counts.named_table_starts, 9);
        counts.script_entries = 19;
        frames.observed_vm = Some(observation(counts));
        frames.record("b", &row(4000, 0, true));
        assert!(frames.rows[2].vm.is_none());
        counts.heap_id = 3;
        frames.observed_vm = Some(observation(counts));
        frames.record("b", &row(5000, 0, true));
        assert!(frames.rows[3].vm.is_none());
        counts.script_entries = 23;
        frames.observed_vm = Some(observation(counts));
        frames.record("b", &row(6000, 0, true));
        assert_eq!(frames.rows[4].vm.unwrap().counts.script_entries, 4);
        assert!(frames.bytes() >= CAPACITY * size_of::<Frame>());
    }
}
