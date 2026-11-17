//! Complete a fixed Value argument transaction between original clock points.
use super::*;

mod value;

impl Vm {
    pub(super) fn scalar_entry_step(
        &mut self,
        s: &mut PreparationFrame,
        meter: &mut Meter,
    ) -> bool {
        #[cfg(test)]
        if self.legacy_scalar_entry {
            return false;
        }
        #[cfg(test)]
        if self.work_probe.general_script_preparation || self.work_probe.single_install_step {
            return false;
        }
        let PreparationPhase::Copy {
            source: ArgSource::Caller { frame, descriptor },
            index: 0,
        } = s.phase
        else {
            return false;
        };
        if s.shared_parameters {
            return false;
        }
        let Some(entry) = self.program.value_entry(s.function) else {
            return false;
        };
        if !entry.primitive {
            #[cfg(test)]
            if !self.value_entries {
                return false;
            }
            let entry = *entry;
            return self.value_entry_step(s, meter, entry, frame, descriptor);
        }
        let function = &self.program.package().functions[s.function.index()];
        let signature = function.signature;
        let parameters = &self.program.package().signatures[signature.index()].parameters;
        let count = parameters.len();
        if count != s.argument_count as usize {
            return false;
        }
        let bindings = function.bindings.len();
        let layout = self.program.script_parameter_lookup_work();
        // Defaults is the largest original grant. Every constituent remains a
        // short step even when their combined work is greater than 31.
        if layout + 2 >= 32 {
            return false;
        }
        let work = (11 + layout) * count as u64 + bindings as u64 + 5;
        let steps = (4 * count + bindings + 4) as u8;
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
        if call.contract.is_some() || !call.names.is_empty() || window.len as usize != count {
            return false;
        }
        let mut values = [Value::NULL; 4];
        let Ok(source) =
            self.slots
                .borrow_initialized_window(caller.regs, window.start.0, window.len)
        else {
            return false;
        };
        values[..count].copy_from_slice(source);
        // A closure without captures is still an ordinary register-call target.
        // Only a bound receiver changes the positional parameter prefix.
        if !matches!(self.receiver_prefix(s.env, s.closure, false), Ok(0)) {
            return false;
        }
        // Reading every source precedes all callee type checks, as in Copy.
        // Rejected speculation leaves the original phase to report its fault.
        for (parameter, &value) in parameters.iter().zip(&values[..count]) {
            if self.check_type(parameter.ty, value).is_err() {
                return false;
            }
        }
        if self
            .slots
            .initialize_scalar_entry(
                &self.heap,
                s.args,
                s.bound,
                s.regs,
                &values[..count],
                || assert!(meter.take_short_sequence(work, steps)),
            )
            .is_err()
        {
            return false;
        }
        execution_count!(self, ArgumentCopies, count as u64);
        execution_count!(self, ParametersPrepared, count as u64);
        execution_count!(self, InstallBindings, bindings as u64);
        #[cfg(feature = "execution-counts")]
        {
            // The caller already observed the first Copy attempt. Retain the
            // remaining logical phase admissions while removing their dispatch.
            for _ in 0..count {
                self.execution_counts.step(&ScriptPhase::Copy {
                    source: ArgSource::Caller { frame, descriptor },
                    index: 0,
                });
            }
            for _ in 0..=count {
                self.execution_counts
                    .step(&ScriptPhase::Bind(ArgumentBinding::default()));
                self.execution_counts
                    .step(&ScriptPhase::Defaults { parameter: 0 });
            }
            for _ in 0..count {
                self.execution_counts.step(&ScriptPhase::Parameter {
                    parameter: 0,
                    storage: BindingStorage::Value(Reg(0)),
                });
            }
            for _ in 0..=bindings / 8 {
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
