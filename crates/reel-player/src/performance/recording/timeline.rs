//! Continuous preparation-state intervals, including cold page construction.
use super::{Row, StoryRuntime};
use reel_core::profile::Phase;
use std::collections::VecDeque;
use std::io::{self, Write};

const CAPACITY: usize = 128 * super::SEGMENTS;
const PHASES: [Phase; 12] = [
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
    Phase::Read,
    Phase::Decode,
];

/// Readiness observed after the callback; several kinds of work may coexist.
#[derive(Clone, Copy, PartialEq, Eq)]
struct State {
    stage_ready: bool,
    activities_pending: bool,
    images_pending: bool,
    script_pending: bool,
    derived_pending: bool,
    gc_pending: bool,
}
impl State {
    fn capture(story: Option<&StoryRuntime>) -> Self {
        Self {
            stage_ready: story.is_none_or(|s| s.driver().adapter().stage_ready()),
            activities_pending: story.is_some_and(|s| s.driver().activities_pending()),
            images_pending: story.is_some_and(|s| s.driver().ui().images_block_presentation()),
            script_pending: story.is_some_and(|s| s.driver().has_runnable_tasks()),
            derived_pending: story
                .is_some_and(|s| s.driver().adapter().vm().heap().derived_pending()),
            gc_pending: story.is_some_and(|s| {
                s.driver().adapter().vm().heap().phase() != aside_vm::GcPhase::Idle
            }),
        }
    }
}

struct Loop {
    segment: u64,
    begin: u64,
    elapsed: u64,
    active: u64,
    outside: u64,
    loops: u64,
    max_loop: u64,
    micros: [u64; PHASES.len()],
    presented: u64,
    published: u64,
    state: State,
}

#[derive(Default)]
pub(super) struct Timeline {
    labels: VecDeque<(u64, String)>,
    segment: u64,
    last_elapsed: Option<u64>,
    observed: u64,
    dropped: u64,
    loops: VecDeque<Loop>,
}

impl Timeline {
    pub(super) fn record(&mut self, title: &str, row: &Row, story: Option<&StoryRuntime>) {
        self.push(title, row, State::capture(story));
    }

    fn push(&mut self, title: &str, row: &Row, state: State) {
        self.observed += row.loops;
        if self.labels.back().is_none_or(|(_, label)| label != title) {
            self.segment += 1;
            if self.labels.len() == super::SEGMENTS {
                let (expired, _) = self.labels.pop_front().unwrap();
                while self.loops.front().is_some_and(|r| r.segment <= expired) {
                    self.dropped += self.loops.pop_front().unwrap().loops;
                }
            }
            if self.labels.capacity() == 0 {
                self.labels.reserve_exact(super::SEGMENTS);
            }
            self.labels.push_back((self.segment, title.into()));
        }
        let begin = self
            .last_elapsed
            .replace(row.elapsed)
            .unwrap_or_else(|| row.elapsed.saturating_sub(row.active));
        let outside = row.elapsed.saturating_sub(begin).saturating_sub(row.active);
        let presented = row.phases.calls[Phase::Present as usize];
        let published = row.phases.calls[Phase::ProjectionPublished as usize];
        // Keep each publication and presentation as its own boundary. Adjacent
        // maintenance callbacks with the same readiness share one interval.
        if let Some(last) = self.loops.back_mut()
            && last.segment == self.segment
            && last.state == state
            && last.presented == 0
            && last.published == 0
            && presented == 0
            && published == 0
        {
            last.elapsed = row.elapsed;
            last.active += row.active;
            last.outside += outside;
            last.loops += row.loops;
            last.max_loop = last.max_loop.max(row.max_loop);
            for (slot, phase) in last.micros.iter_mut().zip(PHASES) {
                *slot += row.phases.micros[phase as usize];
            }
            return;
        }
        if self.loops.capacity() == 0 {
            self.loops.reserve_exact(CAPACITY);
        }
        if self.loops.len() == CAPACITY {
            self.dropped += self.loops.pop_front().unwrap().loops;
        }
        self.loops.push_back(Loop {
            segment: self.segment,
            begin,
            elapsed: row.elapsed,
            active: row.active,
            outside,
            loops: row.loops,
            max_loop: row.max_loop,
            micros: PHASES.map(|phase| row.phases.micros[phase as usize]),
            presented,
            published,
            state,
        });
    }

    pub(super) fn bytes(&self) -> usize {
        self.loops.capacity() * size_of::<Loop>()
            + self.labels.capacity() * size_of::<(u64, String)>()
            + self
                .labels
                .iter()
                .map(|(_, title)| title.capacity())
                .sum::<usize>()
    }

