use super::{RunOutcome, SliceBudget, TaskId, VmError};
use aside_vm::executor::Vm;

/// Explicit policy for a session's next VM slice.
#[derive(Clone, Copy, Debug)]
pub enum SessionBudget {
    /// Preserve every supplied limit, including work and allocation.
    Exact(SliceBudget),
    /// Grant this task's last reported minimum work/allocation requirement.
    /// Instruction limits, deadlines and hard heap quotas remain unchanged.
    RetryMinimum(SliceBudget),
}

/// Explicit work policy for one native continuation.
#[derive(Clone, Copy, Debug)]
pub enum NativeBudget {
    /// Keep the supplied work limit and report the next required minimum.
    Exact(u32),
    /// Admit the current continuation's next indivisible step when necessary.
    RetryMinimum(u32),
}

impl NativeBudget {
    pub(super) fn work(self) -> u32 {
        match self {
            Self::Exact(work) | Self::RetryMinimum(work) => work,
        }
    }

    pub(super) fn with_minimum(self, required: u32) -> u32 {
        match self {
            Self::Exact(work) => work,
            Self::RetryMinimum(work) => work.max(required),
        }
    }
}

/// Scheduling advice only; the VM continuation remains the source of truth.
/// Each task owns its hint so another task's slice cannot overwrite it.
#[derive(Default)]
pub(super) struct SliceMinimum {
    work: u64,
    allocation: u64,
}

impl SliceMinimum {
    pub(super) fn run(
        &mut self,
        vm: &mut Vm,
        task: TaskId,
        policy: SessionBudget,
        now: impl FnMut() -> u64,
    ) -> Result<RunOutcome, VmError> {
        let budget = match policy {
            SessionBudget::Exact(budget) => budget,
            SessionBudget::RetryMinimum(mut budget) => {
                budget.work = budget.work.max(self.work);
                budget.allocation_bytes = budget.allocation_bytes.max(self.allocation);
                budget
            }
        };
        let outcome = vm.run_slice(task, budget, now);
        match &outcome {
            Ok(RunOutcome::BudgetYield(usage))
                if usage.required_work != 0 || usage.required_allocation != 0 =>
            {
                self.work = usage.required_work;
                self.allocation = usage.required_allocation;
            }
            // An expired deadline (or zero instruction grant) did not consume
            // the pending operation. Retain its minimum for the next attempt.
            Ok(RunOutcome::BudgetYield(usage))
                if usage.instructions == 0 && usage.work == 0 && usage.allocation_bytes == 0 => {}
            _ => *self = Self::default(),
        }
        outcome
    }
}
