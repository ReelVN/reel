//! Shared sequential parameter preparation and native author-value projection.
use super::*;

impl Vm {
    pub(super) fn check_cell_type(
        &self,
        contract: CellContract,
        value: Value,
    ) -> Result<(), FaultKind> {
        if let Some(ty) = contract.type_id {
            self.check_type(ty, value)
        } else if self
            .heap
            .accepts_builtin(contract.ty, value)
            .map_err(heap_fault)?
        {
            Ok(())
        } else {
            Err(FaultKind::Type)
        }
    }
    // Call only after the current stage's layout query work has been admitted.
    fn native_storage(&self, native: NativeId, parameter: u32) -> Result<BindingStorage, VmError> {
        #[cfg(test)]
        self.work_probe
            .native_layout_queries
            .set(self.work_probe.native_layout_queries.get() + 1);
        Ok(self.program.native_parameter_storage(native, parameter)?)
    }
    pub(super) fn parameter_value(
        &mut self,
        contract: CellContract,
        input: Value,
        storage: BindingStorage,
        shared: bool,
    ) -> Result<Value, FaultKind> {
        if matches!(storage, BindingStorage::Value(_)) {
            self.check_cell_type(contract, input)?;
            return Ok(input);
        }
        if shared {
            let info = self.heap.cell_info(input).map_err(heap_fault)?;
            if !info.initialized {
                return Err(FaultKind::Uninitialized);
            }
            if info.contract != contract {
                return Err(FaultKind::Type);
            }
            let value = self.heap.cell_read(input).map_err(heap_fault)?;
            self.check_cell_type(contract, value)?;
            return Ok(input);
        }
        self.check_cell_type(contract, input)?;
        let cell = self
            .heap
            .alloc_language_cell(contract)
            .map_err(heap_fault)?;
        self.heap.cell_initialize(cell, input).map_err(heap_fault)?;
        Ok(cell)
    }
    pub(super) fn native_layout_work(&self) -> u64 {
        self.program.native_parameter_lookup_work()
    }
    pub(super) fn program_lookup_work(&self) -> u64 {
        // Source-location and fault lookups can inspect other package indexes.
        2 + self
            .program
            .stats()
            .owned_capacity_bytes
            .checked_ilog2()
            .unwrap_or(0) as u64
    }
    pub(super) fn native_parameter_step(
        &mut self,
        n: &mut NativeFrame,
        m: &mut Meter,
    ) -> Result<bool, VmError> {
        let sig = &self.program.package().signatures[n.signature.index()];
        let count = sig.parameters.len() as u32;
        match n.phase {
            NativePhase::Rest => {
                if !m.take(0, 3, std::mem::size_of::<crate::payload::Values>() as u64) {
                    return Ok(false);
                }
                match self.heap.alloc_array(&[]) {
                    Ok(rest) => {
                        self.put(n.bound, count, rest)?;
                        n.phase = NativePhase::Bind(ArgumentBinding::new(u32::from(n.receiver)));
                    }
                    Err(e) => self.native_fault(n, heap_fault(e))?,
                }
            }
            NativePhase::Defaults { parameter } => {
                let literal = sig
                    .parameters
                    .get(parameter as usize)
                    .filter(|_| self.slots.read(at(n.bound, parameter)).is_err())
                    .and_then(|p| match p.default {
                        Some(ParameterDefault::Constant(id)) => Some((id, p.ty)),
                        _ => None,
                    });
                let literal_work = literal.map_or(0, |(_, ty)| 2 + self.type_checks.extra(ty));
                let layout_work = if n.bindings.is_some() {
                    self.native_layout_work()
                } else {
                    0
                };
                let allocation = literal.map_or(0, |(id, _)| self.constant_allocation_bytes(id));
                if !m.take(0, layout_work + 8 + literal_work, allocation) {
                    return Ok(false);
                }
                if parameter == count + u32::from(sig.rest.is_some()) {
                    n.phase = NativePhase::Resume(NativeInput::Entry);
                } else if self.slots.read(at(n.bound, parameter)).is_ok() {
                    let storage = if n.bindings.is_some() {
                        self.native_storage(n.native, parameter)?
                    } else {
                        BindingStorage::Value(Reg(parameter))
                    };
                    n.phase = NativePhase::Parameter { parameter, storage };
                } else if let Some(default) = sig
                    .parameters
                    .get(parameter as usize)
                    .and_then(|p| p.default)
                {
                    if let Some((constant, ty)) = literal {
                        match self.start_constant(constant, n.roots, [0, 1]) {
                            Ok(Some(value)) => match self.check_type(ty, value) {
                                Ok(()) => {
                                    self.put(n.bound, parameter, value)?;
                                    if n.bindings.is_none() {
                                        n.phase = NativePhase::Defaults {
                                            parameter: parameter + 1,
                                        };
                                    }
                                }
                                Err(kind) => self.native_fault(n, kind)?,
                            },
                            Ok(None) => {
                                n.phase = NativePhase::DefaultConstant {
                                    parameter,
                                    work: constants::ConstantWork {
                                        dest: Reg(parameter),
                                        id: constant,
                                        index: 0,
                                    },
                                }
                            }
                            Err(kind) => self.native_fault(n, kind)?,
                        }
                    } else {
                        let ParameterDefault::Function(function) = default else {
                            unreachable!("constant default handled above")
                        };
                        n.phase = NativePhase::DefaultCall {
                            parameter,
                            function,
                        };
                    }
                } else {
                    self.native_fault(n, FaultKind::MissingArgument)?;
                }
            }
            NativePhase::Parameter { parameter, storage } => {
                let bytes = if matches!(storage, BindingStorage::Cell(_)) {
                    std::mem::size_of::<crate::cell::CellState>() as u64
                } else {
                    0
                };
                let p = sig
                    .parameters
                    .get(parameter as usize)
                    .or(sig.rest.as_ref())
                    .unwrap();
                if !m.take(0, 4 + self.type_checks.extra(p.ty), bytes) {
                    return Ok(false);
                }
                let contract = self.contract(ModuleId(n.module.0), p.ty, p.mutable);
                let input = self.value(n.bound, parameter)?;
                match self.parameter_value(contract, input, storage, false) {
                    Ok(value) => {
                        self.put(n.bindings.unwrap_or(n.bound), parameter, value)?;
                        if matches!(storage, BindingStorage::Cell(_)) {
                            self.clear(n.bound, parameter)?;
                        }
                        n.phase = NativePhase::Defaults {
                            parameter: parameter + 1,
                        };
                    }
                    Err(kind) => self.native_fault(n, kind)?,
                }
            }
            NativePhase::DefaultCall {
                parameter,
                function,
            } => {
                let (work, bytes) = self.script_push_cost(function, parameter);
                if !m.take(0, work + 1, bytes) {
                    return Ok(false);
                }
                let top = self.active.frames.data.len() - 1;
                self.active.frames.data[top] = Frame::Native(*n);
                match self.push_script(
                    function,
                    None,
                    ArgSource::Prefix { frame: n.id },
                    parameter,
                    n.owner,
                    inherit(n.context, Context::NoWait),
                    n.effects,
                    ReturnTo::NativeDefault {
                        frame: n.id,
                        parameter,
                    },
                ) {
                    Ok(()) => {}
                    Err(e) => self.native_fault(n, language_fault(e)?)?,
                }
            }
            NativePhase::DefaultConstant {
                parameter,
                mut work,
            } => match self.constant_step(n.roots, [0, 1], &mut work, m)? {
                constants::ConstantStep::Blocked => return Ok(false),
                constants::ConstantStep::Pending => {
                    n.phase = NativePhase::DefaultConstant { parameter, work }
                }
                constants::ConstantStep::Ready(value) => {
                    self.put(n.bound, parameter, value)?;
                    self.finish_constant(n.roots, [0, 1])?;
                    n.phase = NativePhase::Defaults { parameter };
                }
                constants::ConstantStep::Fault(kind) => self.native_fault(n, kind)?,
            },
            NativePhase::Project { index, input } => {
                if !m.take(
                    0,
                    self.native_layout_work() + 4 + self.type_checks.argument_extra(n.signature),
                    0,
                ) {
                    return Ok(false);
                }
                if index == count + u32::from(sig.rest.is_some()) {
                    n.phase = NativePhase::Issue(input);
                } else {
                    let p = sig
                        .parameters
                        .get(index as usize)
                        .or(sig.rest.as_ref())
                        .unwrap();
                    let ty = p.ty;
                    let storage = self.native_storage(n.native, index)?;
                    let binding = self.value(n.bindings.unwrap(), index)?;
                    let value = if matches!(storage, BindingStorage::Cell(_)) {
                        self.heap.cell_read(binding)?
                    } else {
                        binding
                    };
                    match self.check_type(ty, value) {
                        Ok(()) => {
                            self.put(n.bound, index, value)?;
                            n.phase = NativePhase::Project {
                                index: index + 1,
                                input,
                            };
                        }
                        Err(kind) => self.native_fault(n, kind)?,
                    }
                }
            }
            _ => return Err(VmError::Contract("native parameter stage")),
        }
        Ok(true)
    }
}
