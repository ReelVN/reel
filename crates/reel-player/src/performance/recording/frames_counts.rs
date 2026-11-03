//! Count-only snapshots use the same interval as frame wall times. Counts cover
//! VM operations and host accesses to frame slots; they are not exclusive costs
//! of VmSlice. Missing/cross-identity deltas remain explicitly invalid.
use super::vm::GcCounts;
use aside_vm::executor::{ExecutionCounts, NATIVE_STEP_NAMES, SCRIPT_STEP_NAMES};
use std::io::{self, Write};

fn visit(
    counts: &ExecutionCounts,
    gc: &GcCounts,
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
        member_get_table_costs,
        member_get_host_costs,
        member_get_class_costs,
        member_get_property_costs,
        member_get_other_costs,
        named_table_starts,
        named_table_hint_trusted,
        named_table_hint_checked,
        named_table_hint_rejected,
        named_table_hint_absent,
        named_table_candidates,
        named_table_steps,
        named_table_equality_steps,
        named_host_methods,
    );
    emit("vm_gc_collections_count", gc.collections)?;
    emit("vm_gc_units_count", gc.units)?;
    emit("vm_heap_chunk_allocations_count", gc.chunk_allocations)?;
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
    visit(
        &ExecutionCounts::default(),
        &GcCounts::default(),
        |name, _| write!(out, "\t{name}"),
    )
}

pub(super) fn values(
    out: &mut impl Write,
    sample: Option<(&ExecutionCounts, &GcCounts)>,
) -> io::Result<()> {
    write!(out, "\t{}", u8::from(sample.is_some()))?;
    let empty_counts = ExecutionCounts::default();
    let empty_gc = GcCounts::default();
    let (counts, gc) = sample.unwrap_or((&empty_counts, &empty_gc));
    visit(counts, gc, |_, value| write!(out, "\t{value}"))
}
