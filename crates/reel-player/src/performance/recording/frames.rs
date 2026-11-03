//! Accumulate every maintenance loop between presentation submissions. A short
//! VM slice is not a frame: retained samples include all intervening loop work.
//! These are submission intervals and wall times, not display/GPU timestamps.
use super::Row;
#[cfg(feature = "vm-execution-counts")]
#[path = "frames_counts.rs"]
mod counts;
#[cfg(any(feature = "vm-execution-counts", feature = "vm-profiling"))]
#[path = "frames_vm.rs"]
mod vm;
use reel_core::profile::Phase;
#[path = "frames_native.rs"]
mod native;
#[path = "frames_thread.rs"]
mod thread;
#[path = "frames_wall.rs"]
mod wall;
#[cfg(target_os = "vita")]
pub(super) const NATIVE_FIELDS: usize = native::FIELDS;
use std::collections::VecDeque;
use std::io::{self, Write};

const CAPACITY: usize = 512;
const SLOW_CAPACITY: usize = 64;
const LABELS: usize = super::SEGMENTS;
const CALL_PHASES: [Phase; 3] = [Phase::VmSlice, Phase::HostCall, Phase::SessionGc];
const PHASES: [Phase; 42] = [
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
    Phase::SessionMedia,
    Phase::SessionSaves,
    Phase::SessionAssets,
    Phase::RenderFence,
    Phase::UiRewrite,
    Phase::UiLayout,
    Phase::UiCompose,
    Phase::UiPaint,
    Phase::UiGeometry,
    Phase::UiImageResolve,
    Phase::GeometryUpload,
    Phase::GeometryPack,
    Phase::DrawPrepare,
    Phase::UiPaintRecord,
];

struct Pending {
    active: u64,
    loops: u64,
    max_loop: u64,
    micros: [u64; PHASES.len()],
    upload_bytes: u64,
    projection_updates: u64,
    calls: [u64; CALL_PHASES.len()],
}
impl Default for Pending {
    fn default() -> Self {
        Self {
            active: 0,
            loops: 0,
            max_loop: 0,
            micros: [0; PHASES.len()],
            upload_bytes: 0,
            projection_updates: 0,
            calls: [0; CALL_PHASES.len()],
        }
    }
}
struct Frame {
    segment: u64,
    index: u64,
    elapsed: u64,
    span: u64,
    partial: bool,
    presented: u64,
    loop_begin: u64,
    loop_end: u64,
    update_begin: u64,
    update_end: u64,
    work: Pending,
    thread: Option<reel_core::profile::ThreadSnapshot>,
    wall: Option<reel_core::profile::WallSnapshot>,
    native: Option<[u64; native::FIELDS]>,
    #[cfg(any(feature = "vm-execution-counts", feature = "vm-profiling"))]
    vm: Option<vm::Observation>,
    #[cfg(any(feature = "vm-execution-counts", feature = "vm-profiling"))]
    vm_invalid_reason: u8,
}

