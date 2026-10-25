//! Temporary, bounded device recording. Disk IO happens after the session closes.
use reel_core::profile::{NAMES, Sample};
use std::{
    collections::VecDeque,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
    time::Instant,
};

const WINDOWS: usize = 512;
const SLOW: usize = 64;
static SERIAL: AtomicU32 = AtomicU32::new(0);
static VM_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static GPU_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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
                eprintln!("[reel-probe] live sampling started; CPU us and completed calls; nested phases overlap");
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
                    eprintln!("{line}");
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
            thread.thread().unpark();
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
    fn recording_keeps_bounded_history_and_writes_complete_totals() {
        let mut recording = Recording::new();
        for index in 1..=600 {
            recording.started = Instant::now() - std::time::Duration::from_millis(index * 300);
            drop(reel_core::profile::Scope::new(
                reel_core::profile::Phase::Draw,
            ));
            recording.record(
                Instant::now() - std::time::Duration::from_millis(60),
                512,
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

    pub(crate) fn record(&mut self, started: Instant, vm: u64, gpu: u64) {
        VM_BYTES.store(vm, Ordering::Relaxed);
        GPU_BYTES.store(gpu, Ordering::Relaxed);
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
