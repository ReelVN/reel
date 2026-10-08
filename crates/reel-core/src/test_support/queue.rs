use crate::{
    budget::QueueLimits,
    identity::Priority,
    result::{RejectCode, RejectReason, Rejected},
};
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QueueUsage {
    /// Includes undrained terminal records, not just pending operations.
    pub requests: u32,
    pub bytes: u64,
    pub peak_bytes: u64,
    pub pending: usize,
    pub completed: usize,
}

/// Linear permission to enqueue exactly one terminal record.
pub(super) struct CompletionSlot {
    pub id: u64,
    pub bytes: u64,
}
pub(super) struct Entry<R> {
    pub slot: CompletionSlot,
    pub request: R,
    priority: usize,
}
struct Finished<E> {
    event: E,
    bytes: u64,
}

/// Settlement follows removal of the owning Entry; no growing tombstone set.
pub(super) struct Queue<R, E> {
    limits: QueueLimits,
    pending: [VecDeque<Entry<R>>; 3],
    events: VecDeque<Finished<E>>,
    usage: QueueUsage,
}

impl<R, E> Queue<R, E> {
    pub fn new(limits: QueueLimits) -> Self {
        Self {
            limits,
            pending: std::array::from_fn(|_| VecDeque::new()),
            events: VecDeque::new(),
            usage: QueueUsage::default(),
        }
    }
    pub fn accept(
        &mut self,
        id: u64,
        request: R,
        bytes: u64,
        priority: Priority,
    ) -> Result<(), Rejected<R>> {
        let priority = match priority {
            Priority::Prefetch => 0,
            Priority::Normal => 1,
            Priority::Visible => 2,
        };
        let next = self
            .usage
            .bytes
            .checked_add(bytes)
            .filter(|n| *n <= self.limits.max_bytes);
        let failure = if self.usage.requests >= self.limits.max_requests || next.is_none() {
            Some(RejectReason::new(
                RejectCode::Busy,
                "request/completion queue full",
            ))
        } else if self.pending[priority].try_reserve(1).is_err()
            || self
                .events
                .try_reserve(self.usage.requests as usize + 1 - self.events.len())
                .is_err()
        {
            Some(RejectReason::new(
                RejectCode::OutOfMemory,
                "cannot reserve request and terminal slots",
            ))
        } else {
            None
        };
        if let Some(reason) = failure {
            return Err(Rejected { request, reason });
        }
        self.pending[priority].push_back(Entry {
            slot: CompletionSlot { id, bytes },
            request,
            priority,
        });
        self.usage.requests += 1;
        self.usage.bytes = next.expect("checked above");
        self.usage.peak_bytes = self.usage.peak_bytes.max(self.usage.bytes);
        Ok(())
    }
    pub fn pending(&self) -> impl Iterator<Item = &Entry<R>> {
        self.pending.iter().flat_map(|q| q.iter())
    }
    pub fn check_capacity(&self, bytes: u64) -> Result<(), RejectReason> {
        if self.usage.requests >= self.limits.max_requests
            || self
                .usage
                .bytes
                .checked_add(bytes)
                .is_none_or(|n| n > self.limits.max_bytes)
        {
            return Err(RejectReason::new(
                RejectCode::Busy,
                "request/completion queue full",
            ));
        }
        Ok(())
    }
    pub fn pop(&mut self) -> Option<Entry<R>> {
        // Exactly three head checks, independent of queued request count.
        self.pending.iter_mut().rev().find_map(|q| q.pop_front())
    }
    pub fn take(&mut self, id: u64) -> Option<Entry<R>> {
        for queue in &mut self.pending {
            if let Some(index) = queue.iter().position(|e| e.slot.id == id) {
                return queue.remove(index);
            }
        }
        None
    }
    pub fn requeue(&mut self, entry: Entry<R>) {
        self.pending[entry.priority].push_back(entry);
    }
    pub fn finish(&mut self, event: E, slot: CompletionSlot) {
        // The slot was reserved at accept, before any ID or effect was published.
        self.events.push_back(Finished {
            event,
            bytes: slot.bytes,
        });
    }
    pub fn drain(&mut self, out: &mut Vec<E>, limit: usize) {
        for _ in 0..limit {
            let Some(f) = self.events.pop_front() else {
                break;
            };
            self.usage.requests -= 1;
            self.usage.bytes -= f.bytes;
            out.push(f.event);
        }
    }
    pub fn usage(&self) -> QueueUsage {
        QueueUsage {
            pending: self.pending.iter().map(VecDeque::len).sum(),
            completed: self.events.len(),
            ..self.usage
        }
    }
}

pub(super) fn charge(parts: &[u64]) -> Result<u64, RejectReason> {
    parts
        .iter()
        .try_fold(0u64, |total, n| total.checked_add(*n))
        .ok_or_else(|| {
            RejectReason::new(RejectCode::InvalidRequest, "reservation byte sum overflow")
        })
}

pub(super) fn invalid(message: &str) -> RejectReason {
    RejectReason::new(RejectCode::InvalidRequest, message)
}
