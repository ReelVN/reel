//! Per-slice admission and bounded soft-clock checks.
use super::{SliceBudget, SliceUsage};

pub(super) struct Meter {
    pub(super) budget: SliceBudget,
    pub(super) usage: SliceUsage,
    pub(super) clock_steps: u8,
    #[cfg(any(feature = "profiling", feature = "execution-counts"))]
    pub(super) actual_budget_clock_queries: u64,
    #[cfg(feature = "profiling")]
    pub(super) observe_clock: bool,
    #[cfg(feature = "profiling")]
    pub(super) budget_clock_ns: u64,
}

impl Meter {
    pub(super) fn new(budget: SliceBudget) -> Self {
        Self {
            budget,
            usage: SliceUsage::default(),
            clock_steps: 0,
            #[cfg(any(feature = "profiling", feature = "execution-counts"))]
            actual_budget_clock_queries: 0,
            #[cfg(feature = "profiling")]
            observe_clock: false,
            #[cfg(feature = "profiling")]
            budget_clock_ns: 0,
        }
    }

    /// Consume another short internal step before the next clock boundary.
    /// Fused operations retain their individual work admission and root writes.
    pub(super) fn next_short_step(&mut self) -> bool {
        if self.budget.deadline_ns.is_none() {
            return true;
        }
        if self.clock_steps == 0 {
            return false;
        }
        self.clock_steps -= 1;
        true
    }

    pub(super) fn deadline_reached(&mut self, now: &mut impl FnMut() -> u64) -> bool {
        let Some(deadline) = self.budget.deadline_ns else {
            return false;
        };
        if self.clock_steps != 0 {
            self.clock_steps -= 1;
            return false;
        }
        self.clock_steps = 31;
        #[cfg(any(feature = "profiling", feature = "execution-counts"))]
        {
            self.actual_budget_clock_queries += 1;
        }
        #[cfg(feature = "profiling")]
        let start = self.observe_clock.then(std::time::Instant::now);
        let value = now();
        #[cfg(feature = "profiling")]
        if let Some(start) = start {
            self.budget_clock_ns += start.elapsed().as_nanos() as u64;
        }
        value >= deadline
    }

    pub(super) fn admits(&mut self, instructions: u32, work: u64, allocation: u64) -> bool {
        self.usage.required_work = work;
        self.usage.required_allocation = allocation;
        instructions <= self.budget.instructions - self.usage.instructions
            && work <= self.budget.work - self.usage.work
            && allocation <= self.budget.allocation_bytes - self.usage.allocation_bytes
    }

    pub(super) fn take(&mut self, instructions: u32, work: u64, allocation: u64) -> bool {
        if !self.admits(instructions, work, allocation) {
            return false;
        }
        self.usage.instructions += instructions;
        self.usage.work += work;
        self.usage.allocation_bytes += allocation;
        // Allocations and bulk work end the current clock batch immediately.
        if allocation != 0 || work >= 32 {
            self.clock_steps = 0;
        }
        self.usage.required_work = 0;
        self.usage.required_allocation = 0;
        true
    }
}
