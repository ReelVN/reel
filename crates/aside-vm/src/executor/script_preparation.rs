//! Preparation uses a transient view; canonical frame fields and serialized
//! phase/cursors remain in the task stack throughout every step.
use super::*;
mod scalar_entry;
#[cfg(test)]
mod tests;

pub(super) fn is_preparation(phase: &ScriptPhase) -> bool {
    matches!(
        phase,
        ScriptPhase::Copy { .. }
            | ScriptPhase::Bind(_)
            | ScriptPhase::Defaults { .. }
            | ScriptPhase::Parameter { .. }
            | ScriptPhase::Install { .. }
    )
}

#[derive(Clone, Copy)]
enum PreparationPhase {
    Copy {
        source: ArgSource,
        index: u32,
    },
    Bind(ArgumentBinding),
    Defaults {
        parameter: u32,
    },
    Parameter {
        parameter: u32,
        storage: BindingStorage,
    },
    Install {
        binding: u32,
    },
    Rest,
    DefaultCall {
        parameter: u32,
        function: FunctionId,
    },
    DefaultConstant {
        parameter: u32,
        work: constants::ConstantWork,
    },
    GeneratorCreate,
    Body,
}
impl PreparationPhase {
    fn canonical(self) -> ScriptPhase {
        match self {
            Self::Copy { source, index } => ScriptPhase::Copy { source, index },
            Self::Bind(binding) => ScriptPhase::Bind(binding),
            Self::Defaults { parameter } => ScriptPhase::Defaults { parameter },
            Self::Parameter { parameter, storage } => ScriptPhase::Parameter { parameter, storage },
            Self::Install { binding } => ScriptPhase::Install { binding },
            Self::Rest => ScriptPhase::Rest,
            Self::DefaultCall {
                parameter,
                function,
            } => ScriptPhase::DefaultCall {
                parameter,
                function,
            },
            Self::DefaultConstant { parameter, work } => {
                ScriptPhase::DefaultConstant { parameter, work }
            }
            Self::GeneratorCreate => ScriptPhase::GeneratorCreate,
            Self::Body => ScriptPhase::Body,
        }
    }
}

pub(super) struct PreparationFrame {
    function: FunctionId,
    regs: RootRangeToken,
    env: RootRangeToken,
    args: RootRangeToken,
    bound: RootRangeToken,
    argument_count: u32,
    closure: bool,
    shared_parameters: bool,
    parameter_module: ModuleInstanceId,
    phase: PreparationPhase,
}
impl PreparationFrame {
    pub(super) fn capture(s: &ScriptFrame) -> Self {
        let phase = match &s.phase {
            ScriptPhase::Copy { source, index } => PreparationPhase::Copy {
                source: *source,
                index: *index,
            },
            ScriptPhase::Bind(binding) => PreparationPhase::Bind(*binding),
            ScriptPhase::Defaults { parameter } => PreparationPhase::Defaults {
                parameter: *parameter,
            },
            ScriptPhase::Parameter { parameter, storage } => PreparationPhase::Parameter {
                parameter: *parameter,
                storage: *storage,
            },
            ScriptPhase::Install { binding } => PreparationPhase::Install { binding: *binding },
            _ => unreachable!("only canonical preparation phases are captured"),
        };
        Self {
            function: s.code.function,
            regs: s.regs,
            env: s.env,
            args: s.args,
            bound: s.bound,
            argument_count: s.argument_count,
            closure: s.closure,
            shared_parameters: s.shared_parameters,
            parameter_module: s.parameter_module,
            phase,
        }
    }
}

impl Vm {
    #[cfg_attr(not(feature = "profiling"), inline(always))]
    pub(super) fn preparation_step(&mut self, m: &mut Meter) -> Result<bool, VmError> {
        part_cost!(
            self.step_costs,
            ScriptPrepare,
            self.preparation_step_inner(m)
        )
    }
    fn preparation_step_inner(&mut self, m: &mut Meter) -> Result<bool, VmError> {
        let Some(Frame::Script(frame)) = self.active.frames.data.last() else {
            unreachable!("preparation has an active script frame")
        };
        let mut s = PreparationFrame::capture(frame);
        self.preparation_context_step(&mut s, m)
    }

