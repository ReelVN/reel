//! One bounded active Script execution segment across synchronous phase changes.
use super::*;

#[cfg(test)]
fn phase_kind(frame: &ScriptFrame) -> usize {
    if script_preparation::is_preparation(&frame.phase) {
        1
    } else if matches!(frame.phase, ScriptPhase::Body) {
        0
    } else if frame
        .table_work
        .as_ref()
        .is_some_and(|w| w.named_read(frame.member_symbol))
    {
        2
    } else if matches!(
        frame.phase,
        ScriptPhase::Exit { .. } | ScriptPhase::ExitApply { .. } | ScriptPhase::Release { .. }
    ) {
        3
    } else if matches!(
        frame.phase,
        ScriptPhase::Equality { .. } | ScriptPhase::EqualityCleanup { .. }
    ) {
        4
    } else {
        5
    }
}

fn retained(phase: &ScriptPhase) -> bool {
    script_preparation::is_preparation(phase)
        || matches!(
            phase,
            ScriptPhase::Body
                | ScriptPhase::CompactBegin
                | ScriptPhase::Sequence { .. }
                | ScriptPhase::TableValidate
                | ScriptPhase::TableScan
                | ScriptPhase::Equality { .. }
                | ScriptPhase::EqualityCleanup { .. }
                | ScriptPhase::Exit { .. }
                | ScriptPhase::ExitApply { .. }
                | ScriptPhase::Release { .. }
        )
}

impl Vm {
    #[cfg(test)]
    pub(super) fn count_script_driver_phase(&self) {
        let Some(frame) = self.active.frames.data.last() else {
            return;
        };
        let kind = match frame {
            Frame::Script(frame) => phase_kind(frame),
            Frame::Native(_) => 6,
        };
        let mut counts = self.work_probe.driver_phase_entries.get();
        counts[kind] += 1;
        self.work_probe.driver_phase_entries.set(counts);
    }
    #[inline]
    pub(super) fn script_execution_enabled(&self) -> bool {
        #[cfg(test)]
        if self.legacy_script_execution {
            return false;
        }
        #[cfg(test)]
        if self.work_probe.single_scalar_step
            || self.work_probe.legacy_script_publication
            || self.work_probe.general_script_preparation
            || self.work_probe.single_named_read_step
        {
            return false;
        }
        true
    }

    pub(super) fn script_execution_block(
        &mut self,
        meter: &mut Meter,
        now: &mut impl FnMut() -> u64,
    ) -> Result<bool, VmError> {
        let depth = self.active.frames.data.len();
        let Some(Frame::Script(source)) = self.active.frames.data.last() else {
            unreachable!()
        };
        let mut working = self.script_working_copy(source);
        self.script_execution_working_block(&mut working, meter, depth, now, true)
    }

    pub(super) fn script_execution_working_block(
        &mut self,
        working: &mut ScriptFrame,
        meter: &mut Meter,
        depth: usize,
        now: &mut impl FnMut() -> u64,
        first_checked: bool,
    ) -> Result<bool, VmError> {
        // The driver handled these controls before entering. Synchronous
        // Script helpers cannot acquire external control of this exclusive Vm;
        // their allocations report blocked/fault transitions below instead.
        debug_assert!(
            !self.active.cancelling
                && !self.active.cooperative_cancel
                && !self.active.abort_equality
                && self.active.fault_build.is_none()
        );
        let mut preparation = None;
        #[cfg(test)]
        self.work_probe
            .script_execution_entries
            .set(self.work_probe.script_execution_entries.get() + 1);
        for step in usize::from(!first_checked)..32 {
            if step != 0 || !first_checked {
                // A changed activation, a fault/control transition or terminal
                // result returns to the driver before its ordinary preflight.
                if self.script_execution_boundary(working, depth) {
                    break;
                }
                // Every prior driver/short-step boundary retains its original
                // supplied-clock observation. The canonical roots/PC are current.
                if meter.deadline_reached(now) {
                    return Ok(false);
                }
            }
            let keep = retained(&working.phase);
            let preparing = script_preparation::is_preparation(&working.phase);
            if !preparing {
                preparation = None;
            }
            #[cfg(test)]
            {
                let kind = phase_kind(working);
                let mut counts = self.work_probe.script_execution_steps.get();
                counts[kind] += 1;
                self.work_probe.script_execution_steps.set(counts);
            }
            let progressed = if preparing {
                #[cfg(feature = "execution-counts")]
                self.execution_counts.step(&working.phase);
                step_cost!(
                    self,
                    Script,
                    self.preparation_working_step(working, &mut preparation, meter)
                )
            } else if matches!(working.phase, ScriptPhase::Body) {
                match self.scalar_working_step(working, meter, now)? {
                    scalars::ScalarStep::Done => Ok(true),
                    scalars::ScalarStep::Budget => Ok(false),
                    scalars::ScalarStep::Body(opcode) => {
                        #[cfg(feature = "profiling")]
                        self.step_costs.begin_fallback();
                        step_cost!(
                            self,
                            Script,
                            self.script_body_decoded_step(working, meter, opcode)
                        )
                    }
                    scalars::ScalarStep::General => {
                        #[cfg(feature = "profiling")]
                        self.step_costs.begin_fallback();
                        step_cost!(self, Script, self.script_step(working, meter))
                    }
                    _ => unreachable!("a retained Body has no preparation or named cursor"),
                }
            } else if named_reads::eligible(working, meter) {
                #[cfg(feature = "profiling")]
                let span = self.step_costs.start_driver(step_costs::Phase::Script);
                let result = self.named_read_working_step(working, meter);
                #[cfg(feature = "profiling")]
                self.step_costs.finish_driver(span);
                result
            } else {
                step_cost!(self, Script, self.script_step(working, meter))
            }?;
            if !progressed {
                return Ok(false);
            }
            if !keep {
                break;
            }
        }
        Ok(true)
    }

    fn script_execution_boundary(&self, working: &ScriptFrame, depth: usize) -> bool {
        self.active.frames.data.len() != depth
            || !matches!(self.active.frames.data.last(), Some(Frame::Script(frame)) if frame.id == working.id)
            || self.active.blocked.is_some()
            || self.active.terminal.is_some()
            || self.has_unbuilt_script_fault()
    }

    pub(super) fn has_unbuilt_script_fault(&self) -> bool {
        matches!(self.active.frames.data.last(), Some(Frame::Script(frame))
            if matches!(&frame.phase,
                ScriptPhase::Exit { reason: ExitReason::Throw(seed), .. }
                | ScriptPhase::Release { reason: ExitReason::Throw(seed) }
                if seed.fault.is_none()))
    }
}
