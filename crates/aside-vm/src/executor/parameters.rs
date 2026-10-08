//! Shared sequential parameter preparation and native author-value projection.
use super::*;

impl Vm {
    pub(super) fn parameter_value(
        &mut self,
        contract: CellContract,
        input: Value,
        storage: BindingStorage,
        shared: bool,
    ) -> Result<Value, FaultKind> {
        if matches!(storage, BindingStorage::Value(_)) {
            self.check_type(contract.type_id, input)?;
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
            self.check_type(contract.type_id, value)?;
            return Ok(input);
        }
        self.check_type(contract.type_id, input)?;
        let cell = self
            .heap
            .alloc_language_cell(contract)
            .map_err(heap_fault)?;
        self.heap.cell_initialize(cell, input).map_err(heap_fault)?;
        Ok(cell)
    }
    pub(super) fn native_layout_work(&self) -> u64 {
        // Cache entries occupy owned package bytes; this bounds its binary search.
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
                        n.phase = NativePhase::Bind {
                            index: 0,
                            positional: 0,
                        };
                    }
                    Err(e) => self.native_fault(n, heap_fault(e))?,
                }
            }
            NativePhase::Defaults { parameter } => {
                if !m.take(0, self.native_layout_work() + 2, 0) {
                    return Ok(false);
                }
                if parameter == count + u32::from(sig.rest.is_some()) {
                    n.phase = NativePhase::Resume(NativeInput::Entry);
                } else if self.slots.read(at(n.bound, parameter)).is_ok() {
                    let storage = self.program.native_parameter_storage(n.native, parameter)?;
                    n.phase = NativePhase::Parameter { parameter, storage };
                } else if let Some(default) = sig
                    .parameters
                    .get(parameter as usize)
                    .and_then(|p| p.default)
                {
                    n.phase = NativePhase::DefaultCall {
                        parameter,
                        function: default,
                    };
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
                if !m.take(0, 4, bytes) {
                    return Ok(false);
                }
                let p = sig
                    .parameters
                    .get(parameter as usize)
                    .or(sig.rest.as_ref())
                    .unwrap();
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
                if !m.take(0, self.script_push_cost(function, parameter) + 1, 0) {
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
            NativePhase::Project { index, input } => {
                if !m.take(0, self.native_layout_work() + 4, 0) {
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
                    let storage = self.program.native_parameter_storage(n.native, index)?;
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
