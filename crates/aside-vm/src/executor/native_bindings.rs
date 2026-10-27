//! Bounded weak bindings of native methods. Entries never retain a receiver or
//! callable; ordinary heap validation enforces generation and GC cutoffs.
use super::*;

const SLOTS: usize = 256;

#[derive(Clone, Copy)]
struct Binding {
    receiver: u64,
    callable: u64,
    function: u32,
}

pub(super) struct NativeBindings([Binding; SLOTS]);

impl Default for NativeBindings {
    fn default() -> Self {
        Self(
            [Binding {
                receiver: 0,
                callable: 0,
                function: u32::MAX,
            }; SLOTS],
        )
    }
}

impl NativeBindings {
    fn slot(receiver: u64, function: FunctionId) -> usize {
        let hash = receiver.wrapping_mul(0x9e37_79b9_7f4a_7c15)
            ^ u64::from(function.0).wrapping_mul(0x85eb_ca6b);
        ((hash ^ (hash >> 32)) as usize) & (SLOTS - 1)
    }

    pub(super) fn get(&self, receiver: u64, function: FunctionId) -> Option<u64> {
        let binding = self.0[Self::slot(receiver, function)];
        (binding.receiver == receiver && binding.function == function.0).then_some(binding.callable)
    }

    pub(super) fn remember(&mut self, receiver: u64, function: FunctionId, callable: u64) {
        self.0[Self::slot(receiver, function)] = Binding {
            receiver,
            callable,
            function: function.0,
        };
    }
}

// The fixed storage is included by Vm's control-size reservation. Snapshots
// rebuild it cold, and collection never traverses or clears these weak keys.
const _: () = assert!(std::mem::size_of::<NativeBindings>() <= 6_144);
