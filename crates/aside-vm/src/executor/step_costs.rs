//! Sampled execution-stage wall time. Bounded sample storage is charged at VM
//! load; snapshots restart these diagnostic counters and instruction locations.

#[cfg(feature = "profiling")]
mod driver;
#[cfg(feature = "profiling")]
mod hotspots;
#[cfg(feature = "profiling")]
mod stages;
#[cfg(feature = "profiling")]
pub(crate) use driver::Control;
#[cfg(feature = "profiling")]
pub use driver::{DRIVER_SAMPLE_STRIDE, DriverCost};
#[cfg(feature = "profiling")]
pub use hotspots::{STEP_HOTSPOT_CAPACITY, STEP_MODES, StepHotspot, StepPoint};

#[cfg(feature = "profiling")]
macro_rules! step_cost {
    // A logical event inside an already-observed driver span. Events retain
    // stage eligibility and its bounded event countdown.
    ($state:expr, $phase:ident, $point:expr, $body:expr) => {{
        let phase = $crate::executor::step_costs::Phase::$phase;
        let start = $state.start(phase);
        let point = start.and_then(|_| $point);
        let result = $body;
        $state.finish(phase, start, point);
        result
    }};
    ($vm:expr, $phase:ident, $body:expr) => {{
        let phase = $crate::executor::step_costs::Phase::$phase;
        let driver = $vm.step_costs.start_driver(phase);
        let start = $vm.step_costs.start(phase);
        let point = start.and_then(|_| $vm.step_profile_point());
        let result = $body;
        $vm.step_costs.finish(phase, start, point);
        $vm.step_costs.finish_driver(driver);
        result
    }};
}
#[cfg(not(feature = "profiling"))]
macro_rules! step_cost {
    ($state:expr, $phase:ident, $point:expr, $body:expr) => {
        $body
    };
    ($vm:expr, $phase:ident, $body:expr) => {
        $body
    };
}

#[cfg(feature = "profiling")]
mod enabled {
    use super::super::{Frame, ScriptPhase};
    pub use super::stages::{STEP_SAMPLE_STRIDE, StepCost};
    use super::{STEP_HOTSPOT_CAPACITY, StepHotspot, StepPoint, hotspots::Hotspots};
    use std::time::Instant;

    /// Execution-stage order for `Vm::step_costs`. Stages are nested: fetch,
    /// instruction cost and execution are included in scalar/script totals.
    pub const STEP_NAMES: [&str; 6] = ["scalar", "script", "native", "fetch", "cost", "execute"];

    #[derive(Clone, Copy)]
    pub(crate) enum Phase {
        Scalar,
        Script,
        Native,
        Fetch,
        Cost,
        Execute,
    }

    pub(crate) struct State(super::stages::Stages, Hotspots, super::driver::State);

    impl State {
        pub(crate) fn new(usage: &crate::HeapBudgetUsage) -> Result<Self, crate::HeapError> {
            Ok(Self(
                super::stages::Stages::default(),
                Hotspots::new(usage)?,
                super::driver::State::default(),
            ))
        }
        pub(crate) fn begin_slice(&self) -> Option<super::driver::Slice> {
            self.0.begin_slice();
            self.2.begin()
        }
        #[cfg(test)]
        pub(crate) fn set_step_phase(&self, phase: Option<Option<Phase>>) {
            self.0
                .force(phase.map(|phase| phase.map(|phase| phase as usize)));
        }
        #[cfg(test)]
        pub(crate) fn set_driver_sampling(&self, sampling: Option<bool>) {
            self.2.set_sampling(sampling);
        }
        pub(crate) fn begin_run(&self, slice: &mut Option<super::driver::Slice>) {
            self.2.begin_run(slice);
        }
        pub(crate) fn end_run(&self, slice: &mut Option<super::driver::Slice>) {
            self.2.end_run(slice);
        }
        pub(crate) fn finish_slice(
            &self,
            slice: Option<super::driver::Slice>,
            usage: super::super::SliceUsage,
            queries: u64,
            clock_ns: u64,
        ) {
            self.2.finish(slice, usage, queries, clock_ns);
            self.0.end_slice();
        }
        pub(crate) fn fail_entry(&self, slice: Option<super::driver::Slice>) {
            self.2.fail_entry(slice);
            self.0.end_slice();
        }
        #[inline]
        pub(crate) fn begin_fallback(&self) {
            self.2.begin_fallback();
        }
        #[inline]
        pub(crate) fn control(&self, control: super::Control) {
            self.2.control(control);
        }
        #[inline]
        pub(crate) fn start_driver(&self, phase: Phase) -> Option<super::driver::Span> {
            let index = match phase {
                Phase::Scalar => 0,
                Phase::Script => 1,
                Phase::Native => 2,
                _ => return None,
            };
            self.2.start_span(index)
        }
        #[inline]
        pub(crate) fn finish_driver(&self, span: Option<super::driver::Span>) {
            self.2.finish_span(span);
        }
        #[inline]
        pub(crate) fn start(&self, phase: Phase) -> Option<Instant> {
            self.0.start(phase as usize)
        }
        #[inline]
        pub(crate) fn finish(
            &self,
            phase: Phase,
            start: Option<Instant>,
            point: Option<StepPoint>,
        ) {
            self.0.finish(phase as usize, start, point, &self.1);
        }
    }

