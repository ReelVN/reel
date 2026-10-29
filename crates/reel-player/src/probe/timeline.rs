//! Short consecutive loop samples expose cadence hidden by segment averages.
use super::{Row, StoryRuntime};
use reel_core::profile::Phase;
use std::collections::VecDeque;
#[cfg(any(target_os = "vita", test))]
use std::io::{self, Write};

const PER_SEGMENT: usize = 32;
const CAPACITY: usize = PER_SEGMENT * super::SEGMENTS;
const PHASES: [Phase; 10] = [
    Phase::Tick,
    Phase::Session,
    Phase::UiSync,
    Phase::Record,
    Phase::Compile,
    Phase::Replay,
    Phase::Present,
    Phase::HostWait,
    Phase::UiStageSync,
    Phase::UiImageRequests,
];

struct Loop {
    elapsed: u64,
    active: u64,
    micros: [u64; PHASES.len()],
    presented: u64,
    published: u64,
    stage_ready: bool,
    activities_pending: bool,
    images_pending: bool,
}

#[derive(Default)]
pub(super) struct Timeline {
    title: String,
    began: u64,
    captured: usize,
    loops: VecDeque<Loop>,
}

impl Timeline {
    pub(super) fn record(&mut self, title: &str, row: &Row, story: Option<&StoryRuntime>) {
        if self.title != title {
            self.title.clear();
            self.title.push_str(title);
            self.began = row.elapsed;
            self.captured = 0;
        }
        // Skip the first quarter second of each label, then keep consecutive
        // loops. Fixed capacity bounds diagnostics independently of run length.
        if row.elapsed.saturating_sub(self.began) < 250_000 || self.captured == PER_SEGMENT {
            return;
        }
        self.captured += 1;
        if self.loops.capacity() == 0 {
            self.loops.reserve_exact(CAPACITY);
        }
        if self.loops.len() == CAPACITY {
            self.loops.pop_front();
        }
        self.loops.push_back(Loop {
            elapsed: row.elapsed,
            active: row.active,
            micros: PHASES.map(|phase| row.phases.micros[phase as usize]),
            presented: row.phases.calls[Phase::Present as usize],
            published: row.phases.calls[Phase::ProjectionPublished as usize],
            stage_ready: story.is_none_or(|s| s.driver().adapter().stage_ready()),
            activities_pending: story.is_some_and(|s| s.driver().activities_pending()),
            images_pending: story.is_some_and(|s| s.driver().ui().images_block_presentation()),
        });
    }

    pub(super) fn bytes(&self) -> usize {
        self.loops.capacity() * size_of::<Loop>() + self.title.capacity()
    }

    #[cfg(any(target_os = "vita", test))]
    pub(super) fn report(&self, out: &mut impl Write) -> io::Result<()> {
        write!(out, "loop_columns\telapsed_us\tactive_us")?;
        for phase in PHASES {
            write!(out, "\t{}_us", reel_core::profile::NAMES[phase as usize])?;
        }
        writeln!(
            out,
            "\tpresented\tpublished\tstage_ready\tactivities_pending\timages_pending"
        )?;
        for row in &self.loops {
            write!(out, "loop_sample\t{}\t{}", row.elapsed, row.active)?;
            for micros in row.micros {
                write!(out, "\t{micros}")?;
            }
            writeln!(
                out,
                "\t{}\t{}\t{}\t{}\t{}",
                row.presented,
                row.published,
                u8::from(row.stage_ready),
                u8::from(row.activities_pending),
                u8::from(row.images_pending)
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_consecutive_loops_after_warmup_and_bounds_long_runs() {
        let mut timeline = Timeline::default();
        for segment in 0..100 {
            let base = segment * 1_000_000;
            let title = segment.to_string();
            for at in 0..100 {
                let row = Row {
                    elapsed: base + at * 10_000,
                    ..Default::default()
                };
                timeline.record(&title, &row, None);
            }
        }
        assert_eq!(timeline.loops.len(), CAPACITY);
        assert_eq!(timeline.loops.capacity(), CAPACITY);
        let latest: Vec<_> = timeline
            .loops
            .iter()
            .rev()
            .take(PER_SEGMENT)
            .map(|r| r.elapsed)
            .collect();
        assert_eq!(latest[0], 99_560_000);
        assert_eq!(latest[PER_SEGMENT - 1], 99_250_000);
        let mut report = Vec::new();
        timeline.report(&mut report).unwrap();
        let report = String::from_utf8(report).unwrap();
        let mut lines = report.lines();
        let columns = lines.next().unwrap().split('\t').count();
        assert!(lines.all(|line| line.split('\t').count() == columns));
    }
}
