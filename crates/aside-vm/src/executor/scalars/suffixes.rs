//! Retire verified lifetime tails only when every original observation fits.
use super::*;

impl Block<'_> {
    pub(super) fn value_suffix(
        &mut self,
        meter: &mut Meter,
        _opcode: Opcode,
        value: Value,
        remaining: u8,
        step: &mut u8,
    ) -> Option<Exit> {
        #[cfg(any(test, feature = "vm-validation-oracle"))]
        if self.legacy_suffixes {
            return None;
        }
        #[cfg(test)]
        if self.single || self.general_clears {
            return None;
        }
        let plan = self
            .program
            .scalar_suffix(self.cursor.function, self.cursor.pc)?;
        let extra = (plan.instructions() - 1) as u8;
        if extra > remaining
            || !meter.admits_short_steps(extra)
            || !self.program.package().functions[self.cursor.function.index()]
                .code
                .resident_range(self.code_window, self.cursor.pc.index(), plan.end.index())
            || !meter.admits(plan.instructions(), plan.work(), 0)
        {
            return None;
        }
        match self.registers.write_suffix(
            plan.destination,
            value,
            &plan.clears[..plan.clear_count as usize],
            plan.dead,
            || {
                assert!(meter.take(plan.instructions(), plan.work(), 0));
                for _ in 0..extra {
                    assert!(meter.next_short_step());
                }
            },
        ) {
            Ok(()) => {}
            // A malformed internal mapping retains the ordinary valid prefix.
            Err(HeapError::OutOfBounds) => return None,
            Err(error) => {
                assert!(meter.take(1, 1, 0));
                instruction_admitted!(self, _opcode);
                execution_count!(self, BytecodeExecutions, 1);
                return Some(Exit::Fault(heap_fault(error)));
            }
        }
        instruction_admitted!(self, _opcode);
        if plan.prefix_count != 0 {
            let _clear_opcode = match plan.prefix_count {
                1 => Opcode::Clear,
                2 => Opcode::Clear2,
                _ => Opcode::Clear3,
            };
            instruction_admitted!(self, _clear_opcode);
        }
        if plan.moved {
            instruction_admitted!(self, Opcode::Move);
            instruction_admitted!(self, Opcode::Clear);
        }
        execution_count!(self, BytecodeExecutions, u64::from(plan.instructions()));
        #[cfg(test)]
        {
            self.work_probe
                .scalar_suffix_groups
                .set(self.work_probe.scalar_suffix_groups.get() + 1);
            self.work_probe
                .scalar_suffix_steps
                .set(self.work_probe.scalar_suffix_steps.get() + u64::from(extra));
        }
        self.cursor.instruction_pc = plan.last_pc;
        self.cursor.instruction_end = plan.end;
        self.cursor.advance(plan.end);
        *step += extra;
        Some(Exit::Done)
    }
}