    impl super::super::Vm {
        /// Read whole-slice wall observations without allocation or task traversal.
        /// One in every DRIVER_SAMPLE_STRIDE calls is observed in full. Entry/run/
        /// exit and run's outer spans are disjoint within each observation; these
        /// values include instrumentation and scheduling delays, not CPU time.
        /// The stage sampler rotates its eligible phase independently. Successful load/restore resets
        /// this state; compare snapshots only within the same heap and epoch.
        pub fn driver_costs(&self) -> super::DriverCost {
            self.step_costs
                .2
                .snapshot(self.heap.id().get(), self.heap.epoch().0)
        }
        /// Read fixed per-VM stage counters without walking tasks or allocating.
        /// A single eligible phase rotates per slice; one entry per
        /// STEP_SAMPLE_STRIDE eligible calls is selected. Selection can correlate
        /// with periodic work; denominators/missing samples remain explicit and
        /// durations are never extrapolated. All times are inclusive wall time,
        /// include observation costs, and overlap driver/other stage samples.
        /// Root operations are inside caller time, not independently timed.
        /// Restore starts cold; pair differences with the driver's VM identity.
        pub fn step_costs(&self) -> [StepCost; STEP_NAMES.len()] {
            self.step_costs.0.snapshot()
        }
        /// Drain bounded instruction-location samples into caller-owned storage.
        /// Returns observations dropped by full collision sets since the prior
        /// drain. No allocation or logging occurs; restore starts the table cold.
        pub fn drain_step_hotspots(
            &self,
            output: &mut [StepHotspot; STEP_HOTSPOT_CAPACITY],
        ) -> u64 {
            self.step_costs.1.drain(output)
        }
        pub(in crate::executor) fn step_profile_point(&self) -> Option<StepPoint> {
            Some(match self.active.frames.data.last()? {
                Frame::Script(script) => StepPoint {
                    function: script.code.function.0,
                    pc: script.pc.0,
                    native: None,
                    mode: match script.phase {
                        ScriptPhase::Body => 0,
                        ScriptPhase::Copy { .. } => 1,
                        ScriptPhase::Rest => 2,
                        ScriptPhase::Bind(_) => 3,
                        ScriptPhase::Defaults { .. } => 4,
                        ScriptPhase::Parameter { .. } => 5,
                        ScriptPhase::Install { .. } => 6,
                        ScriptPhase::DefaultCall { .. } => 7,
                        ScriptPhase::CompactBegin => 8,
                        ScriptPhase::Exit { .. } => 9,
                        ScriptPhase::ExitApply { .. } => 10,
                        ScriptPhase::Release { .. } => 11,
                        ScriptPhase::GeneratorCreate
                        | ScriptPhase::GeneratorCapture { .. }
                        | ScriptPhase::GeneratorStart { .. }
                        | ScriptPhase::GeneratorRestore { .. } => 13,
                        ScriptPhase::Search(_) => 14,
                        ScriptPhase::Sort(_) => 15,
                        ScriptPhase::Class(_) => 16,
                        ScriptPhase::Constant(_) | ScriptPhase::DefaultConstant { .. } => 17,
                        ScriptPhase::Spread(_) | ScriptPhase::TableSpread(_) => 18,
                        ScriptPhase::KeyPath { .. }
                        | ScriptPhase::TableValidate
                        | ScriptPhase::TableScan => 19,
                        ScriptPhase::HostMethod { .. } => 20,
                        ScriptPhase::Builtin(_)
                        | ScriptPhase::Bulk(_)
                        | ScriptPhase::Sequence { .. } => 21,
                        ScriptPhase::TextBuiltin(_)
                        | ScriptPhase::TextPush { .. }
                        | ScriptPhase::TextFinish { .. }
                        | ScriptPhase::Concat { .. } => 22,
                        ScriptPhase::Equality { .. } | ScriptPhase::EqualityCleanup { .. } => 23,
                    },
                },
                Frame::Native(native) => StepPoint {
                    function: native.location.code.function.0,
                    pc: native.location.pc.0,
                    native: Some(native.native.0),
                    mode: 12,
                },
            })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::executor::*;

        #[test]
        fn native_samples_keep_the_call_site_and_script_execution_keeps_its_pc() {
            for phase in [Phase::Native, Phase::Execute] {
                let mut vm = Vm::load(
                    verify(fixtures::helper_native(), Limits::default()).unwrap(),
                    review_tests::echo_registry(),
                    VmLimits::default(),
                    SessionEpoch(3),
                )
                .unwrap();
                vm.step_costs.set_step_phase(Some(Some(phase)));
                let task = vm
                    .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
                    .unwrap();
                assert!(matches!(
                    vm.run_slice(task, SliceBudget::default(), || 0).unwrap(),
                    RunOutcome::HostCall(_)
                ));
                let mut output = [StepHotspot::default(); STEP_HOTSPOT_CAPACITY];
                assert_eq!(vm.drain_step_hotspots(&mut output), 0);
                if matches!(phase, Phase::Native) {
                    let native = output
                        .iter()
                        .find(|row| row.phase == Phase::Native as u8 && row.samples != 0)
                        .unwrap();
                    assert_eq!(
                        native.point,
                        StepPoint {
                            function: 1,
                            pc: 0,
                            native: Some(0),
                            mode: 12
                        }
                    );
                } else {
                    let execute = output
                        .iter()
                        .find(|row| row.phase == Phase::Execute as u8 && row.samples != 0)
                        .unwrap();
                    assert_eq!(execute.point.function, fixtures::ENTRY.0);
                    assert_eq!(execute.point.mode, 0);
                    let instruction = vm.program.package().functions
                        [execute.point.function as usize]
                        .code
                        .instruction(execute.point.pc as usize)
                        .unwrap()
                        .unwrap();
                    assert_eq!(
                        instruction_spec(instruction.opcode).unwrap().opcode,
                        Opcode::Call
                    );
                }
            }
        }

        #[test]
        fn counts_every_entry_and_samples_only_the_declared_stride() {
            let state = State::new(&crate::HeapBudgetUsage::new(u64::MAX)).unwrap();
            state.0.begin_slice();
            for _ in 0..(2 * STEP_SAMPLE_STRIDE + 1) {
                let start = state.start(Phase::Scalar);
                state.finish(Phase::Scalar, start, None);
            }
            let row = state.0.snapshot()[Phase::Scalar as usize];
            assert_eq!((row.calls, row.samples), (255, 3));
            assert_eq!(
                state.0.snapshot()[Phase::Script as usize],
                StepCost::default()
            );
        }
    }
}
#[cfg(feature = "profiling")]
pub(crate) use enabled::{Phase, State};
#[cfg(feature = "profiling")]
pub use enabled::{STEP_NAMES, STEP_SAMPLE_STRIDE, StepCost};
