//! A single stage is eligible in each slice; counters never read the clock.
use super::{StepPoint, hotspots::Hotspots};
use std::{cell::Cell, time::Instant};

/// Sample calls 0, 127, ... among a stage's eligible entries, not all entries.
pub const STEP_SAMPLE_STRIDE: u64 = 127;
const PHASES: usize = 6;
const RESET: u8 = (STEP_SAMPLE_STRIDE - 1) as u8;

macro_rules! metrics {
    ($($key:ident => $field:ident: $doc:literal;)*) => {
        #[derive(Clone, Copy)]
        enum Metric {
            $($key,)*
        }
        const FIELDS: usize = [$(stringify!($field)),*].len();
        /// Inclusive sampled stage wall time and its exact selection denominators.
        /// Different stages observe different entries/slices and can be nested;
        /// their durations must not be summed or extrapolated into exclusive CPU
        /// shares. Root-slot operations remain inside their caller's duration.
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        pub struct StepCost {
            $(#[doc = $doc] pub $field: u64,)*
        }
        impl StepCost {
            /// Stable diagnostic field order, paired with `metrics`; no durable encoding.
            pub const METRIC_NAMES: [&'static str; FIELDS] = [$(stringify!($field)),*];
            /// Read all fields in `METRIC_NAMES` order without allocation or clocks.
            pub fn metrics(&self) -> [u64; FIELDS] {
                [$(self.$field),*]
            }
            /// Subtract monotonic fields after the caller has validated VM identity.
            /// A decreasing counter returns None; restore resets this diagnostic state.
            pub fn checked_delta(&self, before: &Self) -> Option<Self> {
                Some(Self {
                    $($field: self.$field.checked_sub(before.$field)?,)*
                })
            }
        }
        struct Stage {
            values: [Cell<u64>; FIELDS],
            remaining: Cell<u8>,
            overhead_remaining: Cell<u8>,
        }
        impl Default for Stage {
            fn default() -> Self {
                Self {
                    values: std::array::from_fn(|_| Cell::new(0)),
                    remaining: Cell::new(0),
                    overhead_remaining: Cell::new(0),
                }
            }
        }
        impl Stage {
            #[inline]
            fn add(&self, metric: Metric, value: u64) {
                let cell = &self.values[metric as usize];
                cell.set(cell.get() + value);
            }
            fn snapshot(&self) -> StepCost {
                StepCost {
                    $($field: self.values[Metric::$key as usize].get(),)*
                }
            }
        }
    }
}
metrics! {
    Calls=>calls:"All entries into this stage, including retries and entries outside eligible slices.";
    Samples=>samples:"Selected entries whose end timestamp was recorded; durations remain inclusive wall time.";
    Wall=>sample_wall_ns:"Raw wall nanoseconds for completed samples, including point capture and clock overhead; nested durations overlap.";
    EligibleSlices=>eligible_slices:"run_slice entries selecting this stage, including failed task selection; stages rotate once per slice. Periodic work can correlate with this selection.";
    EligibleCalls=>eligible_calls:"Stage entries during its selected slices. One in STEP_SAMPLE_STRIDE eligible entries is selected; other stages count calls only.";
    Selected=>selected_calls:"Entries selected for timing, including unfinished spans. calls-selected_calls is unmeasured; selected_calls-samples is unfinished/lost timing.";
    Clocks=>clock_queries:"Actual Instant reads issued by stage timing and its overhead observations; excludes driver and supplied budget clocks.";
    Dropped=>location_dropped_samples:"Completed samples whose location could not enter the bounded hotspot collision set; their duration remains in this stage.";
    Missing=>location_missing_samples:"Completed samples without a logical location; not zero-duration or unmeasured samples.";
    OverheadSamples=>overhead_samples:"Completed samples whose post-body counter/hotspot publication was timed, one in STEP_SAMPLE_STRIDE completions. Entry/selection/capture and final overhead-counter publication are excluded.";
    OverheadWall=>overhead_wall_ns:"Raw wall time of those publication observations, including their clock overhead. This is partial observer cost, not a correction or an estimate for unobserved work.";
}

#[derive(Default)]
pub(super) struct Stages {
    rows: [Stage; PHASES],
    selected: Cell<Option<usize>>,
    next: Cell<usize>,
    #[cfg(test)]
    forced: Cell<Option<Option<usize>>>,
}

fn select(remaining: &Cell<u8>) -> bool {
    let count = remaining.get();
    if count == 0 {
        remaining.set(RESET);
        true
    } else {
        remaining.set(count - 1);
        false
    }
}

impl Stages {
    pub(super) fn begin_slice(&self) {
        let phase = self.next.get();
        self.next
            .set(if phase + 1 == PHASES { 0 } else { phase + 1 });
        #[cfg(test)]
        let selected = self.forced.get().unwrap_or(Some(phase));
        #[cfg(not(test))]
        let selected = Some(phase);
        self.selected.set(selected);
        if let Some(phase) = selected {
            self.rows[phase].add(Metric::EligibleSlices, 1);
        }
    }
    pub(super) fn end_slice(&self) {
        self.selected.set(None);
    }
    #[cfg(test)]
    pub(super) fn force(&self, phase: Option<Option<usize>>) {
        self.forced.set(phase);
    }
    #[inline]
    pub(super) fn start(&self, phase: usize) -> Option<Instant> {
        let row = &self.rows[phase];
        row.add(Metric::Calls, 1);
        if self.selected.get() != Some(phase) {
            return None;
        }
        row.add(Metric::EligibleCalls, 1);
        if !select(&row.remaining) {
            return None;
        }
        row.add(Metric::Selected, 1);
        row.add(Metric::Clocks, 1);
        Some(Instant::now())
    }
    #[inline]
    pub(super) fn finish(
        &self,
        phase: usize,
        start: Option<Instant>,
        point: Option<StepPoint>,
        hotspots: &Hotspots,
    ) {
        if let Some(start) = start {
            self.publish(phase, start, point, hotspots);
        }
    }
    fn publish(&self, phase: usize, start: Instant, point: Option<StepPoint>, hotspots: &Hotspots) {
        let ended = Instant::now();
        let row = &self.rows[phase];
        row.add(Metric::Clocks, 1);
        let observe = select(&row.overhead_remaining);
        let overhead = observe.then(|| {
            row.add(Metric::Clocks, 1);
            Instant::now()
        });
        let elapsed = ended.duration_since(start).as_nanos() as u64;
        row.add(Metric::Samples, 1);
        row.add(Metric::Wall, elapsed);
        match point {
            Some(point) => {
                if !hotspots.record(point, phase as u8, elapsed) {
                    row.add(Metric::Dropped, 1);
                }
            }
            None => row.add(Metric::Missing, 1),
        }
        if let Some(overhead) = overhead {
            let ended = Instant::now();
            row.add(Metric::Clocks, 1);
            row.add(Metric::OverheadSamples, 1);
            row.add(
                Metric::OverheadWall,
                ended.duration_since(overhead).as_nanos() as u64,
            );
        }
    }
    pub(super) fn snapshot(&self) -> [StepCost; PHASES] {
        std::array::from_fn(|phase| self.rows[phase].snapshot())
    }
}

#[cfg(test)]
mod tests;
