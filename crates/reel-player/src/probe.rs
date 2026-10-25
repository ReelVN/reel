//! Temporary, bounded device recording. Disk IO happens after the session closes.
use crate::story_runtime::StoryRuntime;
use reel_core::profile::{NAMES, Sample};
use std::{
    collections::VecDeque,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    time::Instant,
};

const WINDOWS: usize = 512;
const SLOW: usize = 64;
static SERIAL: AtomicU32 = AtomicU32::new(0);
static VM_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static GPU_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
const VM_NAMES: [&str; 18] = [
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
];
const ABSENT: u64 = u64::MAX;
static VM_REQUESTED: AtomicBool = AtomicBool::new(true);
static VM_SEQUENCE: AtomicU32 = AtomicU32::new(0);
static VM_SESSION: AtomicU32 = AtomicU32::new(0);
static VM_EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
static VM_AT_MS: AtomicU64 = AtomicU64::new(0);
static VM_SAMPLE: [AtomicU64; VM_NAMES.len()] = [const { AtomicU64::new(ABSENT) }; VM_NAMES.len()];

/// The live thread requests one snapshot per second. The event loop otherwise
/// reads only the memory ledger and this flag, without walking VM task frames.
fn publish_vm(story: Option<&StoryRuntime>) {
    if !VM_REQUESTED.swap(false, Ordering::Relaxed) {
        return;
    }
    let sample = vm_sample(story);
    VM_SEQUENCE.fetch_add(1, Ordering::AcqRel);
    for (slot, value) in VM_SAMPLE.iter().zip(sample) {
        slot.store(value, Ordering::Relaxed);
    }
    VM_AT_MS.store(
        VM_EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64,
        Ordering::Relaxed,
    );
    VM_SEQUENCE.fetch_add(1, Ordering::Release);
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
    std::sync::atomic::fence(Ordering::Acquire);
    if sequence != VM_SEQUENCE.load(Ordering::Acquire) {
        return;
    }
    let _ = write!(
        line,
        " vm_sample={} vm_session={} vm_age_ms={} vm_state=",
        sequence / 2,
        VM_SESSION.load(Ordering::Relaxed),
        (VM_EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64).saturating_sub(at_ms)
    );
    for (name, value) in VM_NAMES.into_iter().zip(sample) {
        if value != ABSENT {
            let _ = write!(line, "{name}:{value},");
        }
    }
}

