//! One VM snapshot at the presentation boundary aligns counts, sampled stages
//! and whole-slice observations. Identity changes invalidate all its differences.
#[cfg(feature = "vm-execution-counts")]
use aside_vm::executor::ExecutionCounts;
use aside_vm::executor::Vm;
#[cfg(feature = "vm-profiling")]
use aside_vm::executor::{
    DriverCost, LIFECYCLE_NAMES, LIFECYCLE_OUTER_NAMES, STEP_NAMES, StepCost,
};
use std::io::{self, Write};

#[derive(Clone, Copy, Default)]
pub(super) struct Observation {
    pub(super) gc: GcCounts,
    #[cfg(feature = "vm-execution-counts")]
    pub(super) counts: ExecutionCounts,
    #[cfg(feature = "vm-profiling")]
    pub(super) driver: DriverCost,
    #[cfg(feature = "vm-profiling")]
    pub(super) steps: [StepCost; STEP_NAMES.len()],
}

/// Existing heap counters, read at the same presentation boundary as VM counts.
#[derive(Clone, Copy, Default)]
pub(super) struct GcCounts {
    pub(super) collections: u64,
    pub(super) units: u64,
    pub(super) chunk_allocations: u64,
    #[cfg(feature = "vm-execution-counts")]
    pub(super) work_units: [u64; aside_vm::GC_WORK_NAMES.len()],
}

impl GcCounts {
    fn checked_delta(self, before: Self) -> Option<Self> {
        #[cfg(feature = "vm-execution-counts")]
        let work_units = {
            let mut delta = [0; aside_vm::GC_WORK_NAMES.len()];
            for (index, units) in delta.iter_mut().enumerate() {
                *units = self.work_units[index].checked_sub(before.work_units[index])?;
            }
            delta
        };
        Some(Self {
            collections: self.collections.checked_sub(before.collections)?,
            units: self.units.checked_sub(before.units)?,
            chunk_allocations: self
                .chunk_allocations
                .checked_sub(before.chunk_allocations)?,
            #[cfg(feature = "vm-execution-counts")]
            work_units,
        })
    }
}

impl Observation {
    pub(super) fn capture(vm: &Vm) -> Self {
        let heap = vm.heap().stats();
        Self {
            gc: GcCounts {
                collections: heap.collections,
                units: heap.gc_units,
                chunk_allocations: heap.chunk_allocations,
                #[cfg(feature = "vm-execution-counts")]
                work_units: vm.heap().gc_work_units(),
            },
            #[cfg(feature = "vm-execution-counts")]
            counts: vm.execution_counts(),
            #[cfg(feature = "vm-profiling")]
            driver: vm.driver_costs(),
            #[cfg(feature = "vm-profiling")]
            steps: vm.step_costs(),
        }
    }

    pub(super) fn same_identity(&self, before: &Self) -> bool {
        #[cfg(feature = "vm-execution-counts")]
        if (self.counts.heap_id, self.counts.epoch) != (before.counts.heap_id, before.counts.epoch)
        {
            return false;
        }
        #[cfg(feature = "vm-profiling")]
        if (self.driver.heap_id, self.driver.epoch) != (before.driver.heap_id, before.driver.epoch)
        {
            return false;
        }
        true
    }

    pub(super) fn checked_delta(&self, before: &Self) -> Option<Self> {
        #[cfg(feature = "vm-profiling")]
        let steps = {
            let mut delta = [StepCost::default(); STEP_NAMES.len()];
            for (i, value) in delta.iter_mut().enumerate() {
                *value = self.steps[i].checked_delta(&before.steps[i])?;
            }
            delta
        };
        Some(Self {
            gc: self.gc.checked_delta(before.gc)?,
            #[cfg(feature = "vm-execution-counts")]
            counts: self.counts.checked_delta(&before.counts)?,
            #[cfg(feature = "vm-profiling")]
            driver: self.driver.checked_delta(&before.driver)?,
            #[cfg(feature = "vm-profiling")]
            steps,
        })
    }
}

