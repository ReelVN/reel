//! Disjoint dispatch gaps reuse the driver's timestamps; no clock is read here.
use super::DriverCost;
use std::{cell::Cell, time::Instant};

#[derive(Clone, Copy)]
pub(crate) enum Control {
    Iteration,
    Cancelling,
    FaultBuild,
    AbortEquality,
    CooperativeCancel,
    UnbuiltFault,
}

#[derive(Default)]
pub(super) struct State {
    last_end: Cell<Option<Instant>>,
    dispatched: Cell<bool>,
    open: Cell<bool>,
    durations: [Cell<u64>; 4],
    counts: [Cell<u64>; 4],
    controls: [Cell<u64>; 6],
    errors: Cell<u64>,
}

impl State {
    pub(super) fn begin(&self, began: Instant) {
        self.last_end.set(Some(began));
        self.dispatched.set(false);
        self.open.set(false);
        for value in self
            .durations
            .iter()
            .chain(&self.counts)
            .chain(&self.controls)
        {
            value.set(0);
        }
        self.errors.set(0);
    }

    fn error(&self) {
        self.errors.set(self.errors.get() + 1);
    }

    fn gap(&self, kind: usize, ended: Instant) {
        self.counts[kind].set(self.counts[kind].get() + 1);
        let Some(began) = self.last_end.take() else {
            self.error();
            return;
        };
        let Some(duration) = ended.checked_duration_since(began) else {
            self.error();
            return;
        };
        self.durations[kind]
            .set(self.durations[kind].get() + duration.as_nanos().min(u128::from(u64::MAX)) as u64);
    }

    pub(super) fn start_span(&self, phase: usize, began: Instant) {
        if self.open.replace(true) {
            self.error();
        }
        let kind = if !self.dispatched.replace(true) {
            0
        } else if phase == 0 {
            1
        } else {
            2
        };
        self.gap(kind, began);
    }

    pub(super) fn end_span(&self, ended: Instant) {
        if !self.open.replace(false) || self.last_end.get().is_some() {
            self.error();
        }
        self.last_end.set(Some(ended));
    }

    pub(super) fn end(&self, ended: Instant) {
        if self.open.get() {
            self.error();
        }
        self.gap(3, ended);
    }

    #[inline]
    pub(super) fn control(&self, control: Control) {
        let value = &self.controls[control as usize];
        value.set(value.get() + 1);
    }

    pub(super) fn publish(&self, row: &mut DriverCost) {
        [
            row.first_dispatch_gap_ns,
            row.scalar_reentry_gap_ns,
            row.classified_dispatch_gap_ns,
            row.final_dispatch_gap_ns,
        ] = std::array::from_fn(|i| self.durations[i].get());
        [
            row.first_dispatch_gaps,
            row.scalar_reentry_gaps,
            row.classified_dispatch_gaps,
            row.final_dispatch_gaps,
        ] = std::array::from_fn(|i| self.counts[i].get());
        [
            row.loop_iterations,
            row.loop_cancelling,
            row.loop_fault_build,
            row.loop_abort_equality,
            row.loop_cooperative_cancel,
            row.loop_unbuilt_fault,
        ] = std::array::from_fn(|i| self.controls[i].get());
        row.loop_gap_errors = self.errors.get();
        let sum = self.durations.iter().map(Cell::get).sum::<u64>();
        if sum != row.loop_ns {
            row.loop_gap_errors += 1;
        }
    }
}

#[cfg(test)]
mod tests;
