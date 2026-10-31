//! Whole-slice observations supplement the independent per-stage sampler.
use crate::executor::SliceUsage;
use std::{cell::Cell, time::Instant};
mod gaps;
pub(crate) use gaps::Control;

/// Observe calls 0, 127, 254, ... in full, independently of the step sampler.
pub const DRIVER_SAMPLE_STRIDE: u64 = 127;

macro_rules! totals {
    ($($field:ident: $doc:literal;)*) => {
        /// Cumulative wall observations, not CPU running time. Only selected
        /// slices contribute durations and sampled work; calls/entry_failures
        /// count all entries. Fixed-period sampling can correlate with work.
        ///
        /// Within the same observations, wall = entry + run + exit and run =
        /// scalar + script + native + loop. Budget-clock time is a loop child.
        /// Fetch/cost/execute remain inclusive in the rotating stage sampler.
        /// Observed slices include extra clocks and may reach deadlines sooner.
        /// Calibration is raw empty-clock-pair time, never an automatic correction.
        /// Four disjoint gap durations sum to loop_ns within the same observations.
        /// They reuse existing timestamps, include marker publication and waits,
        /// and are not source-PC attribution or exclusive CPU measurements.
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        pub struct DriverCost {
            /// Process-local VM identity; fresh after successful load/restore.
            pub heap_id: u32,
            /// Session epoch paired with heap_id for snapshot differences.
            pub epoch: u64,
            /// All run_slice entries, including invalid task selections.
            pub calls: u64,
            /// Task selections returning an error, including unobserved calls.
            pub entry_failures: u64,
            $(#[doc = $doc] pub $field: u64,)*
        }
        impl DriverCost {
            /// Difference one uninterrupted VM's snapshots. Identity changes and
            /// counter decreases fail; this diagnostic has no persistent encoding.
            pub fn checked_delta(&self, before: &Self) -> Option<Self> {
                if self.heap_id != before.heap_id || self.epoch != before.epoch {
                    return None;
                }
                Some(Self {
                    heap_id: self.heap_id, epoch: self.epoch,
                    calls: self.calls.checked_sub(before.calls)?,
                    entry_failures: self.entry_failures.checked_sub(before.entry_failures)?,
                    $($field: self.$field.checked_sub(before.$field)?,)*
                })
            }
            fn add_observation(&mut self, row: &Self) {
                $(self.$field += row.$field;)*
            }
        }
    }
}
totals! {
    samples: "Completed whole-slice observations; invalid selections are entry-only observations.";
    wall_ns: "Observed entry-through-exit wall time; excludes sample selection and final counter publication.";
    entry_ns: "Task selection and Meter setup, including empty-clock-pair calibration.";
    run_ns: "Entire interpreter run, including dispatch, loops, clock queries and instrumentation.";
    exit_ns: "Code-window release and cumulative usage/count publication after run returns.";
    scalar_ns: "Complete outer scalar attempts, including attempts returning General and nested fetches.";
    script_ns: "Complete outer script dispatch, including fallback classification/frame copy and nested stages.";
    native_ns: "Complete outer native dispatch, including fallback classification/frame copy; excludes external hosts.";
    loop_ns: "Run wall time outside the three mutually disjoint outer dispatch spans.";
    budget_clock_queries: "Actual supplied-clock calls in observed slices, not profiling clocks.";
    budget_clock_ns: "Observed supplied-clock callback wall time, nested in loop_ns, including timing overhead.";
    observer_clock_queries: "Extra Instant clock reads issued by this observer; excludes stage sampling (StepCost.clock_queries) and supplied clocks.";
    calibration_pairs: "Empty Instant clock pairs measured in observed entries.";
    calibration_ns: "Raw elapsed nanoseconds between those empty clock pairs; already inside entry_ns.";
    sampled_instructions: "Admitted instructions in observed slices; observation overhead can change deadline yields.";
    sampled_work: "Interpreter work in observed slices, for checking their workload distribution.";
    first_dispatch_gap_ns: "Run start to the first outer dispatch; includes preflight and observer publication, not CPU time.";
    first_dispatch_gaps: "Observed runs entering at least one outer dispatch; excludes entry-only failures.";
    scalar_reentry_gap_ns: "After an outer dispatch, wall time until the next Scalar attempt; includes preflight, control work and markers.";
    scalar_reentry_gaps: "Observed outer dispatch gaps whose destination is Scalar, excluding the first dispatch.";
    classified_dispatch_gap_ns: "After an outer dispatch, wall time until Script or Native begins; includes preparation/named routing and markers.";
    classified_dispatch_gaps: "Observed Script/Native handoffs after the first dispatch, irrespective of the prior dispatch kind.";
    final_dispatch_gap_ns: "Last outer dispatch end to run end; an observed run without dispatch puts its entire run here.";
    final_dispatch_gaps: "Completed observed runs, including runs without dispatch; excludes entry-only failures.";
    loop_gap_errors: "Invalid gap nesting/timestamp order or gap-sum closure mismatches; nonzero invalidates the gap attribution.";
    loop_iterations: "Interpreter loop entries in observed slices only; includes attempts ending in a yield or terminal return.";
    loop_cancelling: "Cancelling-branch entries in observed slices; counts attempts, not completed cancellation operations.";
    loop_fault_build: "Fault-build-branch entries in observed slices, including insufficient-budget attempts.";
    loop_abort_equality: "Equality-abort-branch entries in observed slices, including insufficient-budget attempts.";
    loop_cooperative_cancel: "Cooperative-cancel-branch entries in observed slices, including insufficient-budget attempts.";
    loop_unbuilt_fault: "Unbuilt-fault-branch entries in observed slices, including insufficient-budget attempts.";
}

#[derive(Clone, Copy, Default)]
struct Active {
    spans: [u64; 3],
    clocks: u64,
    calibration: u64,
    fallback: Option<Instant>,
}

pub(crate) struct Slice {
    began: Instant,
    run: Option<Instant>,
    after_run: Option<Instant>,
}
pub(crate) struct Span {
    phase: usize,
    began: Instant,
}

#[derive(Default)]
pub(super) struct State {
    calls: Cell<u64>,
    failures: Cell<u64>,
    enabled: Cell<bool>,
    active: Cell<Active>,
    gaps: gaps::State,
    totals: Cell<DriverCost>,
    #[cfg(test)]
    sampling: Cell<Option<bool>>,
}

fn nanos(end: Instant, begin: Instant) -> u64 {
    end.duration_since(begin)
        .as_nanos()
        .min(u128::from(u64::MAX)) as u64
}

impl State {
    pub(super) fn begin(&self) -> Option<Slice> {
        let call = self.calls.get();
        self.calls.set(call + 1);
        let sample = call.is_multiple_of(DRIVER_SAMPLE_STRIDE);
        #[cfg(test)]
        let sample = self.sampling.get().unwrap_or(sample);
        if !sample {
            return None;
        }
        let began = Instant::now();
        let calibration_start = Instant::now();
        let calibration_end = Instant::now();
        self.active.set(Active {
            clocks: 3,
            calibration: nanos(calibration_end, calibration_start),
            ..Default::default()
        });
        self.enabled.set(true);
        Some(Slice {
            began,
            run: None,
            after_run: None,
        })
    }
    #[cfg(test)]
    pub(super) fn set_sampling(&self, sampling: Option<bool>) {
        self.sampling.set(sampling);
    }

    pub(super) fn begin_run(&self, slice: &mut Option<Slice>) {
        if let Some(slice) = slice {
            let began = Instant::now();
            slice.run = Some(began);
            self.gaps.begin(began);
            self.clock();
        }
    }
    pub(super) fn end_run(&self, slice: &mut Option<Slice>) {
        if let Some(slice) = slice {
            let ended = Instant::now();
            slice.after_run = Some(ended);
            self.gaps.end(ended);
            self.clock();
        }
    }
    fn clock(&self) {
        let mut active = self.active.get();
        active.clocks += 1;
        self.active.set(active);
    }
    #[inline]
    pub(super) fn control(&self, control: Control) {
        if self.enabled.get() {
            self.gaps.control(control);
        }
    }
    #[inline]
    pub(super) fn begin_fallback(&self) {
        if self.enabled.get() {
            self.observe_fallback();
        }
    }
    #[cold]
    fn observe_fallback(&self) {
        let began = Instant::now();
        let mut active = self.active.get();
        active.clocks += 1;
        active.fallback = Some(began);
        self.active.set(active);
    }
    #[inline]
    pub(super) fn start_span(&self, phase: usize) -> Option<Span> {
        if !self.enabled.get() {
            return None;
        }
        Some(self.observe_span(phase))
    }
    #[cold]
    fn observe_span(&self, phase: usize) -> Span {
        let mut active = self.active.get();
        let began = if phase != 0 {
            active.fallback.take()
        } else {
            None
        };
        let began = began.unwrap_or_else(|| {
            active.clocks += 1;
            Instant::now()
        });
        self.active.set(active);
        self.gaps.start_span(phase, began);
        Span { phase, began }
    }
    #[inline]
    pub(super) fn finish_span(&self, span: Option<Span>) {
        if let Some(span) = span {
            self.finish_observed_span(span);
        }
    }
    #[cold]
    fn finish_observed_span(&self, span: Span) {
        let ended = Instant::now();
        let mut active = self.active.get();
        active.clocks += 1;
        active.spans[span.phase] += nanos(ended, span.began);
        self.active.set(active);
        self.gaps.end_span(ended);
    }
    pub(super) fn finish(
        &self,
        slice: Option<Slice>,
        usage: SliceUsage,
        queries: u64,
        clock_ns: u64,
    ) {
        let Some(slice) = slice else {
            return;
        };
        let ended = Instant::now();
        let active = self.active.get();
        self.enabled.set(false);
        let run = slice.run.expect("observed run began");
        let after_run = slice.after_run.expect("observed run ended");
        let run_ns = nanos(after_run, run);
        let dispatch = active.spans.iter().sum::<u64>();
        // The three outer dispatch spans never overlap. Saturation preserves a
        // detectable hierarchy failure rather than wrapping if a clock regresses.
        let mut row = DriverCost {
            samples: 1,
            wall_ns: nanos(ended, slice.began),
            entry_ns: nanos(run, slice.began),
            run_ns,
            exit_ns: nanos(ended, after_run),
            scalar_ns: active.spans[0],
            script_ns: active.spans[1],
            native_ns: active.spans[2],
            loop_ns: run_ns.saturating_sub(dispatch),
            budget_clock_queries: queries,
            budget_clock_ns: clock_ns,
            observer_clock_queries: active.clocks + 1 + queries * 2,
            calibration_pairs: 1,
            calibration_ns: active.calibration,
            sampled_instructions: u64::from(usage.instructions),
            sampled_work: usage.work,
            ..Default::default()
        };
        self.gaps.publish(&mut row);
        self.record(&row);
    }
    pub(super) fn fail_entry(&self, slice: Option<Slice>) {
        self.failures.set(self.failures.get() + 1);
        if let Some(slice) = slice {
            let ended = Instant::now();
            let active = self.active.get();
            self.enabled.set(false);
            let wall_ns = nanos(ended, slice.began);
            self.record(&DriverCost {
                samples: 1,
                wall_ns,
                entry_ns: wall_ns,
                observer_clock_queries: active.clocks + 1,
                calibration_pairs: 1,
                calibration_ns: active.calibration,
                ..Default::default()
            });
        }
    }
    fn record(&self, row: &DriverCost) {
        let mut total = self.totals.get();
        total.add_observation(row);
        self.totals.set(total);
    }
    pub(super) fn snapshot(&self, heap_id: u32, epoch: u64) -> DriverCost {
        DriverCost {
            heap_id,
            epoch,
            calls: self.calls.get(),
            entry_failures: self.failures.get(),
            ..self.totals.get()
        }
    }
}

#[cfg(test)]
mod tests;
