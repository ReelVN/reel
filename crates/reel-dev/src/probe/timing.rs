use super::{Result, directory, log};
use std::{io::Write, time::Duration};

pub struct Frames(Vec<[u64; 5]>);
impl Frames {
    pub fn new(capacity: usize) -> Self {
        Self(Vec::with_capacity(capacity))
    }
    pub fn push(&mut self, durations: [Duration; 5]) {
        self.0
            .push(durations.map(|duration| duration.as_micros() as u64));
    }
    pub fn report(&self, case: &str, warm_after: usize) -> Result {
        let mut file = std::io::BufWriter::new(std::fs::File::create(
            directory().join(format!("{case}.tsv")),
        )?);
        writeln!(
            file,
            "frame\tbuild_us\tsubmit_us\tpresent_us\tqueue_wait_us\ttotal_us"
        )?;
        for (index, row) in self.0.iter().enumerate() {
            writeln!(
                file,
                "{index}\t{}\t{}\t{}\t{}\t{}",
                row[0], row[1], row[2], row[3], row[4]
            )?;
        }
        file.flush()?;
        let warm = &self.0[self.0.len().min(warm_after)..];
        log(format_args!(
            "TIMING {case} cold_us={:?} warm_frames={}",
            self.0.first(),
            warm.len()
        ));
        if warm.is_empty() {
            return Ok(());
        }
        for (column, name) in ["build", "submit", "present", "queue_wait", "total"]
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
        Ok(())
    }
}
