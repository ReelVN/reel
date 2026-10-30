use super::{Result, log};
use std::{io::Write, time::Duration};

/// Measure instrumentation cost before a package session starts. These batch
/// costs include loop overhead; they are reported rather than subtracted.
pub fn calibrate() {
    const CALLS: u128 = 4096;
    let measure = |scopes: bool| {
        let mut batches = [0; 3];
        for batch in &mut batches {
            let start = std::time::Instant::now();
            for _ in 0..CALLS {
                if scopes {
                    drop(std::hint::black_box(reel_core::profile::Scope::new(
                        reel_core::profile::Phase::Sampler,
                    )));
                } else {
                    std::hint::black_box(std::time::Instant::now());
                }
            }
            *batch = start.elapsed().as_nanos() / CALLS;
        }
        batches.sort_unstable();
        batches
    };
    let clock = measure(false);
    let scope = measure(true);
    log(format_args!(
        "CALIBRATION calls_per_batch={CALLS} batches=3 clock_ns_min_median_max={clock:?} scope_ns_min_median_max={scope:?} includes_loop_overhead=true subtracted=false"
    ));
}

pub struct Frames(Vec<[u64; 6]>);
impl Frames {
    pub fn new(capacity: usize) -> Self {
        Self(Vec::with_capacity(capacity))
    }
    pub fn push(&mut self, durations: [Duration; 5]) {
        let mut row = [0; 6];
        row[..5].copy_from_slice(&durations.map(|duration| duration.as_micros() as u64));
        self.0.push(row);
    }
    pub fn completed(&mut self, index: usize, duration: Duration) {
        self.0[index][5] = duration.as_micros() as u64;
    }
    pub fn report(&self, case: &str, warm_after: usize) -> Result {
        let mut file = std::io::stderr().lock();
        writeln!(file, "[reel-render-report] case={case}")?;
        writeln!(
            file,
            "frame\tbuild_us\tsubmit_us\tpresent_us\tqueue_wait_us\ttotal_us\tcompletion_observed_us"
        )?;
        for (index, row) in self.0.iter().enumerate() {
            writeln!(
                file,
                "{index}\t{}\t{}\t{}\t{}\t{}\t{}",
                row[0], row[1], row[2], row[3], row[4], row[5]
            )?;
        }
        file.flush()?;
        let warm = &self.0[self.0.len().min(warm_after)..];
        log(format_args!(
            "TIMING {case} cold_us={:?} warm_frames={}",
            self.0.first(),
            warm.len()
        ));
        if warm_after > 8 && self.0.len() > 1 {
            self.report_window(
                &format!("{case}-reveal"),
                &self.0[1..self.0.len().min(warm_after)],
            );
        }
        self.report_window(case, warm);
        Ok(())
    }
    fn report_window(&self, case: &str, warm: &[[u64; 6]]) {
        if warm.is_empty() {
            return;
        }
        for (column, name) in [
            "build",
            "submit",
            "present",
            "queue_wait",
            "total",
            "completion_observed",
        ]
        .iter()
        .enumerate()
        {
            let mut values: Vec<_> = warm.iter().map(|row| row[column]).collect();
            values.sort_unstable();
            let percentile = |p: usize| values[(values.len() * p).div_ceil(100) - 1];
            log(format_args!(
                "TIMING {case} {name}_us p50={} p95={} p99={} max={} over_16_667={}",
                percentile(50),
                percentile(95),
                percentile(99),
                values.last().unwrap(),
                values.iter().filter(|&&value| value > 16_667).count()
            ));
        }
    }
}
