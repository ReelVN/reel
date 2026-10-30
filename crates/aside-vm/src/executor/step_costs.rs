//! Sampled execution-stage wall time. Fixed per-VM storage is included in the
//! existing control reservation; snapshots restart these diagnostic counters.

#[cfg(feature = "profiling")]
macro_rules! step_cost {
    ($vm:expr, $phase:ident, $body:expr) => {{
        let phase = $crate::executor::step_costs::Phase::$phase;
        let start = $vm.step_costs.start(phase);
        let result = $body;
        $vm.step_costs.finish(phase, start);
        result
    }};
}
#[cfg(not(feature = "profiling"))]
macro_rules! step_cost {
    ($vm:expr, $phase:ident, $body:expr) => {
        $body
    };
}

#[cfg(feature = "profiling")]
mod enabled {
    use std::{cell::Cell, time::Instant};

    /// Execution-stage order for `Vm::step_costs`. Stages are nested: fetch,
    /// instruction cost and execution are included in scalar/script totals.
    pub const STEP_NAMES: [&str; 6] = ["scalar", "script", "native", "fetch", "cost", "execute"];

    /// One in every 127 stage entries reads the clock. Every entry is counted.
    pub const STEP_SAMPLE_STRIDE: u64 = 127;

    /// Cumulative sampled wall time, including timing overhead. Sample durations
    /// are not extrapolated or subtracted, and nested stages cannot be summed.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct StepCost {
        /// Number of entries into this stage.
        pub calls: u64,
        /// Entries whose wall time was sampled.
        pub samples: u64,
        /// Accumulated wall nanoseconds for sampled entries only.
        pub sample_wall_ns: u64,
    }

    #[derive(Clone, Copy)]
    pub(crate) enum Phase {
        Scalar,
        Script,
        Native,
        Fetch,
        Cost,
        Execute,
    }

    #[derive(Default)]
    pub(crate) struct State([Cell<StepCost>; STEP_NAMES.len()]);

    impl State {
        pub(crate) fn start(&self, phase: Phase) -> Option<Instant> {
            let cell = &self.0[phase as usize];
            let mut row = cell.get();
            let sample = row.calls.is_multiple_of(STEP_SAMPLE_STRIDE);
            row.calls += 1;
            cell.set(row);
            sample.then(Instant::now)
        }
        pub(crate) fn finish(&self, phase: Phase, start: Option<Instant>) {
            if let Some(start) = start {
                let elapsed = start.elapsed().as_nanos() as u64;
                let cell = &self.0[phase as usize];
                let mut row = cell.get();
                row.samples += 1;
                row.sample_wall_ns += elapsed;
                cell.set(row);
            }
        }
    }

    impl super::super::Vm {
        /// Read fixed per-VM stage counters without walking tasks or allocating.
        /// Profiling builds sample one entry per `STEP_SAMPLE_STRIDE`; durations
        /// include clock overhead and nested stages overlap. Restore starts cold.
        pub fn step_costs(&self) -> [StepCost; STEP_NAMES.len()] {
            std::array::from_fn(|i| self.step_costs.0[i].get())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn counts_every_entry_and_samples_only_the_declared_stride() {
            let state = State::default();
            for _ in 0..(2 * STEP_SAMPLE_STRIDE + 1) {
                let start = state.start(Phase::Scalar);
                state.finish(Phase::Scalar, start);
            }
            let row = state.0[Phase::Scalar as usize].get();
            assert_eq!((row.calls, row.samples), (255, 3));
            assert_eq!(state.0[Phase::Script as usize].get(), StepCost::default());
        }
    }
}
#[cfg(feature = "profiling")]
pub(crate) use enabled::{Phase, State};
#[cfg(feature = "profiling")]
pub use enabled::{STEP_NAMES, STEP_SAMPLE_STRIDE, StepCost};