#[derive(Default)]
pub(super) struct Frames {
    labels: VecDeque<(u64, String)>,
    rows: VecDeque<Frame>,
    slow: Vec<Frame>,
    segment: u64,
    index: u64,
    last_present: Option<u64>,
    loop_sequence: u64,
    update_sequence: u64,
    last_present_loop: Option<u64>,
    last_present_update: Option<u64>,
    pending: Pending,
    dropped: u64,
    discarded_loops: u64,
    previous_thread: Option<reel_core::profile::ThreadSnapshot>,
    previous_wall: Option<reel_core::profile::WallSnapshot>,
    previous_native: Option<[u64; native::FIELDS]>,
    #[cfg(any(feature = "vm-execution-counts", feature = "vm-profiling"))]
    observed_vm: Option<vm::Observation>,
    #[cfg(any(feature = "vm-execution-counts", feature = "vm-profiling"))]
    previous_vm: Option<vm::Observation>,
}
impl Frames {
    #[cfg(any(target_os = "vita", test))]
    pub(super) fn record_native(&mut self, current: Option<[u64; native::FIELDS]>) {
        if let Some(frame) = self.rows.back_mut() {
            frame.native = current.as_ref().and_then(|after| {
                self.previous_native
                    .as_ref()
                    .and_then(|before| native::delta(after, before))
            });
        }
        self.previous_native = current;
    }
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
            self.last_present_loop = None;
            self.last_present_update = None;
            self.previous_thread = None;
            self.previous_wall = None;
            self.previous_native = None;
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
                let before = self.slow.len();
                self.slow.retain(|frame| frame.segment > expired);
                self.dropped += (before - self.slow.len()) as u64;
            }
            self.labels.push_back((self.segment, title.into()));
        }
        self.pending.active += row.active;
        self.pending.loops += row.loops;
        self.pending.max_loop = self.pending.max_loop.max(row.max_loop);
        self.pending.upload_bytes += row.geometry.bytes;
        self.loop_sequence += row.loops;
        let updates = row.phases.calls[Phase::ProjectionPublished as usize];
        self.update_sequence += updates;
        self.pending.projection_updates += updates;
        for (count, phase) in self.pending.calls.iter_mut().zip(CALL_PHASES) {
            *count += row.phases.calls[phase as usize];
        }
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
            let expired = self.rows.pop_front().unwrap();
            self.retain_slow(expired);
        }
        let previous = self.last_present.replace(row.elapsed);
        let loop_begin = self
            .last_present_loop
            .replace(self.loop_sequence)
            .unwrap_or(self.loop_sequence - self.pending.loops);
        let update_begin = self
            .last_present_update
            .replace(self.update_sequence)
            .unwrap_or(self.update_sequence - self.pending.projection_updates);
        let after = reel_core::profile::ThreadSnapshot::capture();
        let thread = self
            .previous_thread
            .as_ref()
            .and_then(|before| after.checked_delta(before));
        self.previous_thread = Some(after);
        let after = reel_core::profile::WallSnapshot::capture();
        let wall = after.as_ref().and_then(|after| {
            self.previous_wall
                .as_ref()
                .and_then(|before| after.checked_delta(before))
        });
        self.previous_wall = after;
        #[cfg(any(feature = "vm-execution-counts", feature = "vm-profiling"))]
        let (vm, vm_invalid_reason) = {
            let observed = self.observed_vm.take();
            let (delta, reason) = match (observed.as_ref(), self.previous_vm.as_ref()) {
                (None, _) => (None, 2),
                (Some(_), None) => (None, 1),
                (Some(after), Some(before)) if !after.same_identity(before) => (None, 3),
                (Some(after), Some(before)) => match after.checked_delta(before) {
                    Some(delta) => (Some(delta), 0),
                    None => (None, 4),
                },
            };
            self.previous_vm = observed;
            (delta, reason)
        };
        self.rows.push_back(Frame {
            segment: self.segment,
            index: self.index,
            elapsed: row.elapsed,
            span: previous.map_or(0, |at| row.elapsed.saturating_sub(at)),
            partial: previous.is_none(),
            presented,
            loop_begin,
            loop_end: self.loop_sequence,
            update_begin,
            update_end: self.update_sequence,
            work: std::mem::take(&mut self.pending),
            thread,
            wall,
            native: None,
            #[cfg(any(feature = "vm-execution-counts", feature = "vm-profiling"))]
            vm,
            #[cfg(any(feature = "vm-execution-counts", feature = "vm-profiling"))]
            vm_invalid_reason,
        });
        self.index += presented;
    }

    fn retain_slow(&mut self, frame: Frame) {
        if self.slow.len() < SLOW_CAPACITY {
            if self.slow.capacity() == 0 {
                self.slow.reserve_exact(SLOW_CAPACITY);
            }
            self.slow.push(frame);
            return;
        }
        let (index, lightest) = self
            .slow
            .iter()
            .enumerate()
            .min_by_key(|(_, frame)| frame.work.active)
            .unwrap();
        if frame.work.active > lightest.work.active {
            self.slow[index] = frame;
        }
        self.dropped += 1;
    }

    pub(super) fn bytes(&self) -> usize {
        self.rows.capacity() * size_of::<Frame>()
            + self.slow.capacity() * size_of::<Frame>()
            + self.labels.capacity() * size_of::<(u64, String)>()
            + self
                .labels
                .iter()
                .map(|(_, label)| label.capacity())
                .sum::<usize>()
    }

    pub(super) fn report(&self, out: &mut impl Write) -> io::Result<()> {
        writeln!(out, "[reel-frame-report] begin")?;
        writeln!(
            out,
            "frame_metadata\tversion=1\tcapacity={}\trecent_capacity={CAPACITY}\tslow_capacity={SLOW_CAPACITY}\tslow_selection=active_us\tdropped={}\tdiscarded_segment_loops={}\tpending_loops={}\ttiming=submission_intervals_and_nested_wall_time\tvm_execution_counts={}\tvm_profiling={}\tthread_profiling={}\tthread_vm_stride=127\tvm_counter_interval=presentation_submission\tvm_invalid_reasons=0:valid,1:first_or_segment,2:no_vm,3:identity_changed,4:counter_reset\tupdate_kind=projection_published\tclock_update_counts=false",
            CAPACITY + SLOW_CAPACITY,
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
            "frame_columns\tsegment\tindex\tpartial\telapsed_us\tspan_us\tactive_us\tloops\tmax_loop_us\tpresent_calls\tgeometry_upload_bytes\tloop_begin\tloop_end\tprojection_update_begin\tprojection_update_end\tprojection_updates_count\tretained_slow"
        )?;
        for phase in PHASES {
            write!(out, "\t{}_us", reel_core::profile::NAMES[phase as usize])?;
        }
        for phase in CALL_PHASES {
            write!(out, "\t{}_calls", reel_core::profile::NAMES[phase as usize])?;
        }
        #[cfg(any(feature = "vm-execution-counts", feature = "vm-profiling"))]
        write!(out, "\tvm_counter_invalid_reason")?;
        wall::columns(out)?;
        native::columns(out)?;
        thread::columns(out)?;
        #[cfg(any(feature = "vm-execution-counts", feature = "vm-profiling"))]
        vm::columns(out)?;
        writeln!(out)?;
        let mut rows: Vec<_> = self.slow.iter().chain(&self.rows).collect();
        rows.sort_unstable_by_key(|frame| frame.index);
        for frame in rows {
            write!(
                out,
                "frame_sample\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                frame.segment,
                frame.index,
                u8::from(frame.partial),
                frame.elapsed,
                frame.span,
                frame.work.active,
                frame.work.loops,
                frame.work.max_loop,
                frame.presented,
                frame.work.upload_bytes,
                frame.loop_begin,
                frame.loop_end,
                frame.update_begin,
                frame.update_end,
                frame.work.projection_updates,
                u8::from(
                    self.rows
                        .front()
                        .is_none_or(|first| frame.index < first.index)
                )
            )?;
            for value in frame.work.micros {
                write!(out, "\t{value}")?;
            }
            for value in frame.work.calls {
                write!(out, "\t{value}")?;
            }
            #[cfg(any(feature = "vm-execution-counts", feature = "vm-profiling"))]
            write!(out, "\t{}", frame.vm_invalid_reason)?;
            wall::values(out, frame.wall.as_ref())?;
            native::values(out, frame.native.as_ref())?;
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
            gc: Default::default(),
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
    fn native_samples_cross_loops_reset_at_labels_and_preserve_gauges() {
        let mut frames = Frames::default();
        frames.record("a", &row(1000, 0, true));
        frames.record_native(Some([10; native::FIELDS]));
        assert!(frames.rows[0].native.is_none());
        frames.record("a", &row(1100, 0, false));
        frames.record("a", &row(2000, 0, true));
        frames.record_native(Some([15; native::FIELDS]));
        let sample = frames.rows[1].native.unwrap();
        assert_eq!(&sample[..native::FIELDS - 2], &[5; native::FIELDS - 2]);
        assert_eq!(&sample[native::FIELDS - 2..], &[15; 2]);
        frames.record("b", &row(3000, 0, true));
        frames.record_native(Some([20; native::FIELDS]));
        assert!(frames.rows[2].native.is_none());
        frames.record("b", &row(4000, 0, true));
        frames.record_native(None);
        frames.record("b", &row(5000, 0, true));
        frames.record_native(Some([25; native::FIELDS]));
        assert!(frames.rows[4].native.is_none());
    }

    #[test]
    fn wall_frames_close_across_active_scopes_and_do_not_span_labels() {
        let _ledger = reel_core::profile::WallRecording::start().unwrap();
        let mut frames = Frames::default();
        let _scope = reel_core::profile::Scope::new(Phase::Session);
        frames.record("a", &row(1000, 0, true));
        frames.record("a", &row(2000, 0, false));
        frames.record("a", &row(3000, 0, true));
        let wall = frames.rows[1].wall.as_ref().unwrap();
        assert_eq!(wall.wall_ns, wall.nanos[Phase::Session as usize]);
        assert_eq!(wall.errors, 0);
        assert!(frames.rows[0].wall.is_none());
        frames.record("b", &row(4000, 0, true));
        assert!(frames.rows[2].wall.is_none());
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
        assert_eq!(
            frames.dropped,
            2000 - (frames.rows.len() + frames.slow.len()) as u64
        );
        assert!(
            frames
                .rows
                .iter()
                .chain(&frames.slow)
                .all(|row| frames.labels.iter().any(|(id, _)| *id == row.segment))
        );
    }

    #[test]
    fn slow_frames_survive_the_recent_ring_without_retaining_idle_gaps() {
        let mut frames = Frames::default();
        for index in 0..2000 {
            let mut sample = row(index * 1_000_000, 0, true);
            sample.active = if index == 1 { 200_000 } else { 1000 + index };
            frames.record("story", &sample);
        }
        assert_eq!(frames.slow.len(), SLOW_CAPACITY);
        assert!(
            frames
                .slow
                .iter()
                .any(|frame| frame.index == 1 && frame.work.active == 200_000)
        );
        assert!(
            frames
                .slow
                .iter()
                .all(|frame| frame.index == 1 || frame.index >= 1425)
        );
        assert_eq!(frames.dropped, 2000 - (CAPACITY + SLOW_CAPACITY) as u64);
        let mut output = Vec::new();
        frames.report(&mut output).unwrap();
        let output = String::from_utf8(output).unwrap();
        let indices: Vec<u64> = output
            .lines()
            .filter(|line| line.starts_with("frame_sample\t"))
            .map(|line| line.split('\t').nth(2).unwrap().parse().unwrap())
            .collect();
        assert_eq!(indices.len(), CAPACITY + SLOW_CAPACITY);
        assert!(indices.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn loop_and_projection_sequences_cover_all_maintenance_work() {
        let mut frames = Frames::default();
        let mut first = row(1000, 10, true);
        first.phases.calls[Phase::ProjectionPublished as usize] = 2;
        frames.record("a", &first);
        let mut maintenance = row(2000, 20, false);
        maintenance.phases.calls[Phase::ProjectionPublished as usize] = 3;
        maintenance.phases.calls[Phase::VmSlice as usize] = 4;
        frames.record("a", &maintenance);
        let mut presented = row(3000, 30, true);
        presented.phases.calls[Phase::ProjectionPublished as usize] = 1;
        presented.phases.calls[Phase::VmSlice as usize] = 5;
        frames.record("a", &presented);
        let frame = &frames.rows[1];
        assert_eq!(
            (frame.loop_begin, frame.loop_end, frame.work.loops),
            (1, 3, 2)
        );
        assert_eq!(
            (
                frame.update_begin,
                frame.update_end,
                frame.work.projection_updates
            ),
            (2, 6, 4)
        );
        assert_eq!(frame.work.calls[0], 9);
        frames.record("b", &row(4000, 0, true));
        let frame = &frames.rows[2];
        assert!(frame.partial);
        assert_eq!((frame.loop_begin, frame.loop_end), (3, 4));
        assert_eq!((frame.update_begin, frame.update_end), (6, 6));
    }

    #[cfg(feature = "vm-execution-counts")]
    #[test]
    fn vm_reason_codes_and_gc_counts_share_the_presentation_interval() {
        let mut frames = Frames::default();
        let mut counts = ExecutionCounts::default();
        counts.heap_id = 1;
        counts.epoch = 2;
        let mut sample = observation(counts);
        sample.gc.collections = 2;
        sample.gc.units = 100;
        frames.observed_vm = Some(sample);
        frames.record("a", &row(1000, 0, true));
        assert_eq!(frames.rows[0].vm_invalid_reason, 1);
        sample.gc.collections = 3;
        sample.gc.units = 145;
        frames.observed_vm = Some(sample);
        frames.record("a", &row(2000, 20, true));
        assert_eq!(frames.rows[1].vm_invalid_reason, 0);
        let gc = frames.rows[1].vm.unwrap().gc;
        assert_eq!((gc.collections, gc.units), (1, 45));
        sample.counts.epoch = 3;
        frames.observed_vm = Some(sample);
        frames.record("a", &row(3000, 0, true));
        assert_eq!(frames.rows[2].vm_invalid_reason, 3);
        sample.gc.units = 140;
        frames.observed_vm = Some(sample);
        frames.record("a", &row(4000, 0, true));
        assert_eq!(frames.rows[3].vm_invalid_reason, 4);
        frames.record("a", &row(5000, 0, true));
        assert_eq!(frames.rows[4].vm_invalid_reason, 2);
        frames.observed_vm = Some(sample);
        frames.record("a", &row(6000, 0, true));
        assert_eq!(frames.rows[5].vm_invalid_reason, 1);
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