pub(crate) struct Live {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Live {
    pub(crate) fn start() -> io::Result<Self> {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::Builder::new().name("reel-probe".into()).stack_size(64 << 10)
            .spawn(move || {
                use std::fmt::Write as _;
                let start = Instant::now();
                let mut previous = Sample::snapshot();
                let mut line = String::with_capacity(2048);
                eprintln!("[reel-probe] live sampling started; elapsed us and completed calls; nested phases overlap; VM counters cumulative, last_* latest slice; gc_phase idle=0 mark=1 weak=2 sweep=3; fn/pc identify the last selected task; vm_sample advances on main-thread snapshots");
                while !stopped.load(Ordering::Relaxed) {
                    let deadline = Instant::now() + std::time::Duration::from_secs(1);
                    while !stopped.load(Ordering::Relaxed) {
                        let remaining = deadline.saturating_duration_since(Instant::now());
                        if remaining.is_zero() { break; }
                        std::thread::sleep(remaining);
                    }
                    if stopped.load(Ordering::Relaxed) { break; }
                    let current = Sample::snapshot();
                    let delta = current.delta(previous);
                    previous = current;
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
                    append_vm(&mut line);
                    eprintln!("{line}");
                    VM_REQUESTED.store(true, Ordering::Relaxed);
                }
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vm_snapshot_reports_selected_task_execution_and_collection_progress() {
        use reel_engine::{
            Epoch, Session, SessionId,
            aside::{SliceBudget, VmLimits},
        };
        let driver = crate::aside_session::AsideSessionUi::from_source(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            "fn main() { await run(delay(1)); }",
            "probe_wait",
            1,
            "main",
            "en",
            VmLimits::default(),
        )
        .unwrap();
        let mut story = StoryRuntime::new(driver);
        for _ in 0..1000 {
            story.tick(SliceBudget::default(), || 0).unwrap();
            if story.tick_interval() == std::time::Duration::from_secs(1) {
                break;
            }
        }
        assert_eq!(story.tick_interval(), std::time::Duration::from_secs(1));
        let sample = vm_sample(Some(&story));
        let field = |sample: [u64; VM_NAMES.len()], name| {
            sample[VM_NAMES.iter().position(|value| *value == name).unwrap()]
        };
        let vm = story.driver().adapter().vm();
        let location = vm.task_location(vm.task_id().unwrap()).unwrap().unwrap();
        assert_eq!(field(sample, "fn"), u64::from(location.code.function.0));
        assert_eq!(field(sample, "pc"), u64::from(location.pc.0));
        assert_eq!(field(sample, "native_entries"), vm.stats().native_entries);
        assert!(field(sample, "native_entries") > 0);
        assert_eq!(
            field(sample, "last_instructions"),
            u64::from(vm.stats().usage.instructions)
        );
        assert_eq!(field(sample, "stage_ready"), 1);
        assert_eq!(field(sample, "derived_task"), 0);
        let collections = field(sample, "collections");
        let units = field(sample, "gc_units");
        story.driver_mut().unwrap().begin_collection().unwrap();
        assert_ne!(field(vm_sample(Some(&story)), "gc_phase"), 0);
        while story.driver().adapter().vm().heap().phase() != aside_vm::GcPhase::Idle {
            story
                .driver_mut()
                .unwrap()
                .gc_step(Default::default())
                .unwrap();
        }
        let collected = vm_sample(Some(&story));
        assert!(field(collected, "collections") > collections);
        assert!(field(collected, "gc_units") > units);
        assert_eq!(field(collected, "slices"), field(sample, "slices"));
        assert_eq!(field(collected, "gc_phase"), 0);
        assert_eq!(vm_sample(None), [ABSENT; VM_NAMES.len()]);
    }

    #[test]
    fn recording_keeps_bounded_history_and_writes_complete_totals() {
        let live = Live::start().unwrap();
        let mut recording = Recording::new();
        for index in 1..=600 {
            recording.started = Instant::now() - std::time::Duration::from_millis(index * 300);
            drop(reel_core::profile::Scope::new(
                reel_core::profile::Phase::Draw,
            ));
            recording.record(
                Instant::now() - std::time::Duration::from_millis(60),
                None,
                1024,
            );
        }
        assert_eq!(recording.windows.len(), WINDOWS);
        assert_eq!(recording.slow.len(), SLOW);
        assert_eq!(recording.total.loops, 600);
        assert_eq!(
            recording.total.phases.calls[reel_core::profile::Phase::Draw as usize],
            600
        );
        let root = std::env::temp_dir().join(format!("reel-probe-check-{}", std::process::id()));
        let path = recording
            .write(&root, "Window\tlight", 1, "closed")
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let rows: Vec<_> = text.lines().skip(5).collect();
        let width = rows[0].split('\t').count();
        assert!(
            rows[1..]
                .iter()
                .all(|line| line.split('\t').count() == width)
        );
        assert_eq!(
            text.lines()
                .filter(|line| line.starts_with("window\t"))
                .count(),
            WINDOWS
        );
        assert_eq!(
            text.lines()
                .filter(|line| line.starts_with("slow\t"))
                .count(),
            SLOW
        );
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(root).unwrap();
        drop(live);
    }
}
impl Row {
    fn add(&mut self, other: Self) {
        self.elapsed = other.elapsed;
        self.active += other.active;
        self.loops += other.loops;
        self.max_loop = self.max_loop.max(other.max_loop);
        self.max_present_gap = self.max_present_gap.max(other.max_present_gap);
        self.vm = self.vm.max(other.vm);
        self.gpu = self.gpu.max(other.gpu);
        for i in 0..NAMES.len() {
            self.phases.micros[i] += other.phases.micros[i];
            self.phases.calls[i] += other.phases.calls[i];
        }
    }
}

pub(crate) struct Recording {
    started: Instant,
    startup: Row,
    total: Row,
    pending: Row,
    window_start: u64,
    last_present: Option<u64>,
    windows: VecDeque<Row>,
    slow: VecDeque<Row>,
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
            started: Instant::now(),
            startup,
            total: startup,
            pending: Row::default(),
            window_start: 0,
            last_present: None,
            windows: VecDeque::with_capacity(WINDOWS),
            slow: VecDeque::with_capacity(SLOW),
        }
    }

    pub(crate) fn record(&mut self, started: Instant, story: Option<&StoryRuntime>, gpu: u64) {
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
        publish_vm(story);
        let elapsed = self.started.elapsed().as_micros() as u64;
        let active = started.elapsed().as_micros() as u64;
        let phases = Sample::take();
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
        };
        self.total.add(row);
        self.pending.add(row);
        if active >= 50_000 {
            if self.slow.len() == SLOW {
                self.slow.pop_front();
            }
            self.slow.push_back(row);
        }
        if elapsed - self.window_start >= 250_000 {
            if self.windows.len() == WINDOWS {
                self.windows.pop_front();
            }
            self.windows.push_back(std::mem::take(&mut self.pending));
            self.window_start = elapsed;
        }
    }