#[cfg(feature = "vm-profiling")]
fn visit_profile(
    sample: &Observation,
    mut emit: impl FnMut(&str, u64) -> io::Result<()>,
) -> io::Result<()> {
    macro_rules! counters {
        ($($field:ident),* $(,)?) => {$(emit(concat!("vm_driver_", stringify!($field), "_count"), sample.driver.$field)?;)*};
    }
    macro_rules! durations {
        ($($field:ident),* $(,)?) => {$(emit(concat!("vm_driver_", stringify!($field)), sample.driver.$field)?;)*};
    }
    counters!(
        calls,
        samples,
        entry_failures,
        budget_clock_queries,
        observer_clock_queries,
        calibration_pairs,
        sampled_instructions,
        sampled_work,
        first_dispatch_gaps,
        scalar_reentry_gaps,
        classified_dispatch_gaps,
        final_dispatch_gaps,
        loop_gap_errors,
        loop_iterations,
        loop_cancelling,
        loop_fault_build,
        loop_abort_equality,
        loop_cooperative_cancel,
        loop_unbuilt_fault
    );
    durations!(
        wall_ns,
        entry_ns,
        run_ns,
        exit_ns,
        scalar_ns,
        script_ns,
        native_ns,
        loop_ns,
        first_dispatch_gap_ns,
        scalar_reentry_gap_ns,
        classified_dispatch_gap_ns,
        final_dispatch_gap_ns,
        budget_clock_ns,
        calibration_ns
    );
    let parts = &sample.driver.lifecycle;
    for (name, row) in LIFECYCLE_NAMES.iter().zip(parts.parts) {
        emit(&format!("vm_driver_part_{name}_ns"), row.wall_ns)?;
        emit(&format!("vm_driver_part_{name}_calls_count"), row.calls)?;
    }
    for (name, value) in LIFECYCLE_OUTER_NAMES
        .iter()
        .zip(parts.unattributed_by_outer_ns)
    {
        emit(&format!("vm_driver_parts_unattributed_{name}_ns"), value)?;
    }
    for (name, value) in [
        ("run_ns", parts.run_ns),
        ("unattributed_ns", parts.unattributed_ns),
        ("observer_ns", parts.observer_ns),
        ("observer_clock_queries_count", parts.observer_clock_queries),
        ("errors_count", parts.errors),
    ] {
        emit(&format!("vm_driver_parts_{name}"), value)?;
    }
    for (name, row) in STEP_NAMES.iter().zip(sample.steps) {
        for (metric, value) in StepCost::METRIC_NAMES.into_iter().zip(row.metrics()) {
            let suffix = if metric.ends_with("_ns") {
                ""
            } else {
                "_count"
            };
            emit(&format!("vm_step_{name}_{metric}{suffix}"), value)?;
        }
    }
    Ok(())
}

pub(super) fn columns(out: &mut impl Write) -> io::Result<()> {
    #[cfg(feature = "vm-execution-counts")]
    super::counts::columns(out)?;
    #[cfg(feature = "vm-profiling")]
    {
        write!(out, "\tvm_profile_valid")?;
        visit_profile(&Observation::default(), |name, _| write!(out, "\t{name}"))?;
    }
    Ok(())
}

pub(super) fn values(out: &mut impl Write, sample: Option<&Observation>) -> io::Result<()> {
    #[cfg(feature = "vm-execution-counts")]
    super::counts::values(out, sample.map(|sample| (&sample.counts, &sample.gc)))?;
    #[cfg(feature = "vm-profiling")]
    {
        write!(out, "\t{}", u8::from(sample.is_some()))?;
        visit_profile(sample.unwrap_or(&Observation::default()), |_, n| {
            write!(out, "\t{n}")
        })?;
    }
    Ok(())
}

#[cfg(all(test, feature = "vm-profiling"))]
mod tests {
    use super::*;

    #[test]
    fn profile_delta_rejects_restored_identity_and_decreasing_step_samples() {
        let mut before = Observation::default();
        before.driver.heap_id = 1;
        before.driver.epoch = 2;
        before.steps[0].calls = 5;
        let mut after = before;
        after.driver.wall_ns = 10;
        after.steps[0].calls = 7;
        assert_eq!(after.checked_delta(&before).unwrap().steps[0].calls, 2);
        after.driver.epoch = 3;
        assert!(after.checked_delta(&before).is_none());
        after.driver.epoch = 2;
        after.steps[0].eligible_calls = 6;
        after.steps[0].clock_queries = 2;
        after.driver.first_dispatch_gap_ns = 3;
        after.driver.first_dispatch_gaps = 1;
        after.driver.loop_iterations = 7;
        after.driver.lifecycle.parts[0].calls = 2;
        after.driver.lifecycle.parts[0].wall_ns = 9;
        after.driver.lifecycle.unattributed_ns = 4;
        after.driver.lifecycle.unattributed_by_outer_ns = [1, 2, 0, 1];
        after.driver.lifecycle.observer_ns = 3;
        let delta = after.checked_delta(&before).unwrap();
        assert_eq!(delta.steps[0].eligible_calls, 6);
        assert_eq!(delta.steps[0].clock_queries, 2);
        let mut output = Vec::new();
        values(&mut output, Some(&delta)).unwrap();
        let mut names = Vec::new();
        columns(&mut names).unwrap();
        let names = String::from_utf8(names).unwrap();
        let output = String::from_utf8(output).unwrap();
        let columns: Vec<_> = names.split('\t').collect();
        let fields: Vec<_> = output.split('\t').collect();
        assert_eq!(columns.len(), fields.len());
        let index = columns
            .iter()
            .position(|name| *name == "vm_step_scalar_clock_queries_count")
            .unwrap();
        assert_eq!(fields[index], "2");
        for (name, expected) in [
            ("vm_driver_first_dispatch_gap_ns", "3"),
            ("vm_driver_first_dispatch_gaps_count", "1"),
            ("vm_driver_loop_iterations_count", "7"),
            ("vm_driver_part_class_spec_field_calls_count", "2"),
            ("vm_driver_part_class_spec_field_ns", "9"),
            ("vm_driver_parts_unattributed_ns", "4"),
            ("vm_driver_parts_unattributed_scalar_ns", "1"),
            ("vm_driver_parts_unattributed_script_ns", "2"),
            ("vm_driver_parts_unattributed_native_ns", "0"),
            ("vm_driver_parts_unattributed_loop_ns", "1"),
            ("vm_driver_parts_observer_ns", "3"),
        ] {
            let index = columns.iter().position(|column| *column == name).unwrap();
            assert_eq!(fields[index], expected);
        }
        after.steps[0].calls = 4;
        assert!(after.checked_delta(&before).is_none());
    }
}
