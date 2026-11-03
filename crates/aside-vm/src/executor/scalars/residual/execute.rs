use super::*;

impl Block<'_> {
    pub(in crate::executor::scalars) fn residual_begin(&mut self) {
        if let Some(state) = self.residual {
            state.begin(self);
            if let Some(cell) = state.observation_cell.get()
                && self.registers.observed_cell_value(cell).is_err()
            {
                state.mode.set(Mode::Idle);
                state.guard_fallbacks.set(state.guard_fallbacks.get() + 1);
            }
        }
    }
    pub(in crate::executor::scalars) fn residual_finish(&self, success: bool) {
        if let Some(state) = self.residual {
            state.finish(success);
        }
    }
    pub(in crate::executor::scalars) fn residual_step(
        &mut self,
        meter: &mut Meter,
        decoded: BodyInstruction,
        remaining: u8,
        step_count: &mut u8,
    ) -> Option<Exit> {
        let state = self.residual?;
        let index = match state.mode.get() {
            Mode::Idle => return None,
            Mode::Learning => {
                state.learn(self, decoded);
                if !state.cache.as_ref().unwrap().borrow().learning {
                    state.mode.set(Mode::Idle);
                }
                return None;
            }
            Mode::Plan(index) => index,
        };
        let data = state.cache.as_ref()?.borrow();
        let plan = &data.plans.data[index];
        if self.cursor.pc >= plan.scope_end {
            state.mode.set(Mode::Idle);
            return None;
        }
        let mut index = state.next_span.get();
        while index < plan.spans.data.len() && plan.spans.data[index].pc < self.cursor.pc {
            index += 1;
        }
        state.next_span.set(index);
        let span = *plan.spans.data.get(index)?;
        if span.pc != self.cursor.pc {
            return None;
        }
        let count = u32::from(span.instructions);
        if span.instructions - 1 > remaining {
            return None;
        }
        if !meter.admits_short_steps(span.instructions - 1) {
            state.clock_fallbacks.set(state.clock_fallbacks.get() + 1);
            return None;
        }
        if !self.program.package().functions[self.cursor.function.index()]
            .code
            .resident_range(self.code_window, span.pc.index(), span.resident_end.index())
            || !inputs_match(
                &self.registers,
                &plan.inputs.data[span.input_start as usize..span.input_end as usize],
            )
        {
            return None;
        }
        if !meter.admits(count, span.work, 0) {
            state.budget_fallbacks.set(state.budget_fallbacks.get() + 1);
            return None;
        }
        meter.usage.instructions += count;
        meter.usage.work += span.work;
        meter.usage.required_work = 0;
        meter.usage.required_allocation = 0;
        if meter.budget.deadline_ns.is_some() {
            meter.clock_steps -= span.instructions - 1;
        }
        if span.bulk_last {
            meter.clock_steps = 0;
        }
        for write in &plan.writes.data[span.write_start as usize..span.write_end as usize] {
            let result = if let Some(value) = write.value {
                self.registers.write(write.register, value)
            } else {
                self.registers.clear([write.register])
            };
            result.expect("verified residual register mapping");
        }
        #[cfg(feature = "execution-counts")]
        {
            for &(opcode, count) in
                &plan.opcodes.data[span.opcode_start as usize..span.opcode_end as usize]
            {
                self.execution_counts
                    .admitted_count(opcode, u64::from(count));
            }
            for _ in 0..span.script_steps {
                self.execution_counts.step(&ScriptPhase::Body);
            }
        }
        execution_count!(self, MemberGetClassCosts, u64::from(span.fields));
        execution_count!(self, BytecodeExecutions, u64::from(count));
        self.cursor.instruction_pc = span.last_pc;
        self.cursor.instruction_end = span.instruction_end;
        self.cursor.advance(span.target);
        *step_count += span.instructions - 1;
        state.next_span.set(index + 1);
        if index + 1 == plan.spans.data.len() {
            state.mode.set(Mode::Idle);
        }
        state.spans.set(state.spans.get() + 1);
        state
            .instructions
            .set(state.instructions.get() + u64::from(count));
        Some(Exit::Done)
    }
}

fn inputs_match(registers: &Registers<'_>, inputs: &[Input]) -> bool {
    inputs.iter().all(|input| {
        let Ok(actual) = registers.peek(input.register) else {
            return false;
        };
        match input.expected {
            Fact::Scalar(expected) => actual.same_bits(expected),
            Fact::Receiver => registers.peek(0).is_ok_and(|root| root.same_bits(actual)),
            Fact::Unknown => false,
        }
    })
}
