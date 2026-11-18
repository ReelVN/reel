//! Bounded measurements collected in memory and emitted through the process output.
use crate::story_runtime::StoryRuntime;
use reel_core::profile::{GeometryUploads, NAMES, Sample};
use std::{
    collections::VecDeque,
    io::{self, Write},
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    time::Instant,
};

const WINDOWS: usize = 512;
const SLOW: usize = 64;
const SEGMENTS: usize = 64;

#[cfg(feature = "performance")]
mod active;
mod device;
mod effects;
mod frames;
mod timeline;
#[cfg(feature = "vm-profiling")]
mod vm_hotspots;
#[cfg(any(target_os = "vita", test))]
pub(crate) use device::DeviceSample;
static VM_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static GPU_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
const VM_NAMES: [&str; 26] = [
    "fn",
    "pc",
    "main",
    "derived_task",
    "derived_pending",
    "stage_ready",
    "feedback",
    "native_entries",
    "native_resumes",
    "slices",
    "last_instructions",
    "last_work",
    "gc_phase",
    "collections",
    "gc_units",
    "pending_bytes",
    "file",
    "source",
    "instructions",
    "work",
    "derived_starts",
    "compact_derived_starts",
    "derived_outputs",
    "compact_derived_outputs",
    "script_binding_hits",
    "script_binding_misses",
];
const ABSENT: u64 = u64::MAX;
static VM_REQUESTED: AtomicBool = AtomicBool::new(true);
static VM_SEQUENCE: AtomicU32 = AtomicU32::new(0);
static VM_SESSION: AtomicU32 = AtomicU32::new(0);
static VM_EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
static VM_AT_MS: AtomicU64 = AtomicU64::new(0);
static VM_SAMPLE: [AtomicU64; VM_NAMES.len()] = [const { AtomicU64::new(ABSENT) }; VM_NAMES.len()];
#[cfg(feature = "vm-profiling")]
const VM_COST_METRICS: usize = aside_vm::executor::StepCost::METRIC_NAMES.len();
#[cfg(feature = "vm-profiling")]
const VM_COST_FIELDS: usize = aside_vm::executor::STEP_NAMES.len() * VM_COST_METRICS;
#[cfg(feature = "vm-profiling")]
static VM_COSTS: [AtomicU64; VM_COST_FIELDS] = [const { AtomicU64::new(0) }; VM_COST_FIELDS];

/// The live thread requests one snapshot per second. The event loop otherwise
/// reads only the memory ledger and this flag, without walking VM task frames.
fn publish_vm(story: Option<&StoryRuntime>, timing: bool) {
    if !VM_REQUESTED.swap(false, Ordering::Relaxed) {
        return;
    }
    let sample = vm_sample(story);
    #[cfg(feature = "vm-profiling")]
    let costs = story
        .filter(|_| timing)
        .map(|story| story.driver().adapter().vm().step_costs())
        .unwrap_or_default();
    VM_SEQUENCE.fetch_add(1, Ordering::AcqRel);
    for (slot, value) in VM_SAMPLE.iter().zip(sample) {
        slot.store(value, Ordering::Relaxed);
    }
    #[cfg(feature = "vm-profiling")]
    for (row, cost) in VM_COSTS.as_chunks::<VM_COST_METRICS>().0.iter().zip(costs) {
        for (slot, value) in row.iter().zip(cost.metrics()) {
            slot.store(value, Ordering::Relaxed);
        }
    }
    let at_ms = VM_EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64;
    VM_AT_MS.store(at_ms, Ordering::Relaxed);
    VM_SEQUENCE.fetch_add(1, Ordering::Release);
    #[cfg(feature = "vm-profiling")]
    if timing && let Some(story) = story {
        vm_hotspots::publish(
            story.driver().adapter().vm(),
            VM_SESSION.load(Ordering::Relaxed),
            at_ms,
        );
    }
}

