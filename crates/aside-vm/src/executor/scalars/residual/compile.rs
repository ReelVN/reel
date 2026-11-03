//! Learning steps are compiled once into compact replay spans.
use super::*;

impl Plan {
    pub(super) fn compile(&mut self) {
        let mut start = 0;
        while start < self.steps.data.len() {
            let mut end = start + 1;
            while end < self.steps.data.len()
                && self.steps.data[end - 1].target == self.steps.data[end].pc
                && self.steps.data[end - 1].work < 32
            {
                end += 1;
            }
            let steps = &self.steps.data[start..end];
            let first = steps[0];
            let last = *steps.last().unwrap();
            if steps.len() == 1 && first.opcode != Opcode::MemberGet {
                start = end;
                continue;
            }
            let mut span = Span {
                pc: first.pc,
                last_pc: last.pc,
                instruction_end: last.end,
                resident_end: steps.iter().map(|step| step.end).max().unwrap(),
                target: last.target,
                instructions: steps.len() as u8,
                #[cfg(feature = "execution-counts")]
                fields: steps
                    .iter()
                    .filter(|step| step.opcode == Opcode::MemberGet)
                    .count() as u8,
                #[cfg(feature = "execution-counts")]
                script_steps: steps
                    .iter()
                    .filter(|step| {
                        matches!(step.opcode, Opcode::MemberGet | Opcode::Eq | Opcode::Ne)
                    })
                    .count() as u8,
                work: steps.iter().map(|step| step.work).sum(),
                bulk_last: last.work >= 32,
                input_start: self.inputs.data.len() as u16,
                input_end: 0,
                write_start: self.writes.data.len() as u16,
                write_end: 0,
                #[cfg(feature = "execution-counts")]
                opcode_start: self.opcodes.data.len() as u16,
                #[cfg(feature = "execution-counts")]
                opcode_end: 0,
            };
            let mut defined = 0u64;
            for step in steps {
                for input in step.inputs.iter().flatten() {
                    if defined & (1u64 << input.register) == 0
                        && !(input.register == 0 && matches!(input.expected, Fact::Receiver))
                        && !self.inputs.data[span.input_start as usize..]
                            .iter()
                            .any(|existing| existing.register == input.register)
                    {
                        self.inputs.data.push(*input);
                    }
                }
                for (register, _) in step.destinations() {
                    defined |= 1u64 << register;
                }
                #[cfg(feature = "execution-counts")]
                if let Some((_, count)) = self.opcodes.data[span.opcode_start as usize..]
                    .iter_mut()
                    .find(|(op, _)| *op == step.opcode)
                {
                    *count += 1;
                } else {
                    self.opcodes.data.push((step.opcode, 1));
                }
            }
            let mut written = 0u64;
            for step in steps.iter().rev() {
                for (register, value) in step.destinations() {
                    if written & (1u64 << register) == 0 {
                        written |= 1u64 << register;
                        self.writes.data.push(Write { register, value });
                    }
                }
            }
            span.input_end = self.inputs.data.len() as u16;
            span.write_end = self.writes.data.len() as u16;
            #[cfg(feature = "execution-counts")]
            {
                span.opcode_end = self.opcodes.data.len() as u16;
            }
            self.spans.data.push(span);
            start = end;
        }
        self.scope_end = self.spans.data.last().unwrap().target;
        self.steps.data.clear();
    }
}