    pub(super) fn preparation_working_step(
        &mut self,
        working: &mut ScriptFrame,
        preparation: &mut Option<PreparationFrame>,
        m: &mut Meter,
    ) -> Result<bool, VmError> {
        let s = preparation.get_or_insert_with(|| PreparationFrame::capture(working));
        let result = part_cost!(
            self.step_costs,
            ScriptPrepare,
            self.preparation_context_step(s, m)
        );
        // Fault publication belongs to the original helper and returns to the
        // driver before this retained preparation view can be used again.
        if !self.has_unbuilt_script_fault() {
            working.phase = s.phase.canonical();
        }
        result
    }

    fn preparation_context_step(
        &mut self,
        s: &mut PreparationFrame,
        m: &mut Meter,
    ) -> Result<bool, VmError> {
        if self.scalar_entry_step(s, m) {
            self.publish_preparation_phase(s.phase);
            return Ok(true);
        }
        let signature = self.program.package().functions[s.function.index()].signature;
        match s.phase {
            PreparationPhase::Copy { source, index } => {
                if !m.take(0, self.argument_work(source, index), 0) {
                    return Ok(false);
                }
                if index < s.argument_count {
                    match self.checked_argument(source, index) {
                        Ok((name, v)) => {
                            self.put(s.args, index * 2, name)?;
                            self.put(s.args, index * 2 + 1, v)?;
                            execution_count!(self, ArgumentCopies, 1);
                            s.phase = PreparationPhase::Copy {
                                source,
                                index: index + 1,
                            };
                        }
                        Err(e) => {
                            let kind = language_fault(e)?;
                            return self.preparation_fault_step(kind);
                        }
                    }
                } else {
                    s.phase = if self.program.package().signatures[signature.index()]
                        .rest
                        .is_some()
                    {
                        PreparationPhase::Rest
                    } else {
                        PreparationPhase::Bind(ArgumentBinding::new(self.receiver_prefix(
                            s.env,
                            s.closure,
                            s.shared_parameters,
                        )?))
                    };
                }
            }
            PreparationPhase::Bind(mut binding) => {
                let result = if binding.index == s.argument_count {
                    if !m.take(0, 1, 0) {
                        return Ok(false);
                    }
                    s.phase = PreparationPhase::Defaults { parameter: 0 };
                    None
                } else if s.shared_parameters {
                    if !m.take(0, 1, 0) {
                        return Ok(false);
                    }
                    let value = self.value(s.args, binding.index * 2 + 1)?;
                    self.put(s.bound, binding.index, value)?;
                    binding.advance(Value::int(-1));
                    Some(ArgumentStep::Ready(Ok(())))
                } else {
                    Some(self.bind_argument_step(
                        signature,
                        s.bound,
                        s.args,
                        &mut binding,
                        m,
                        false,
                    )?)
                };
                if let Some(result) = result {
                    s.phase = PreparationPhase::Bind(binding);
                    match result {
                        ArgumentStep::Blocked => {
                            self.publish_preparation_phase(s.phase);
                            return Ok(false);
                        }
                        ArgumentStep::Ready(Err(kind)) => return self.preparation_fault_step(kind),
                        _ => {}
                    }
                }
            }
            PreparationPhase::Defaults { parameter } => {
                let sig = &self.program.package().signatures[signature.index()];
                let bound = self.slots.read_vm(at(s.bound, parameter)).is_ok();
                let allocation = sig
                    .parameters
                    .get(parameter as usize)
                    .filter(|_| !bound)
                    .and_then(|p| p.default)
                    .map_or(0, |default| match default {
                        ParameterDefault::Constant(id) => self.constant_allocation_bytes(id),
                        _ => 0,
                    });
                let work = 2 + if bound {
                    self.program.script_parameter_lookup_work()
                } else {
                    0
                };
                if !m.take(0, work, allocation) {
                    return Ok(false);
                }
                if parameter as usize == sig.parameters.len() + usize::from(sig.rest.is_some()) {
                    s.phase = PreparationPhase::Install { binding: 0 };
                } else if bound {
                    let storage = self.program.script_parameter_storage(s.function, parameter);
                    s.phase = PreparationPhase::Parameter { parameter, storage };
                } else if let Some(default) = sig.parameters[parameter as usize].default {
                    match default {
                        ParameterDefault::Function(function) => {
                            s.phase = PreparationPhase::DefaultCall {
                                parameter,
                                function,
                            }
                        }
                        ParameterDefault::Constant(id) => {
                            match self.start_constant(id, s.env, [1, 2]) {
                                Ok(Some(value)) => {
                                    self.put(s.bound, parameter, value)?;
                                }
                                Ok(None) => {
                                    s.phase = PreparationPhase::DefaultConstant {
                                        parameter,
                                        work: constants::ConstantWork {
                                            dest: Reg(parameter),
                                            id,
                                            index: 0,
                                        },
                                    }
                                }
                                Err(kind) => return self.preparation_fault_step(kind),
                            }
                        }
                    }
                } else {
                    return self.preparation_fault_step(FaultKind::MissingArgument);
                }
            }
            PreparationPhase::Parameter { parameter, storage } => {
                let bytes = if matches!(storage, BindingStorage::Cell(_)) && !s.shared_parameters {
                    std::mem::size_of::<crate::cell::CellState>() as u64
                } else {
                    0
                };
                let sig = &self.program.package().signatures[signature.index()];
                let p = sig
                    .parameters
                    .get(parameter as usize)
                    .or(sig.rest.as_ref())
                    .unwrap();
                if !m.take(0, 4 + self.type_checks.extra(p.ty), bytes) {
                    return Ok(false);
                }
                let input = self.value(s.bound, parameter)?;
                let contract = self.contract(ModuleId(s.parameter_module.0), p.ty, p.mutable);
                let prepared = self.parameter_value(contract, input, storage, s.shared_parameters);
                match prepared {
                    Ok(value) => {
                        self.put(s.bound, parameter, value)?;
                        self.put(s.regs, storage.reg().0, value)?;
                        execution_count!(self, ParametersPrepared, 1);
                        s.phase = PreparationPhase::Defaults {
                            parameter: parameter + 1,
                        };
                    }
                    Err(kind) => return self.preparation_fault_step(kind),
                }
            }
            PreparationPhase::Install { mut binding } => {
                let batch = 8;
                #[cfg(test)]
                let batch = if self.work_probe.single_install_step {
                    1
                } else {
                    batch
                };
                for step in 0..batch {
                    // The outer loop checked the first logical step. Subsequent
                    // steps stop before the next required clock observation.
                    if step != 0 && !m.next_short_step() {
                        break;
                    }
                    if !m.take(0, 1, 0) {
                        self.publish_preparation_phase(s.phase);
                        return Ok(false);
                    }
                    let f = &self.program.package().functions[s.function.index()];
                    let Some(b) = f.bindings.get(binding as usize) else {
                        s.phase = if f.kind == FunctionKind::Generator {
                            PreparationPhase::GeneratorCreate
                        } else {
                            PreparationPhase::Body
                        };
                        break;
                    };
                    execution_count!(self, InstallBindings, 1);
                    let clear = match (b.initialization, b.storage) {
                        (BindingInitialization::Explicit, BindingStorage::Value(reg)) => Some(reg),
                        _ => None,
                    };
                    // Parameter slots retain their prepared values. Explicit
                    // Value bindings clear even if a default used that register.
                    if let Some(reg) = clear
                        && let Err(error) = self.clear(s.regs, reg.0)
                    {
                        self.publish_preparation_phase(s.phase);
                        return Err(error);
                    }
                    if clear.is_some() {
                        execution_count!(self, InstallClears, 1);
                    }
                    binding += 1;
                    s.phase = PreparationPhase::Install { binding };
                }
            }

            _ => unreachable!("output-only preparation phase"),
        }
        self.publish_preparation_phase(s.phase);
        Ok(true)
    }

    fn publish_preparation_phase(&mut self, phase: PreparationPhase) {
        let Some(Frame::Script(frame)) = self.active.frames.data.last_mut() else {
            unreachable!("preparation retains the same script frame")
        };
        frame.phase = phase.canonical();
    }

    fn preparation_fault_step(&mut self, kind: FaultKind) -> Result<bool, VmError> {
        let Some(Frame::Script(mut frame)) = self.active.frames.data.last().copied() else {
            unreachable!("preparation fault has an active script frame")
        };
        self.preparation_fault(&mut frame, kind)?;
        *self.active.frames.data.last_mut().unwrap() = Frame::Script(frame);
        Ok(true)
    }
}
