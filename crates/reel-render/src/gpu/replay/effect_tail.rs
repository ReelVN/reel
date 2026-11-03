//! Transfer a surviving final scratch write into its existing pending result.
use super::*;
use crate::common::{effect_result::Token, target_writes::Stamp};

pub(in crate::gpu) struct Tail {
    pending: usize,
    output: Target,
    token: Token,
    stamp: Stamp,
}
impl Context<'_> {
    pub(in crate::gpu) fn watch_effect_tail(
        &mut self,
        pending: usize,
        output: &Target,
        token: Token,
    ) {
        let mut writes = std::mem::take(&mut self.target_writes);
        let stamp = writes.watch(output.slot, |bytes| self.transient(bytes));
        self.target_writes = writes;
        let Ok(stamp) = stamp else {
            return;
        };
        let mut tails = std::mem::take(&mut self.effect_tails);
        let _ = push_stack(
            &mut tails,
            Tail {
                pending,
                output: output.clone(),
                token,
                stamp,
            },
            |bytes| self.transient(bytes),
        );
        self.effect_tails = tails;
    }

    /// All preparation streams and their terminal composites have been encoded.
    /// Pins keep the already charged allocations immutable through submission.
    pub(in crate::gpu) fn seal_effect_tails(&mut self) {
        let tails = std::mem::take(&mut self.effect_tails);
        if !self.effect_admission.enabled() {
            return;
        }
        for tail in tails {
            if !self.target_writes.is_current(tail.stamp) {
                continue;
            }
            let Some(pinned) = tail.output.pin_effect(tail.token, &self.budgets.transient) else {
                continue;
            };
            let mut pins = std::mem::take(&mut self.effect_pins);
            let held = push_stack(&mut pins, pinned.clone(), |bytes| self.transient(bytes));
            self.effect_pins = pins;
            if held.is_err() {
                continue;
            }
            let allocation = self.target_allocations[tail.output.slot].clone();
            self.effect_pending[tail.pending].materialize(
                super::super::retained::Output {
                    allocation,
                    target: pinned,
                },
                true,
            );
        }
    }
}
