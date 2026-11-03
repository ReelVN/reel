//! Bounded Body execution retains one working frame and canonical cursor exits.
use super::*;

impl Vm {
    // Retain one working frame across the same bounded driver iterations.
    // Roots and the canonical cursor still publish after every logical step.
    #[inline]
    pub(in crate::executor) fn script_block(
        &mut self,
        working: &mut ScriptFrame,
        meter: &mut Meter,
        depth: usize,
        now: &mut impl FnMut() -> u64,
    ) -> Result<bool, VmError> {
        if self.script_execution_enabled() {
            return self.script_execution_working_block(working, meter, depth, now, false);
        }
        let limit = 32;
        #[cfg(test)]
        let limit = if self.work_probe.legacy_script_publication {
            1
        } else {
            limit
        };
        // The first step, including capture, used the original Script span.
        for _ in 1..limit {
            if !matches!(working.phase, ScriptPhase::Body)
                || working.table_work.is_some()
                || self.active.frames.data.len() != depth
                || self.active.blocked.is_some()
                || self.active.fault_build.is_some()
            {
                // Calls/await/reactive work publish before pushing. Preparation,
                // generators, faults and other continuations resume in driver.
                break;
            }
            // Consume the original next driver clock check. A due query that
            // still has time keeps this exclusively borrowed frame resident.
            if !meter.next_short_step() && meter.deadline_reached(now) {
                return Ok(false);
            }
            let shared = self.body_handoff_enabled();
            let scalar = if shared {
                self.scalar_working_step(working, meter, now)?
            } else {
                self.scalar_step_clocked(meter, now)?
            };
            let Some(Frame::Script(stored)) = self.active.frames.data.last() else {
                unreachable!("scalar execution retains the Body frame")
            };
            if !matches!(stored.phase, ScriptPhase::Body) || stored.table_work.is_some() {
                // Scalar faults have already published their complete exit.
                return Ok(true);
            }
            if !shared {
                copy_cursor(stored, working);
            }
            match scalar {
                scalars::ScalarStep::Done => continue,
                scalars::ScalarStep::Budget => return Ok(false),
                scalars::ScalarStep::General | scalars::ScalarStep::Body(_) => {}
                scalars::ScalarStep::Preparation | scalars::ScalarStep::NamedRead => {
                    unreachable!("a retained Body cursor has no continuation")
                }
            }
            #[cfg(feature = "profiling")]
            self.step_costs.begin_fallback();
            if !step_cost!(
                self,
                Script,
                match scalar {
                    scalars::ScalarStep::Body(opcode) =>
                        self.script_body_decoded_step(working, meter, opcode),
                    _ => self.script_step(working, meter),
                }
            )? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    #[inline]
    pub(in crate::executor) fn script_working_copy(&self, source: &ScriptFrame) -> ScriptFrame {
        #[cfg(test)]
        self.work_probe
            .script_working_captures
            .set(self.work_probe.script_working_captures.get() + 1);
        *source
    }

    // Call/await still own their original canonical Script slot. Before push,
    // only the cursor changed; argument/root identities remain in that slot.
    #[inline]
    pub(in crate::executor) fn publish_script_cursor(
        &mut self,
        index: usize,
        source: &ScriptFrame,
    ) {
        #[cfg(test)]
        self.work_probe
            .script_call_cursor_publications
            .set(self.work_probe.script_call_cursor_publications.get() + 1);
        #[cfg(test)]
        if self.work_probe.legacy_script_publication {
            self.publish_script_frame(index, source);
            return;
        }
        let Frame::Script(stored) = &mut self.active.frames.data[index] else {
            unreachable!("call cursor publication retains its Script slot")
        };
        debug_assert!(stored.id == source.id);
        debug_assert!(matches!(stored.phase, ScriptPhase::Body));
        copy_cursor(source, stored);
    }

    #[inline]
    pub(in crate::executor) fn publish_body_frame(&mut self, index: usize, source: &ScriptFrame) {
        #[cfg(test)]
        if self.work_probe.legacy_script_publication {
            self.publish_script_frame(index, source);
            return;
        }
        // This entry is used only by script_body_step. Ordinary opcodes mutate
        // the cursor/cache and roots; changes to roots use the existing slots.
        // Continuations, faults and variant replacement retain full publication.
        if matches!(source.phase, ScriptPhase::Body)
            && source.table_work.is_none()
            && let Frame::Script(stored) = &mut self.active.frames.data[index]
        {
            // Only this instruction's captured top slot is published; calls
            // can push a successor without replacing their originating frame.
            debug_assert!(stored.id == source.id);
            debug_assert!(matches!(stored.phase, ScriptPhase::Body));
            debug_assert!(stored.table_work.is_none());
            #[cfg(test)]
            self.work_probe
                .script_body_cursor_publications
                .set(self.work_probe.script_body_cursor_publications.get() + 1);
            copy_cursor(source, stored);
            return;
        }
        self.publish_script_frame(index, source);
    }
}

fn copy_cursor(source: &ScriptFrame, target: &mut ScriptFrame) {
    target.pc = source.pc;
    target.instruction_end = source.instruction_end;
    target.member_symbol = source.member_symbol;
    target.instruction = source.instruction;
    target.instruction_pc = source.instruction_pc;
}