    pub(super) fn report(&self, out: &mut impl Write) -> io::Result<()> {
        writeln!(
            out,
            "loop_metadata\tversion=2\tcapacity={CAPACITY}\tobserved_loops={}\tdropped_loops={}\tstate=after_callback\toutside=elapsed_between_callbacks_excluding_callback_wall",
            self.observed, self.dropped
        )?;
        for (id, label) in &self.labels {
            writeln!(
                out,
                "loop_segment\t{id}\t{}",
                label.replace(['\t', '\r', '\n'], " ")
            )?;
        }
        write!(
            out,
            "loop_columns\tsegment\tbegin_us\telapsed_us\tspan_us\tactive_us\toutside_callbacks_us\tloops\tmax_loop_us"
        )?;
        for phase in PHASES {
            write!(out, "\t{}_us", reel_core::profile::NAMES[phase as usize])?;
        }
        writeln!(
            out,
            "\tpresented\tpublished\tstage_ready\tactivities_pending\timages_pending\tscript_pending\tderived_pending\tgc_pending"
        )?;
        for row in &self.loops {
            write!(
                out,
                "loop_sample\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                row.segment,
                row.begin,
                row.elapsed,
                row.elapsed.saturating_sub(row.begin),
                row.active,
                row.outside,
                row.loops,
                row.max_loop
            )?;
            for micros in row.micros {
                write!(out, "\t{micros}")?;
            }
            writeln!(
                out,
                "\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                row.presented,
                row.published,
                u8::from(row.state.stage_ready),
                u8::from(row.state.activities_pending),
                u8::from(row.state.images_pending),
                u8::from(row.state.script_pending),
                u8::from(row.state.derived_pending),
                u8::from(row.state.gc_pending)
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(elapsed: u64, active: u64, presented: bool) -> Row {
        let mut row = Row {
            elapsed,
            active,
            loops: 1,
            max_loop: active,
            ..Default::default()
        };
        row.phases.calls[Phase::Present as usize] = u64::from(presented);
        row.phases.micros[Phase::Tick as usize] = active;
        row
    }

    #[test]
    fn cold_preparation_keeps_work_gaps_and_each_presentation_boundary() {
        let mut timeline = Timeline::default();
        let mut state = State::capture(None);
        state.stage_ready = false;
        state.script_pending = true;
        for index in 1..=100 {
            timeline.push("history", &row(index * 100, 40, false), state);
        }
        timeline.push("history", &row(18_000, 500, false), State::capture(None));
        timeline.push("history", &row(19_000, 300, true), State::capture(None));
        let mut published = row(20_000, 200, false);
        published.phases.calls[Phase::ProjectionPublished as usize] = 1;
        timeline.push("history", &published, State::capture(None));
        timeline.push("history", &row(21_000, 100, false), State::capture(None));
        assert_eq!(timeline.loops.len(), 5);
        let cold = &timeline.loops[0];
        assert_eq!(
            (cold.begin, cold.elapsed, cold.active, cold.loops),
            (60, 10_000, 4000, 100)
        );
        assert_eq!(cold.outside, 5940);
        assert_eq!(cold.micros[0], 4000);
        assert_eq!(
            (timeline.loops[1].active, timeline.loops[1].outside),
            (500, 7500)
        );
        assert_eq!(timeline.loops[2].presented, 1);
        assert_eq!(timeline.loops[3].published, 1);
        assert_eq!(timeline.observed, 104);
        assert_eq!(timeline.dropped, 0);
        let mut report = Vec::new();
        timeline.report(&mut report).unwrap();
        let report = String::from_utf8(report).unwrap();
        let columns = report
            .lines()
            .find(|line| line.starts_with("loop_columns\t"))
            .unwrap()
            .split('\t')
            .count();
        assert!(
            report
                .lines()
                .filter(|line| line.starts_with("loop_sample\t"))
                .all(|line| line.split('\t').count() == columns)
        );
        assert!(report.contains("loop_segment\t1\thistory"));
    }

    #[test]
    fn long_runs_and_label_retirement_account_for_evicted_callbacks() {
        let mut timeline = Timeline::default();
        for index in 1..=CAPACITY + 10 {
            timeline.record("moving", &row(index as u64 * 100, 10, true), None);
        }
        assert_eq!(timeline.loops.len(), CAPACITY);
        assert_eq!(timeline.loops.capacity(), CAPACITY);
        assert_eq!(timeline.dropped, 10);
        for segment in 0..super::super::SEGMENTS + 1 {
            timeline.record(
                &segment.to_string(),
                &row(1_000_000 + segment as u64 * 100, 10, false),
                None,
            );
        }
        assert_eq!(timeline.labels.len(), super::super::SEGMENTS);
        assert_eq!(timeline.loops.len(), super::super::SEGMENTS);
        assert_eq!(
            timeline.observed,
            timeline.dropped + timeline.loops.iter().map(|r| r.loops).sum::<u64>()
        );
        assert!(
            timeline
                .loops
                .iter()
                .all(|row| row.segment >= timeline.labels[0].0)
        );
    }
}
