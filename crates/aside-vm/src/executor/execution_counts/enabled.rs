use crate::{
    bytecode::{INSTRUCTION_SPECS, Opcode, instruction_spec_index},
    executor::{NativePhase, ScriptPhase, Vm},
};
use std::cell::Cell;

/// General script dispatch groups, excluding the scalar fast path. Counts include
/// attempts that yield before work; Exit combines handler selection/application.
pub const SCRIPT_STEP_NAMES: [&str; 10] = [
    "copy",
    "bind",
    "defaults",
    "parameter",
    "install",
    "body",
    "exit",
    "release",
    "compact_begin",
    "other",
];
/// Native dispatch groups, including retry attempts before a host ticket is issued.
pub const NATIVE_STEP_NAMES: [&str; 11] = [
    "planned",
    "copy",
    "bind",
    "defaults",
    "parameter",
    "project",
    "issue",
    "resume",
    "commit",
    "release",
    "other",
];
const OPCODES: usize = INSTRUCTION_SPECS.len();

macro_rules! fields {
    ($( $counter:ident => $field:ident: $doc:literal; )*) => {
        #[derive(Clone, Copy)]
        pub(crate) enum Counter {
            $( $counter, )*
        }
        const FIELDS: usize = [$(stringify!($counter)),*].len();
        /// Integer-only VM diagnostics, reset at successful load/restore. These
        /// values describe work, not time, and have no persistent encoding.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub struct ExecutionCounts {
            /// Process-local heap identity. A restored VM has a fresh identity.
            pub heap_id: u32,
            /// Caller-provided session epoch, paired with heap_id for differences.
            pub epoch: u64,
            /// Admitted bytecode steps; a suspended reactive read can be admitted again.
            pub instructions: u64,
            /// Existing cumulative interpreter work, including internal steps.
            pub work: u64,
            /// Existing completed run_slice calls.
            pub slices: u64,
            /// Existing native entry and resume counts, excluding external host time.
            pub native_entries: u64,
            /// Existing native continuation/resume count.
            pub native_resumes: u64,
            /// Existing observer recomputation starts, excluding initial construction.
            pub derived_starts: u64,
            /// Existing recomputations using compact script frames.
            pub compact_derived_starts: u64,
            /// Existing weak binding cache hits/misses; reads do not keep objects alive.
            pub native_binding_hits: u64,
            /// Existing native binding cache misses or GC-unconfirmed values.
            pub native_binding_misses: u64,
            /// Existing script binding cache hits.
            pub script_binding_hits: u64,
            /// Existing script binding cache misses or GC-unconfirmed values.
            pub script_binding_misses: u64,
            $(#[doc = $doc] pub $field: u64,)*
            /// Dispatch attempts grouped by SCRIPT_STEP_NAMES, including budget retries.
            pub script_steps: [u64; SCRIPT_STEP_NAMES.len()],
            /// Native dispatch attempts grouped by NATIVE_STEP_NAMES.
            pub native_steps: [u64; NATIVE_STEP_NAMES.len()],
            /// Frame-slot API counts; collector scans and bulk relocation are excluded.
            pub slots: SlotCounts,
            opcode_admissions: [u64; OPCODES],
        }
        impl Default for ExecutionCounts {
            fn default() -> Self {
                Self {
                    heap_id: 0,
                    epoch: 0,
                    instructions: 0,
                    work: 0,
                    slices: 0,
                    native_entries: 0,
                    native_resumes: 0,
                    derived_starts: 0,
                    compact_derived_starts: 0,
                    native_binding_hits: 0,
                    native_binding_misses: 0,
                    script_binding_hits: 0,
                    script_binding_misses: 0,
                    $($field: 0,)*
                    script_steps: [0; SCRIPT_STEP_NAMES.len()],
                    native_steps: [0; NATIVE_STEP_NAMES.len()],
                    slots: SlotCounts::default(),
                    opcode_admissions: [0; OPCODES],
                }
            }
        }
        impl ExecutionCounts {
            /// Iterate the existing opcode catalog in its declaration order.
            /// Admissions sum to instructions; this is not a durable index format.
            pub fn opcode_admissions(&self) -> impl Iterator<Item = (Opcode, u64)> + '_ {
                INSTRUCTION_SPECS.iter()
                    .zip(self.opcode_admissions.iter().copied())
                    .map(|(s, n)| (s.opcode, n))
            }
            /// Difference snapshots from the same uninterrupted VM identity.
            /// A restore, identity mismatch or counter decrease returns None.
            pub fn checked_delta(&self, before: &Self) -> Option<Self> {
                if self.heap_id != before.heap_id || self.epoch != before.epoch {
                    return None;
                }
                let mut result = Self {
                    heap_id: self.heap_id,
                    epoch: self.epoch,
                    instructions: self.instructions.checked_sub(before.instructions)?,
                    work: self.work.checked_sub(before.work)?,
                    slices: self.slices.checked_sub(before.slices)?,
                    native_entries: self.native_entries.checked_sub(before.native_entries)?,
                    native_resumes: self.native_resumes.checked_sub(before.native_resumes)?,
                    derived_starts: self.derived_starts.checked_sub(before.derived_starts)?,
                    compact_derived_starts: self.compact_derived_starts
                        .checked_sub(before.compact_derived_starts)?,
                    native_binding_hits: self.native_binding_hits
                        .checked_sub(before.native_binding_hits)?,
                    native_binding_misses: self.native_binding_misses
                        .checked_sub(before.native_binding_misses)?,
                    script_binding_hits: self.script_binding_hits
                        .checked_sub(before.script_binding_hits)?,
                    script_binding_misses: self.script_binding_misses
                        .checked_sub(before.script_binding_misses)?,
                    $($field: self.$field.checked_sub(before.$field)?,)*
                    slots: self.slots.checked_delta(&before.slots)?,
                    ..Default::default()
                };
                for (i, value) in result.script_steps.iter_mut().enumerate() {
                    *value = self.script_steps[i].checked_sub(before.script_steps[i])?;
                }
                for (i, value) in result.native_steps.iter_mut().enumerate() {
                    *value = self.native_steps[i].checked_sub(before.native_steps[i])?;
                }
                for (i, value) in result.opcode_admissions.iter_mut().enumerate() {
                    *value = self.opcode_admissions[i].checked_sub(before.opcode_admissions[i])?;
                }
                Some(result)
            }
        }
        pub(crate) struct State {
            values: [Cell<u64>; FIELDS],
            steps: [Cell<u64>; SCRIPT_STEP_NAMES.len()],
            native_steps: [Cell<u64>; NATIVE_STEP_NAMES.len()],
            opcodes: [Cell<u64>; OPCODES],
        }
        impl Default for State {
            fn default() -> Self {
                Self {
                    values: std::array::from_fn(|_| Cell::new(0)),
                    steps: std::array::from_fn(|_| Cell::new(0)),
                    native_steps: std::array::from_fn(|_| Cell::new(0)),
                    opcodes: std::array::from_fn(|_| Cell::new(0)),
                }
            }
        }
        impl State {
            pub(crate) fn add(&self, counter: Counter, n: u64) {
                let cell = &self.values[counter as usize];
                cell.set(cell.get() + n);
            }
            pub(crate) fn admitted(&self, op: Opcode) {
                let cell = &self.opcodes[instruction_spec_index(op)];
                cell.set(cell.get() + 1);
            }
            pub(in crate::executor) fn step(&self, phase: &ScriptPhase) {
                let index = match phase {
                    ScriptPhase::Copy { .. } => 0,
                    ScriptPhase::Bind(_) | ScriptPhase::Rest => 1,
                    ScriptPhase::Defaults { .. } => 2,
                    ScriptPhase::Parameter { .. } => 3,
                    ScriptPhase::Install { .. } => 4,
                    ScriptPhase::Body => 5,
                    ScriptPhase::Exit { .. } | ScriptPhase::ExitApply { .. } => 6,
                    ScriptPhase::Release { .. } => 7,
                    ScriptPhase::CompactBegin => 8,
                    _ => 9,
                };
                let cell = &self.steps[index];
                cell.set(cell.get() + 1);
            }
            pub(in crate::executor) fn native_step(&self, phase: &NativePhase) {
                let index = match phase {
                    NativePhase::PlannedCheck { .. } | NativePhase::PlannedCopy { .. } => 0,
                    NativePhase::Copy { .. } => 1,
                    NativePhase::Rest | NativePhase::Bind(_) => 2,
                    NativePhase::Defaults { .. }
                        | NativePhase::DefaultCall { .. }
                        | NativePhase::DefaultConstant { .. } => 3,
                    NativePhase::Parameter { .. } => 4,
                    NativePhase::Project { .. } => 5,
                    NativePhase::Issue(_) => 6,
                    NativePhase::Resume(_) => 7,
                    NativePhase::Commit(_) => 8,
                    NativePhase::Release(_) => 9,
                    _ => 10,
                };
                let cell = &self.native_steps[index];
                cell.set(cell.get() + 1);
            }
            fn snapshot(&self) -> ExecutionCounts {
                ExecutionCounts {
                    $($field: self.values[Counter::$counter as usize].get(),)*
                    script_steps: std::array::from_fn(|i| self.steps[i].get()),
                    native_steps: std::array::from_fn(|i| self.native_steps[i].get()),
                    opcode_admissions: std::array::from_fn(|i| self.opcodes[i].get()),
                    ..Default::default()
                }
            }
        }
    }
}
fields! {
    ScriptEntries=>script_entries:"Successfully pushed script frames, including observer and default bodies.";
    NativeFrames=>native_frames:"Successfully pushed native frames; compact commit transfers are separate.";
    GeneratorResumes=>generator_resumes:"Suspended generator frames restored onto the execution stack.";
    ScriptRegisters=>script_register_values:"Declared register slots summed over successful script entries; not bytes written.";
    ScriptBindings=>script_binding_declarations:"Binding declarations summed over successful script entries.";
    ScriptParameters=>script_parameter_declarations:"Signature parameters, including rest and bound receiver, summed over entries.";
    ArgumentCopies=>argument_values_copied:"Script arguments copied at task entry or Copy phase; each has a name and value slot.";
    ParametersPrepared=>parameters_prepared:"Successfully validated and installed script parameters.";
    InstallBindings=>install_bindings_visited:"Binding declarations visited by installation, excluding its terminal probe.";
    InstallClears=>install_values_cleared:"Explicit Value bindings cleared during installation.";
    CompactCommits=>compact_commits:"Successful compact script-to-native commit transfers.";
    BytecodeExecutions=>bytecode_execute_steps:"Actual scalar/general instruction executions, including faulting operations.";
    ReactiveSuspensions=>reactive_read_suspensions:"Admitted reads that suspend to evaluate a dirty derived input.";
}

