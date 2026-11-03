//! Shared synchronous opcode semantics for checked and borrowed Body operands.
use super::*;
use crate::type_checks::TypeChecks;

pub(super) trait Operands {
    fn read(&self, register: u32) -> Result<Value, HeapError>;
    fn write(&mut self, register: u32, value: Value) -> Result<(), HeapError>;
    fn environment(&self, index: u32) -> Result<Value, HeapError>;
    fn heap_read(&self) -> &Heap;
    fn cell_initialize(&mut self, cell: Value, value: Value) -> Result<(), HeapError>;
    fn cell_write(&mut self, cell: Value, value: Value) -> Result<(), HeapError>;
}

pub(super) struct Checked<'a> {
    pub(super) slots: &'a mut FrameSlots,
    pub(super) heap: &'a mut Heap,
    pub(super) registers: RootRangeToken,
    pub(super) environment: RootRangeToken,
}

impl Operands for Checked<'_> {
    fn read(&self, register: u32) -> Result<Value, HeapError> {
        self.slots.read_vm(at(self.registers, register))
    }
    fn write(&mut self, register: u32, value: Value) -> Result<(), HeapError> {
        self.slots
            .write_vm(self.heap, at(self.registers, register), value)
    }
    fn environment(&self, index: u32) -> Result<Value, HeapError> {
        self.slots.read_vm(at(self.environment, index))
    }
    fn heap_read(&self) -> &Heap {
        self.heap
    }
    fn cell_initialize(&mut self, cell: Value, value: Value) -> Result<(), HeapError> {
        self.heap.cell_initialize(cell, value)
    }
    fn cell_write(&mut self, cell: Value, value: Value) -> Result<(), HeapError> {
        self.heap.cell_write(cell, value)
    }
}

pub(super) fn bind(
    operands: &mut impl Operands,
    program: &VerifiedProgram,
    types: &TypeChecks,
    binding: &BindingDef,
    (context, effects): (ContextFlags, Effects),
    opcode: Opcode,
    source: u32,
) -> Result<(), FaultKind> {
    if opcode == Opcode::BindWrite {
        let allowed = if context.derived {
            match binding.storage {
                BindingStorage::Value(_) => true,
                BindingStorage::Cell(register) => {
                    let cell = operands.read(register.0).map_err(heap_fault)?;
                    operands.heap_read().derived_write_allowed(cell)
                }
            }
        } else {
            effects.0 & Effects::WRITE != 0
        };
        if !allowed {
            return Err(FaultKind::Effect);
        }
    }
    let value = operands.read(source).map_err(heap_fault)?;
    if !types
        .accepts(program.package(), operands.heap_read(), binding.ty, value)
        .map_err(heap_fault)?
    {
        return Err(FaultKind::Type);
    }
    match binding.storage {
        BindingStorage::Cell(register) => {
            let cell = operands.read(register.0).map_err(heap_fault)?;
            if opcode == Opcode::BindInit {
                operands.cell_initialize(cell, value)
            } else {
                operands.cell_write(cell, value)
            }
            .map_err(heap_fault)?;
        }
        BindingStorage::Value(register) => {
            let old = operands.read(register.0);
            if opcode == Opcode::BindInit {
                if old.is_ok() {
                    return Err(FaultKind::InvalidState);
                }
            } else {
                old.map_err(heap_fault)?;
                if !binding.mutable {
                    return Err(FaultKind::ReadOnly);
                }
            }
            operands.write(register.0, value).map_err(heap_fault)?;
        }
    }
    Ok(())
}

pub(super) fn capture(
    operands: &mut impl Operands,
    closure: bool,
    destination: u32,
    capture: u32,
) -> Result<(), FaultKind> {
    if !closure {
        return Err(FaultKind::InvalidState);
    }
    let value = operands.environment(0).map_err(heap_fault)?;
    let value = classes::script_closure_value(operands.heap_read(), value).map_err(vm_fault)?;
    let value = operands
        .heap_read()
        .get_field(value, capture + 3)
        .map_err(heap_fault)?;
    operands.write(destination, value).map_err(heap_fault)
}
