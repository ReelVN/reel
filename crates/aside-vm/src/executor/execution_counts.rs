//! Count-only diagnostics. No clocks, allocation or task traversal occurs here.

#[cfg(feature = "execution-counts")]
macro_rules! execution_count {
    ($vm:expr, $name:ident, $value:expr) => {
        $vm.execution_counts
            .add($crate::executor::execution_counts::Counter::$name, $value);
    };
}
#[cfg(not(feature = "execution-counts"))]
macro_rules! execution_count {
    ($vm:expr, $name:ident, $value:expr) => {};
}
#[cfg(feature = "execution-counts")]
macro_rules! instruction_admitted {
    ($vm:expr, $opcode:expr) => {
        $vm.execution_counts.admitted($opcode);
    };
}
#[cfg(not(feature = "execution-counts"))]
macro_rules! instruction_admitted {
    ($vm:expr, $opcode:expr) => {};
}

#[cfg(feature = "execution-counts")]
mod enabled;
#[cfg(feature = "execution-counts")]
pub(crate) use enabled::{Counter, SlotCounter, SlotState, State};
#[cfg(feature = "execution-counts")]
pub use enabled::{ExecutionCounts, NATIVE_STEP_NAMES, SCRIPT_STEP_NAMES, SlotCounts};

#[cfg(all(test, feature = "execution-counts"))]
mod tests;