fn vm_sample(story: Option<&StoryRuntime>) -> [u64; VM_NAMES.len()] {
    let Some(story) = story else {
        return [ABSENT; VM_NAMES.len()];
    };
    let adapter = story.driver().adapter();
    let vm = adapter.vm();
    let task = vm.task_id();
    let location = task.and_then(|task| vm.task_location(task).ok().flatten());
    let derived_task =
        task.is_some_and(|task| vm.reactive_task_output(task).ok().flatten().is_some());
    let stats = vm.stats();
    [
        location.map_or(ABSENT, |location| u64::from(location.code.function.0)),
        location.map_or(ABSENT, |location| u64::from(location.pc.0)),
        u64::from(task.is_some() && task == adapter.task()),
        u64::from(derived_task),
        u64::from(vm.heap().derived_pending()),
        u64::from(adapter.stage_ready()),
        u64::from(adapter.virtual_feedback_boundary()),
        stats.native_entries,
        stats.native_resumes,
        stats.slices,
        u64::from(stats.usage.instructions),
        stats.usage.work,
        match vm.heap().phase() {
            aside_vm::GcPhase::Idle => 0,
            aside_vm::GcPhase::Mark => 1,
            aside_vm::GcPhase::Weak => 2,
            aside_vm::GcPhase::Sweep => 3,
        },
        stats.heap.collections,
        stats.heap.gc_units,
        stats.heap.pending_bytes,
        location
            .and_then(|location| location.source)
            .map_or(ABSENT, |source| u64::from(source.file.0)),
        location
            .and_then(|location| location.source)
            .map_or(ABSENT, |source| u64::from(source.start)),
        stats.cumulative_instructions,
        stats.cumulative_work,
        stats.derived_starts,
        stats.compact_derived_starts,
        stats.derived_outputs,
        stats.compact_derived_outputs,
        stats.script_binding_hits,
        stats.script_binding_misses,
    ]
}

fn append_vm(line: &mut String) {
    use std::fmt::Write as _;
    let sequence = VM_SEQUENCE.load(Ordering::Acquire);
    if !sequence.is_multiple_of(2) {
        return;
    }
    let sample: [u64; VM_NAMES.len()] =
        std::array::from_fn(|i| VM_SAMPLE[i].load(Ordering::Relaxed));
    let at_ms = VM_AT_MS.load(Ordering::Relaxed);
    #[cfg(feature = "vm-profiling")]
    let costs: [u64; VM_COST_FIELDS] = std::array::from_fn(|i| VM_COSTS[i].load(Ordering::Relaxed));
    std::sync::atomic::fence(Ordering::Acquire);
    if sequence != VM_SEQUENCE.load(Ordering::Acquire) {
        return;
    }
    let _ = write!(
        line,
        " vm_sample={} vm_session={} vm_at_ms={at_ms} vm_age_ms={} vm_counter_window=main_thread_snapshot phase_window=live_thread_period vm_state=",
        sequence / 2,
        VM_SESSION.load(Ordering::Relaxed),
        (VM_EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64).saturating_sub(at_ms)
    );
    for (name, value) in VM_NAMES.into_iter().zip(sample) {
        if value != ABSENT {
            let _ = write!(line, "{name}:{value},");
        }
    }
    #[cfg(feature = "vm-profiling")]
    {
        let _ = write!(
            line,
            " vm_costs_stride={} vm_costs=",
            aside_vm::executor::STEP_SAMPLE_STRIDE
        );
        for (name, row) in aside_vm::executor::STEP_NAMES
            .iter()
            .zip(costs.as_chunks::<VM_COST_METRICS>().0)
        {
            let [calls, samples, nanos] = [row[0], row[1], row[2]];
            let _ = write!(line, "{name}:{calls}/{samples}/{nanos},");
        }
        line.push_str(" vm_step_observations=");
        for (name, row) in aside_vm::executor::STEP_NAMES
            .iter()
            .zip(costs.as_chunks::<VM_COST_METRICS>().0)
        {
            for (metric, value) in aside_vm::executor::StepCost::METRIC_NAMES
                .iter()
                .zip(row)
                .skip(3)
            {
                let _ = write!(line, "{name}.{metric}:{value},");
            }
        }
    }
}

