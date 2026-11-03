//! The same fixed entry transaction for rooted Values and a preinstalled receiver.
use super::*;

impl Vm {
    pub(super) fn value_entry_step(
        &mut self,
        s: &mut PreparationFrame,
        meter: &mut Meter,
        entry: ValueEntry,
        frame: FrameId,
        descriptor: DescriptorId,
    ) -> bool {
        let function = &self.program.package().functions[s.function.index()];
        let signature = function.signature;
        let parameters = &self.program.package().signatures[signature.index()].parameters;
        let count = parameters.len();
        let explicit = s.argument_count as usize;
        if explicit > count {
            return false;
        }
        let layout = self.program.script_parameter_lookup_work();
        let argument_extra = self.type_checks.argument_extra(signature);
        // Every original take remains short; a structural or host type with a
        // larger grant retains the existing resumable parameter machinery.
        if layout + 2 >= 32 || argument_extra + 4 >= 32 {
            return false;
        }
        let parameter_extra: u64 = parameters
            .iter()
            .map(|p| self.type_checks.extra(p.ty))
            .sum();
        let work = (5 + argument_extra) * explicit as u64
            + (6 + layout) * count as u64
            + parameter_extra
            + u64::from(entry.bindings)
            + 5;
        let steps = (2 * explicit + 2 * count + usize::from(entry.bindings) + 4) as u8;
        if !meter.admits_short_steps(steps - 1) || !meter.admits(0, work, 0) {
            return false;
        }
        let Ok(caller) = self.index(frame) else {
            return false;
        };
        let Frame::Script(caller) = self.active.frames.data[caller] else {
            return false;
        };
        let Descriptor::Call(call) = &self.program.package().functions
            [caller.code.function.index()]
        .descriptors[descriptor.index()] else {
            unreachable!()
        };
        let Arguments::Window(window) = call.arguments else {
            return false;
        };
        if call.contract.is_some() || !call.names.is_empty() || window.len as usize != explicit {
            return false;
        }
        // Copy reads all caller operands before any binder/parameter check.
        let mut values = [Value::NULL; 4];
        let prefix = count - explicit;
        if prefix > 1 {
            return false;
        }
        let Ok(source) =
            self.slots
                .borrow_initialized_window(caller.regs, window.start.0, window.len)
        else {
            return false;
        };
        values[prefix..count].copy_from_slice(source);
        if !matches!(self.receiver_prefix(s.env, s.closure, false), Ok(actual) if actual == prefix as u32)
        {
            return false;
        }
        if prefix != 0 {
            let Ok(receiver) = self.slots.read_vm(at(s.bound, 0)) else {
                return false;
            };
            values[0] = receiver;
        }
        // Bind checks explicit arguments before Parameter checks the receiver.
        // With Value storage and no defaults there is no intervening mutation,
        // allocation or callback; the second accepted type check is redundant.
        for index in (prefix..count).chain(0..prefix) {
            if self
                .check_type(parameters[index].ty, values[index])
                .is_err()
            {
                return false;
            }
        }
        if self
            .slots
            .initialize_value_entry(
                &mut self.heap,
                s.args,
                s.bound,
                s.regs,
                &values[..count],
                prefix,
                entry.locals,
                || assert!(meter.take_short_sequence(work, steps)),
            )
            .is_err()
        {
            return false;
        }
        execution_count!(self, ArgumentCopies, explicit as u64);
        execution_count!(self, ParametersPrepared, count as u64);
        execution_count!(self, InstallBindings, u64::from(entry.bindings));
        execution_count!(self, InstallClears, u64::from(entry.clears));
        #[cfg(any(test, feature = "vm-validation-oracle"))]
        {
            self.value_entry_transactions += 1;
            self.value_entry_receivers += prefix as u64;
        }
        #[cfg(feature = "execution-counts")]
        {
            for _ in 0..explicit {
                self.execution_counts.step(&ScriptPhase::Copy {
                    source: ArgSource::Caller { frame, descriptor },
                    index: 0,
                });
            }
            for _ in 0..=explicit {
                self.execution_counts
                    .step(&ScriptPhase::Bind(ArgumentBinding::default()));
            }
            for _ in 0..=count {
                self.execution_counts
                    .step(&ScriptPhase::Defaults { parameter: 0 });
            }
            for _ in 0..count {
                self.execution_counts.step(&ScriptPhase::Parameter {
                    parameter: 0,
                    storage: BindingStorage::Value(Reg(0)),
                });
            }
            for _ in 0..=usize::from(entry.bindings) / 8 {
                self.execution_counts
                    .step(&ScriptPhase::Install { binding: 0 });
            }
        }
        #[cfg(test)]
        self.work_probe
            .scalar_entries
            .set(self.work_probe.scalar_entries.get() + 1);
        s.phase = PreparationPhase::Body;
        true
    }
}