macro_rules! slot_fields {
    ($( $counter:ident => $field:ident: $doc:literal; )*) => {
        #[derive(Clone, Copy)]
        pub(crate) enum SlotCounter {
            $($counter,)*
        }
        const SLOT_FIELDS: usize = [$(stringify!($counter)),*].len();
        /// Main frame-slot API calls. Read/write/clear/borrow/move/copy counts
        /// include attempts; open/release counters describe successful operations.
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        pub struct SlotCounts {
            $(#[doc = $doc] pub $field: u64,)*
        }
        impl SlotCounts {
            fn checked_delta(&self, before: &Self) -> Option<Self> {
                Some(Self {
                    $($field: self.$field.checked_sub(before.$field)?,)*
                })
            }
        }
        pub(crate) struct SlotState([Cell<u64>; SLOT_FIELDS]);
        impl Default for SlotState {
            fn default() -> Self {
                Self(std::array::from_fn(|_| Cell::new(0)))
            }
        }
        impl SlotState {
            pub(crate) fn add(&self, counter: SlotCounter, n: u64) {
                let cell = &self.0[counter as usize];
                cell.set(cell.get() + n);
            }
            pub(crate) fn snapshot(&self) -> SlotCounts {
                SlotCounts {
                    $($field: self.0[SlotCounter::$counter as usize].get(),)*
                }
            }
        }
    }
}
slot_fields! {
    Reads=>read_calls:"Single indexed read API attempts.";
    PairReads=>pair_read_calls:"Paired read API attempts, using one mapping in the ordinary path.";
    Writes=>write_calls:"Publishing write API attempts; bulk null initialization is counted by open_values.";
    ClearCalls=>clear_calls:"Single or packed clear API attempts.";
    ClearValues=>clear_values:"Requested single/packed clear operands; includes invalid later operands.";
    Borrows=>borrow_calls:"Initialized-window borrow attempts.";
    BorrowValues=>borrow_values:"Requested values in initialized-window borrows.";
    Opens=>open_calls:"Successfully opened windows, including empty windows.";
    OpenValues=>open_values:"Slots null-initialized by successful window opens.";
    EmptyOpens=>empty_opens:"Successfully opened zero-length windows.";
    Releases=>release_calls:"Successfully unregistered windows.";
    ReleaseValues=>release_values:"Slots removed from root registration; release does not overwrite them.";
    Moves=>move_calls:"move_value API attempts, whose writes bypass write/clear entry counts.";
    Copies=>copy_calls:"copy_value API attempts, which also use the read/write entries.";
}

impl Vm {
    /// Read count-only diagnostics without clocks, allocation or task traversal.
    /// These counters require execution-counts and are independent of profiling.
    pub fn execution_counts(&self) -> ExecutionCounts {
        let mut counts = self.execution_counts.snapshot();
        counts.heap_id = self.heap.id().get();
        counts.epoch = self.heap.epoch().0;
        counts.instructions = self.cumulative_instructions;
        counts.work = self.cumulative_work;
        counts.slices = self.slices;
        counts.native_entries = self.native_entries;
        counts.native_resumes = self.native_resumes;
        counts.derived_starts = self.derived_starts;
        counts.compact_derived_starts = self.compact_derived_starts;
        counts.native_binding_hits = self.native_binding_hits;
        counts.native_binding_misses = self.native_binding_misses;
        counts.script_binding_hits = self.script_binding_hits;
        counts.script_binding_misses = self.script_binding_misses;
        counts.slots = self.slots.execution_count_snapshot();
        counts
    }
}
