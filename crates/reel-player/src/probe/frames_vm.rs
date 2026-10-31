//! One VM snapshot at the presentation boundary aligns counts, sampled stages
//! and whole-slice observations. Identity changes invalidate all its differences.
#[cfg(feature = "vm-execution-counts")]
use aside_vm::executor::ExecutionCounts;
use aside_vm::executor::Vm;
#[cfg(feature = "vm-profiling")]
use aside_vm::executor::{DriverCost, STEP_NAMES, StepCost};
use std::io::{self, Write};

#[derive(Clone, Copy, Default)]
pub(super) struct Observation {
    #[cfg(feature = "vm-execution-counts")]
    pub(super) counts: ExecutionCounts,
    #[cfg(feature = "vm-profiling")]
    pub(super) driver: DriverCost,
    #[cfg(feature = "vm-profiling")]
    pub(super) steps: [StepCost; STEP_NAMES.len()],
}

impl Observation {
    pub(super) fn capture(vm: &Vm) -> Self {
        Self {
            #[cfg(feature = "vm-execution-counts")]
            counts: vm.execution_counts(),
            #[cfg(feature = "vm-profiling")]
            driver: vm.driver_costs(),
            #[cfg(feature = "vm-profiling")]
            steps: vm.step_costs(),
        }
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
        sampled_work
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
        budget_clock_ns,
        calibration_ns
    );
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
    super::counts::values(out, sample.map(|sample| &sample.counts))?;
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
        after.steps[0].calls = 4;
        assert!(after.checked_delta(&before).is_none());
    }
}
