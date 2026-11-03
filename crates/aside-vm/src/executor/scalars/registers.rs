//! Production window access and the test-only checked single-step oracle.
use super::*;

pub(super) enum Registers<'a> {
    Window(RegisterWindow<'a>),
    #[cfg(test)]
    Checked {
        slots: &'a mut FrameSlots,
        heap: &'a mut Heap,
        range: RootRangeToken,
    },
}

impl Registers<'_> {
    #[cfg(any(test, feature = "vm-validation-oracle"))]
    pub(super) fn write_suffix(
        &mut self,
        destination: u32,
        value: Value,
        clears: &[u32],
        dead: bool,
        admit: impl FnOnce(),
    ) -> Result<(), HeapError> {
        match self {
            Self::Window(window) => window.write_suffix(destination, value, clears, dead, admit),
            #[cfg(test)]
            Self::Checked { .. } => {
                unreachable!("the checked oracle executes original instructions")
            }
        }
    }
    pub(super) fn heap_read(&self) -> &Heap {
        match self {
            Self::Window(window) => window.heap_read(),
            #[cfg(test)]
            Self::Checked { heap, .. } => heap,
        }
    }
    pub(super) fn cell_initialize(&mut self, cell: Value, value: Value) -> Result<(), HeapError> {
        match self {
            Self::Window(window) => window.cell_initialize(cell, value),
            #[cfg(test)]
            Self::Checked { heap, .. } => heap.cell_initialize(cell, value),
        }
    }
    pub(super) fn cell_write(&mut self, cell: Value, value: Value) -> Result<(), HeapError> {
        match self {
            Self::Window(window) => window.cell_write(cell, value),
            #[cfg(test)]
            Self::Checked { heap, .. } => heap.cell_write(cell, value),
        }
    }
    pub(super) fn observed_cell_value(&mut self, cell: Value) -> Result<Option<Value>, HeapError> {
        match self {
            Self::Window(window) => window.observed_cell_value(cell),
            #[cfg(test)]
            Self::Checked { heap, .. } => heap.observed_cell_value(cell),
        }
    }
    pub(super) fn environment(
        &self,
        environment: &crate::frame_slots::EnvironmentWindow<'_>,
        index: u32,
    ) -> Result<Value, HeapError> {
        match self {
            Self::Window(window) => window.read_environment(environment, index),
            #[cfg(test)]
            Self::Checked { .. } => unreachable!("checked scalar oracle has no Body environment"),
        }
    }
    pub(super) fn peek(&self, register: u32) -> Result<Value, HeapError> {
        match self {
            Self::Window(window) => window.peek(register),
            #[cfg(test)]
            Self::Checked { .. } => unreachable!("checked scalar oracle does not speculate"),
        }
    }
    pub(super) fn count_peek_read(&self) {
        match self {
            Self::Window(window) => window.count_peek_read(),
            #[cfg(test)]
            Self::Checked { .. } => unreachable!("checked scalar oracle does not speculate"),
        }
    }
    pub(super) fn count_peek_pair(&self) {
        match self {
            Self::Window(window) => window.count_peek_pair(),
            #[cfg(test)]
            Self::Checked { .. } => unreachable!("checked scalar oracle does not speculate"),
        }
    }
    pub(super) fn read(&self, register: u32) -> Result<Value, HeapError> {
        match self {
            Self::Window(window) => window.read(register),
            #[cfg(test)]
            Self::Checked { slots, range, .. } => slots.read(at(*range, register)),
        }
    }
    pub(super) fn read_pair(&self, first: u32, second: u32) -> Result<(Value, Value), HeapError> {
        match self {
            Self::Window(window) => window.read_pair(first, second),
            #[cfg(test)]
            Self::Checked { slots, range, .. } => slots.read_pair(*range, first, second),
        }
    }
    pub(super) fn cell_read(&self, value: Value) -> Result<Value, HeapError> {
        match self {
            Self::Window(window) => window.cell_read(value),
            #[cfg(test)]
            Self::Checked { heap, .. } => heap.cell_read(value),
        }
    }
    pub(super) fn write(&mut self, register: u32, value: Value) -> Result<(), HeapError> {
        match self {
            Self::Window(window) => window.write(register, value),
            #[cfg(test)]
            Self::Checked { slots, heap, range } => slots.write(heap, at(*range, register), value),
        }
    }
    pub(super) fn clear<const N: usize>(&mut self, registers: [u32; N]) -> Result<(), HeapError> {
        match self {
            Self::Window(window) => window.clear(registers),
            #[cfg(test)]
            Self::Checked { slots, heap, range } => {
                if N == 1 {
                    slots.clear(heap, at(*range, registers[0]))
                } else {
                    slots.clear_registers(heap, *range, registers)
                }
            }
        }
    }
}
