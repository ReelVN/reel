//! Count-only snapshots use the same interval as frame wall times. Counts cover
//! VM operations and host accesses to frame slots; they are not exclusive costs
//! of VmSlice. Missing/cross-identity deltas remain explicitly invalid.
use aside_vm::executor::{ExecutionCounts, NATIVE_STEP_NAMES, SCRIPT_STEP_NAMES};
use std::io::{self, Write};

fn visit(
    counts: &ExecutionCounts,
    mut emit: impl FnMut(&str, u64) -> io::Result<()>,
) -> io::Result<()> {
    macro_rules! fields {
        ($($field:ident),* $(,)?) => {$(
            emit(concat!("vm_", stringify!($field), "_count"), counts.$field)?;
        )*};
    }
    fields!(
        instructions,
        actual_budget_clock_queries,
        work,
        slices,
        native_entries,
        native_resumes,
        derived_starts,
        compact_derived_starts,
        native_binding_hits,
        native_binding_misses,
        script_binding_hits,
        script_binding_misses,
        script_entries,
        native_frames,
        generator_resumes,
        script_register_values,
        script_binding_declarations,
        script_parameter_declarations,
        argument_values_copied,
        parameters_prepared,
        install_bindings_visited,
        install_values_cleared,
        compact_commits,
        bytecode_execute_steps,
        reactive_read_suspensions,
    );
    macro_rules! slots {
        ($($field:ident),* $(,)?) => {$(
            emit(concat!("vm_slots_", stringify!($field), "_count"), counts.slots.$field)?;
        )*};
    }
    slots!(
        read_calls,
        pair_read_calls,
        write_calls,
        clear_calls,
        clear_values,
        borrow_calls,
        borrow_values,
        open_calls,
        open_values,
        empty_opens,
        release_calls,
        release_values,
        move_calls,
        copy_calls
    );
    for (name, value) in SCRIPT_STEP_NAMES.iter().zip(counts.script_steps) {
        emit(&format!("vm_script_{name}_steps_count"), value)?;
    }
    for (name, value) in NATIVE_STEP_NAMES.iter().zip(counts.native_steps) {
        emit(&format!("vm_native_{name}_steps_count"), value)?;
    }
    for (opcode, value) in counts.opcode_admissions() {
        emit(&format!("vm_opcode_{opcode:?}_count"), value)?;
    }
    Ok(())
}

pub(super) fn columns(out: &mut impl Write) -> io::Result<()> {
    write!(out, "\tvm_counts_valid")?;
    visit(&ExecutionCounts::default(), |name, _| {
        write!(out, "\t{name}")
    })
}

pub(super) fn values(out: &mut impl Write, counts: Option<&ExecutionCounts>) -> io::Result<()> {
    write!(out, "\t{}", u8::from(counts.is_some()))?;
    visit(counts.unwrap_or(&ExecutionCounts::default()), |_, value| {
        write!(out, "\t{value}")
    })
}
