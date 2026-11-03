//! Retain an eligible scratch output only after its last frame-local lease.
use super::*;
use crate::common::{effect_result::Token, target_writes::Stamp};

pub(super) struct Tail {
    pending: usize,
    stamp: Stamp,
    token: Token,
    target: Target,
}

impl Context<'_> {
    /// These ordinary effect outputs are read-only after production. Their
    /// exclusive-source proof requires any later write to acquire a new lease,
    /// which invalidates the watched version through TargetWrites::begin.
    pub(in crate::gxm) fn watch_effect_tail(
        &mut self,
        target: &Target,
        pending: usize,
        token: Token,
    ) {
        let mut writes = std::mem::take(&mut self.target_writes);
        let stamp = writes.watch(target.slot, |bytes| self.transient(bytes));
        self.target_writes = writes;
        let Ok(stamp) = stamp else {
            return;
        };
        let mut tails = std::mem::take(&mut self.effect_tails);
        let _ = push_stack(
            &mut tails,
            Tail {
                pending,
                stamp,
                token,
                target: target.clone(),
            },
            |bytes| self.transient(bytes),
        );
        self.effect_tails = tails;
    }

    /// All streams have completed and flushed their commands. A surviving
    /// version is already included in this frame's physical scratch charge;
    /// retaining it now needs neither a replacement nor another pixel ticket.
    pub(super) fn seal_effect_tails(&mut self) {
        for tail in std::mem::take(&mut self.effect_tails) {
            if !self.target_writes.is_current(tail.stamp) {
                continue;
            }
            let Some(pinned) = tail.target.pin_effect(tail.token, &self.budgets.transient) else {
                continue;
            };
            let mut pins = std::mem::take(&mut self.effect_pins);
            let saved = push_stack(&mut pins, pinned.clone(), |bytes| self.transient(bytes));
            self.effect_pins = pins;
            if saved.is_ok() {
                self.effect_pending[tail.pending].materialize(pinned, true);
            }
        }
    }
}