pub(crate) struct Live {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Live {
    pub(crate) fn start() -> io::Result<Self> {
        drop(crate::media::take_profile_report());
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopped = stop.clone();
        // Unoptimized snapshot/formatting calls retain larger temporary frames.
        let stack_bytes = if cfg!(debug_assertions) {
            256 << 10
        } else {
            64 << 10
        };
        let thread = std::thread::Builder::new().name("reel-probe".into()).stack_size(stack_bytes)
            .spawn(move || {
                use std::fmt::Write as _;
                let start = Instant::now();
                let mut previous = Sample::snapshot();
                let mut geometry = GeometryUploads::snapshot();
                let mut line = String::with_capacity(8192);
                let mut effect_rows = Vec::with_capacity(reel_render::profiling::EFFECT_CAPACITY);
                let mut next_live = Instant::now() + std::time::Duration::from_secs(1);
                let interval = std::time::Duration::from_millis(if cfg!(feature = "vm-profiling") { 100 } else { 1000 });
                #[cfg(feature = "vm-profiling")]
                let mut vm_rows = Vec::with_capacity(aside_vm::executor::STEP_HOTSPOT_CAPACITY);
                eprintln!("[reel-probe] live sampling started; elapsed us and completed calls; nested phases overlap; VM counters cumulative, last_* latest slice; gc_phase idle=0 mark=1 weak=2 sweep=3; fn/pc identify the last selected task; vm_sample advances on main-thread snapshots; frame report pairs VM counters with submission timing");
                while !stopped.load(Ordering::Relaxed) {
                    let deadline = Instant::now() + interval;
                    while !stopped.load(Ordering::Relaxed) {
                        let remaining = deadline.saturating_duration_since(Instant::now());
                        if remaining.is_zero() { break; }
                        std::thread::sleep(remaining);
                    }
                    if stopped.load(Ordering::Relaxed) { break; }
                    if let Some(report) = crate::media::take_profile_report() {
                        let _ = std::io::Write::write_all(&mut std::io::stderr().lock(), report.as_bytes());
                    }
                    if Instant::now() >= next_live {
                    next_live = Instant::now() + std::time::Duration::from_secs(1);
                    let current = Sample::snapshot();
                    let delta = current.delta(previous);
                    previous = current;
                    let current_geometry = GeometryUploads::snapshot();
                    let uploads = current_geometry.delta(geometry);
                    geometry = current_geometry;
                    line.clear();
                    let _ = write!(line, "[reel-probe] t_ms={} vm={} gpu={} active=", start.elapsed().as_millis(),
                        VM_BYTES.load(Ordering::Relaxed), GPU_BYTES.load(Ordering::Relaxed));
                    for (i, elapsed) in Sample::active().iter().enumerate() {
                        if let Some(elapsed) = elapsed { let _ = write!(line, "{}:{}ms,", NAMES[i], elapsed / 1000); }
                    }
                    line.push_str(" work=");
                    for (i, name) in NAMES.iter().enumerate() {
                        if delta.calls[i] != 0 || delta.micros[i] != 0 {
                            let _ = write!(line, "{name}:{}/{};", delta.micros[i], delta.calls[i]);
                        }
                    }
                    let _ = write!(line, " geometry_upload={}/{}", uploads.bytes, uploads.calls);
                    append_vm(&mut line);
                    eprintln!("{line}");
                    if device::append_live(&mut line) {
                        eprintln!("{line}");
                    }
                    effects::print(&mut effect_rows, &mut line, start.elapsed().as_millis());
                    }
                    #[cfg(feature = "vm-profiling")]
                    vm_hotspots::print(&mut vm_rows, &mut line);
                    VM_REQUESTED.store(true, Ordering::Relaxed);
                }
                effects::print(&mut effect_rows, &mut line, start.elapsed().as_millis());
                #[cfg(feature = "vm-profiling")]
                vm_hotspots::print(&mut vm_rows, &mut line);
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}
impl Drop for Live {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Row {
    elapsed: u64,
    active: u64,
    loops: u64,
    max_loop: u64,
    max_present_gap: u64,
    vm: u64,
    gpu: u64,
    phases: Sample,
    geometry: GeometryUploads,
}

#[cfg(test)]
mod tests;
impl Row {
    fn add(&mut self, other: Self) {
        self.elapsed = other.elapsed;
        self.active += other.active;
        self.loops += other.loops;
        self.max_loop = self.max_loop.max(other.max_loop);
        self.max_present_gap = self.max_present_gap.max(other.max_present_gap);
        self.vm = self.vm.max(other.vm);
        self.gpu = self.gpu.max(other.gpu);
        self.geometry.calls += other.geometry.calls;
        self.geometry.bytes += other.geometry.bytes;
        for i in 0..NAMES.len() {
            self.phases.micros[i] += other.phases.micros[i];
            self.phases.calls[i] += other.phases.calls[i];
        }
    }
}

pub(crate) struct Recording {
    #[cfg(feature = "performance")]
    active: Option<active::Active>,
    started: Instant,
    startup: Row,
    total: Row,
    pending: Row,
    window_start: u64,
    last_present: Option<u64>,
    last_geometry: GeometryUploads,
    windows: VecDeque<Row>,
    slow: VecDeque<Row>,
    segment: (String, Row),
    segments: VecDeque<(String, Row)>,
    device: device::DeviceSamples,
    timeline: timeline::Timeline,
    frames: frames::Frames,
}
impl Recording {
    pub(crate) fn new() -> Self {
        VM_SESSION.fetch_add(1, Ordering::Relaxed);
        VM_REQUESTED.store(true, Ordering::Relaxed);
        let startup = Row {
            phases: Sample::take(),
            ..Default::default()
        };
        Self {
            #[cfg(feature = "performance")]
            active: None,
            started: Instant::now(),
            startup,
            total: startup,
            pending: Row::default(),
            window_start: 0,
            last_present: None,
            last_geometry: GeometryUploads::snapshot(),
            windows: VecDeque::with_capacity(if cfg!(target_os = "vita") { 0 } else { WINDOWS }),
            slow: VecDeque::with_capacity(if cfg!(target_os = "vita") { 0 } else { SLOW }),
            segment: (String::new(), Row::default()),
            segments: VecDeque::with_capacity(SEGMENTS),
            device: device::DeviceSamples::default(),
            timeline: timeline::Timeline::default(),
            frames: frames::Frames::default(),
        }
    }

    /// Start one requested measurement through the ordinary session path.
    #[cfg(feature = "performance")]
    pub(crate) fn start(request: super::Request) -> io::Result<Self> {
        let active = active::Active::start(request)?;
        let mut recording = Self::new();
        recording.active = Some(active);
        Ok(recording)
    }

    /// End collection while retaining its request and rows for final publication
    /// after the native graphics device has shut down.
    #[cfg(all(feature = "performance", any(target_os = "vita", test)))]
    pub(crate) fn stop(&mut self) {
        if let Some(active) = &mut self.active {
            active.stop();
        }
    }

    /// Export already recorded ordinary intervals after session cleanup. Repeated
    /// calls are inert. `closed` denotes success; other outcomes retain the error.
    #[cfg(feature = "performance")]
    pub(crate) fn finish(&mut self, title: &str, outcome: &str) -> io::Result<()> {
        let Some(active) = self.active.as_mut() else {
            return Ok(());
        };
        active.stop();
        let request = active.request.clone();
        let result = (|| {
            let file = std::fs::File::create(request.output.join("frames.tsv"))?;
            let mut output = io::BufWriter::with_capacity(64 << 10, file);
            self.write_report(&mut output, title, outcome)?;
            output.flush()
        })();
        let error = result
            .as_ref()
            .err()
            .map(ToString::to_string)
            .or_else(|| (outcome != "closed").then(|| outcome.to_owned()));
        let status = request.write_status(
            if error.is_some() {
                super::StatusState::Error
            } else {
                super::StatusState::Complete
            },
            error.as_deref(),
        );
        drop(self.active.take());
        result.and(status)
    }

    #[cfg(any(target_os = "vita", test))]
    pub(crate) fn record_device(&mut self, sample: DeviceSample) {
        self.device
            .record(self.started.elapsed().as_micros() as u64, sample);
    }

    pub(crate) fn record(
        &mut self,
        started: Instant,
        story: Option<&StoryRuntime>,
        gpu: u64,
        title: &str,
    ) -> bool {
        let _observer = reel_core::profile::Scope::new(reel_core::profile::Phase::Observer);
        let vm = story.map_or(0, |story| {
            story
                .driver()
                .adapter()
                .vm()
                .heap()
                .budget_usage()
                .charged_bytes()
        });
        VM_BYTES.store(vm, Ordering::Relaxed);
        GPU_BYTES.store(gpu, Ordering::Relaxed);
        #[cfg(feature = "performance")]
        let timing = self
            .active
            .as_ref()
            .is_none_or(|active| active.request.vm_timing);
        #[cfg(not(feature = "performance"))]
        let timing = true;
        publish_vm(story, timing);
        let elapsed = self.started.elapsed().as_micros() as u64;
        let active = started.elapsed().as_micros() as u64;
        let phases = Sample::take();
        let geometry = GeometryUploads::snapshot();
        let gap = if phases.calls[reel_core::profile::Phase::Present as usize] != 0 {
            self.last_present
                .replace(elapsed)
                .map_or(0, |last| elapsed - last)
        } else {
            0
        };
        let row = Row {
            elapsed,
            active,
            loops: 1,
            max_loop: active,
            max_present_gap: gap,
            vm,
            gpu,
            phases,
            geometry: geometry.delta(self.last_geometry),
        };
        self.last_geometry = geometry;
        self.record_row(title, row, story)
    }

    fn record_row(&mut self, title: &str, row: Row, story: Option<&StoryRuntime>) -> bool {
        self.timeline.record(title, &row, story);
        #[cfg(any(feature = "vm-execution-counts", feature = "vm-profiling"))]
        if row.phases.calls[reel_core::profile::Phase::Present as usize] != 0 {
            self.frames.observe_vm(story);
        }
        self.frames.record(title, &row);
        self.total.add(row);
        self.pending.add(row);
        if self.segment.0 != title {
            let previous = std::mem::replace(&mut self.segment, (title.into(), Row::default()));
            if previous.1.loops != 0 {
                if self.segments.len() == SEGMENTS {
                    self.segments.pop_front();
                }
                self.segments.push_back(previous);
            }
        }
        self.segment.1.add(row);
        if !cfg!(target_os = "vita") && row.active >= 50_000 {
            if self.slow.len() == SLOW {
                self.slow.pop_front();
            }
            self.slow.push_back(row);
        }
        if !cfg!(target_os = "vita") && row.elapsed - self.window_start >= 250_000 {
            if self.windows.len() == WINDOWS {
                self.windows.pop_front();
            }
            self.windows.push_back(std::mem::take(&mut self.pending));
            self.window_start = row.elapsed;
        }
        row.phases.calls[reel_core::profile::Phase::Present as usize] != 0
    }

    #[cfg(target_os = "vita")]
    pub(crate) fn record_submission(&mut self, values: Option<[u64; frames::NATIVE_FIELDS]>) {
        self.frames.record_native(values);
    }

    pub(crate) fn report(
        &self,
        out: &mut impl Write,
        title: &str,
        outcome: &str,
    ) -> io::Result<()> {
        writeln!(out, "title\t{}", title.replace(['\t', '\r', '\n'], " "))?;
        writeln!(out, "outcome\t{}", outcome.replace(['\t', '\r', '\n'], " "))?;
        writeln!(
            out,
            "recording_bytes\t{}",
            (self.windows.capacity() + self.slow.capacity()) * std::mem::size_of::<Row>()
                + self.segments.capacity() * std::mem::size_of::<(String, Row)>()
                + self
                    .segments
                    .iter()
                    .map(|(label, _)| label.capacity())
                    .sum::<usize>()
                + self.segment.0.capacity()
                + self.device.bytes()
                + self.timeline.bytes()
                + self.frames.bytes()
        )?;
        writeln!(
            out,
            "timing\tElapsed microseconds; nested phases overlap; read/decode include worker time; present includes blocking; idle gaps are included in present_gap"
        )?;
        writeln!(
            out,
            "memory\tVM and GPU resource ledgers; not process or driver memory"
        )?;
        write!(
            out,
            "kind\tlabel\telapsed_us\tactive_us\tloops\tmax_loop_us\tmax_present_gap_us\tvm_peak_bytes\tgpu_peak_bytes\tgeometry_upload_bytes\tgeometry_upload_calls"
        )?;
        for name in NAMES {
            write!(out, "\t{name}_us\t{name}_calls")?;
        }
        writeln!(out)?;
        fn row(out: &mut impl Write, kind: &str, label: &str, r: Row) -> io::Result<()> {
            write!(
                out,
                "{kind}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                label.replace(['\t', '\r', '\n'], " "),
                r.elapsed,
                r.active,
                r.loops,
                r.max_loop,
                r.max_present_gap,
                r.vm,
                r.gpu,
                r.geometry.bytes,
                r.geometry.calls
            )?;
            for i in 0..NAMES.len() {
                write!(out, "\t{}\t{}", r.phases.micros[i], r.phases.calls[i])?;
            }
            writeln!(out)
        }
        row(out, "startup", "", self.startup)?;
        row(out, "total", "", self.total)?;
        for (label, r) in &self.segments {
            row(out, "segment", label, *r)?;
        }
        if self.segment.1.loops != 0 {
            row(out, "segment", &self.segment.0, self.segment.1)?;
        }
        for r in &self.windows {
            row(out, "window", "", *r)?;
        }
        if !cfg!(target_os = "vita") && self.pending.loops != 0 {
            row(out, "window", "", self.pending)?;
        }
        for r in &self.slow {
            row(out, "slow", "", *r)?;
        }
        out.flush()
    }

    fn write_report(&self, mut out: &mut impl Write, title: &str, outcome: &str) -> io::Result<()> {
        #[cfg(feature = "performance")]
        if let Some(active) = &self.active {
            writeln!(
                out,
                "performance_request\t{}",
                serde_json::to_string(&active.request).map_err(io::Error::other)?
            )?;
            writeln!(
                out,
                "performance_scope\t{}",
                if active.request.frame.is_some() {
                    "fixed_frame_setup"
                } else {
                    "ordinary_session"
                }
            )?;
        }
        writeln!(out, "[reel-probe-report] begin")?;
        self.report(&mut out, title, outcome)?;
        writeln!(out, "[reel-probe-report] end")?;
        self.timeline.report(&mut out)?;
        self.frames.report(&mut out)?;
        let mut native = String::with_capacity(8192);
        if device::append_live(&mut native) {
            writeln!(out, "{native}")?;
        }
        out.flush()
    }
}

#[cfg(feature = "performance")]
impl Drop for Recording {
    fn drop(&mut self) {
        if let Some(active) = self.active.take() {
            let _ = active.request.write_status(
                super::StatusState::Error,
                Some("performance recording ended without a terminal result"),
            );
        }
    }
}