    pub(crate) fn write(
        self,
        root: &Path,
        title: &str,
        timestamp: u64,
        outcome: &str,
    ) -> io::Result<PathBuf> {
        std::fs::create_dir_all(root)?;
        let path = root.join(format!(
            "{timestamp}-{}.tsv",
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        let mut out = io::BufWriter::new(std::fs::File::create(&path)?);
        writeln!(out, "title\t{}", title.replace(['\t', '\r', '\n'], " "))?;
        writeln!(out, "outcome\t{}", outcome.replace(['\t', '\r', '\n'], " "))?;
        writeln!(
            out,
            "recording_bytes\t{}",
            (WINDOWS + SLOW) * std::mem::size_of::<Row>()
        )?;
        writeln!(
            out,
            "timing\tCPU microseconds; nested phases overlap; read/decode include worker time; present includes CPU wait; idle gaps are included in present_gap"
        )?;
        writeln!(
            out,
            "memory\tVM and GPU resource ledgers; not process or driver memory"
        )?;
        write!(
            out,
            "kind\telapsed_us\tactive_us\tloops\tmax_loop_us\tmax_present_gap_us\tvm_peak_bytes\tgpu_peak_bytes"
        )?;
        for name in NAMES {
            write!(out, "\t{name}_us\t{name}_calls")?;
        }
        writeln!(out)?;
        fn row(out: &mut impl Write, kind: &str, r: Row) -> io::Result<()> {
            write!(
                out,
                "{kind}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                r.elapsed, r.active, r.loops, r.max_loop, r.max_present_gap, r.vm, r.gpu
            )?;
            for i in 0..NAMES.len() {
                write!(out, "\t{}\t{}", r.phases.micros[i], r.phases.calls[i])?;
            }
            writeln!(out)
        }
        row(&mut out, "startup", self.startup)?;
        row(&mut out, "total", self.total)?;
        for r in self.windows {
            row(&mut out, "window", r)?;
        }
        if self.pending.loops != 0 {
            row(&mut out, "window", self.pending)?;
        }
        for r in self.slow {
            row(&mut out, "slow", r)?;
        }
        out.flush()?;
        Ok(path)
    }
}
