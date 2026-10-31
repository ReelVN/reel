//! Production window access and the test-only checked single-step oracle.
use super::*;
use crate::frame_slots::RegisterWindow;

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
    pub(super) fn heap(&self) -> &Heap {
        match self {
            Self::Window(window) => window.heap(),
            #[cfg(test)]
            Self::Checked { heap, .. } => heap,
        }
    }
    pub(super) fn with_heap<T>(&mut self, operation: impl FnOnce(&mut Heap) -> T) -> T {
        match self {
            Self::Window(window) => window.with_heap(operation),
            #[cfg(test)]
            Self::Checked { heap, .. } => operation(heap),
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
