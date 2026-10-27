use crate::{
    budget::QueueLimits,
    identity::Priority,
    io::ReadDeadline,
    result::{RejectCode, RejectReason, Rejected},
};
use std::{collections::VecDeque, time::Instant};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QueueUsage {
    /// Includes undrained terminal records, not just pending operations.
    pub requests: u32,
    pub bytes: u64,
    pub peak_bytes: u64,
    pub pending: usize,
    pub completed: usize,
    /// Deadline-bearing requests first dispatched after their soft service time.
    pub deadline_misses: u64,
}

/// Linear permission to enqueue exactly one terminal record.
pub struct CompletionSlot {
    pub id: u64,
    pub bytes: u64,
    ordinary: bool,
}
pub struct Entry<R> {
    pub slot: CompletionSlot,
    pub request: R,
    priority: usize,
    deadline: Option<ReadDeadline>,
    started: bool,
}
impl<R> Entry<R> {
    /// Absolute soft deadline retained across bounded continuations.
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline.as_ref().and_then(ReadDeadline::get)
    }
}
struct Finished<E> {
    event: E,
    bytes: u64,
    ordinary: bool,
}

/// Settlement follows removal of the owning Entry; no growing tombstone set.
pub struct Queue<R, E> {
    limits: QueueLimits,
    pending: [VecDeque<Entry<R>>; 4],
    // Count handles rather than published times: paused publishers can resume.
    deadline_handles: [u32; 4],
    events: VecDeque<Finished<E>>,
    usage: QueueUsage,
    realtime_slots: u32,
    realtime_burst: u8,
    ordinary_requests: u32,
}

impl<R, E> Queue<R, E> {
    pub fn new(limits: QueueLimits) -> Self {
        Self {
            limits,
            pending: std::array::from_fn(|_| VecDeque::new()),
            deadline_handles: [0; 4],
            events: VecDeque::new(),
            usage: QueueUsage::default(),
            realtime_slots: 0,
            realtime_burst: 0,
            ordinary_requests: 0,
        }
    }
    /// Reserve part of the existing request ceiling for streaming IO, including
    /// active requests and undrained completions. Ordinary queues stay unchanged.
    /// With both classes ready, four realtime quanta alternate with one ordinary
    /// quantum. A one-slot queue reserves no slot, preserving ordinary progress.
    pub fn with_realtime_reserve(limits: QueueLimits, slots: u32) -> Self {
        let mut queue = Self::new(limits);
        queue.realtime_slots = slots.min(limits.max_requests.saturating_sub(1));
        queue
    }
    pub fn accept(
        &mut self,
        id: u64,
        request: R,
        bytes: u64,
        priority: Priority,
    ) -> Result<(), Rejected<R>> {
        self.accept_before(id, request, bytes, priority, None)
    }
    /// Admit the same owning request with a soft monotonic deadline. Equal
    /// deadlines retain FIFO order; priority and realtime fairness are preserved.
    pub fn accept_before(
        &mut self,
        id: u64,
        request: R,
        bytes: u64,
        priority: Priority,
        deadline: Option<ReadDeadline>,
    ) -> Result<(), Rejected<R>> {
        let priority = match priority {
            Priority::Prefetch => 0,
            Priority::Normal => 1,
            Priority::Visible => 2,
            Priority::Realtime => 3,
        };
        let next = self
            .usage
            .bytes
            .checked_add(bytes)
            .filter(|n| *n <= self.limits.max_bytes);
        let reserved = priority != 3
            && self.ordinary_requests >= self.limits.max_requests - self.realtime_slots;
        let failure =
            if self.usage.requests >= self.limits.max_requests || reserved || next.is_none() {
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
        self.deadline_handles[priority] += u32::from(deadline.is_some());
        self.pending[priority].push_back(Entry {
            slot: CompletionSlot {
                id,
                bytes,
                ordinary: priority != 3,
            },
            request,
            priority,
            deadline,
            started: false,
        });
        self.usage.requests += 1;
        self.ordinary_requests += u32::from(priority != 3);
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
    /// Earliest admitted service time, including partially progressed requests.
    pub fn earliest_deadline(&self) -> Option<Instant> {
        self.pending
            .iter()
            .zip(self.deadline_handles)
            .filter(|(_, count)| *count != 0)
            .flat_map(|(queue, _)| queue.iter())
            .filter_map(Entry::deadline)
            .min()
    }
    pub fn pop(&mut self) -> Option<Entry<R>> {
        self.pop_with_clock(Instant::now)
    }
    /// Dispatch using an explicit monotonic observation, also useful to measure
    /// scheduling traces without wall-clock sleeps.
    pub fn pop_at(&mut self, now: Instant) -> Option<Entry<R>> {
        self.pop_with_clock(|| now)
    }
    fn pop_with_clock(&mut self, clock: impl FnOnce() -> Instant) -> Option<Entry<R>> {
        let ordinary = || self.pending[..3].iter().rposition(|q| !q.is_empty());
        let priority = if self.realtime_slots != 0 && !self.pending[3].is_empty() {
            if self.realtime_burst >= 4
                && let Some(index) = ordinary()
            {
                self.realtime_burst = 0;
                index
            } else {
                self.realtime_burst = self.realtime_burst.saturating_add(1).min(4);
                3
            }
        } else {
            self.realtime_burst = 0;
            self.pending.iter().rposition(|q| !q.is_empty())?
        };
        if self.deadline_handles[priority] == 0 {
            return self.pending[priority].pop_front();
        }
        let index = self.pending[priority]
            .iter()
            .enumerate()
            .min_by_key(|(_, entry)| {
                let deadline = entry.deadline();
                (deadline.is_none(), deadline)
            })
            .map(|(index, _)| index)?;
        let mut entry = self.pending[priority].remove(index).unwrap();
        self.deadline_handles[priority] -= u32::from(entry.deadline.is_some());
        if !entry.started && entry.deadline().is_some_and(|deadline| clock() > deadline) {
            self.usage.deadline_misses += 1;
        }
        entry.started = true;
        Some(entry)
    }
    pub fn take(&mut self, id: u64) -> Option<Entry<R>> {
        for (priority, queue) in self.pending.iter_mut().enumerate() {
            if let Some(index) = queue.iter().position(|e| e.slot.id == id) {
                let entry = queue.remove(index).unwrap();
                self.deadline_handles[priority] -= u32::from(entry.deadline.is_some());
                return Some(entry);
            }
        }
        None
    }
    pub fn requeue(&mut self, entry: Entry<R>) {
        self.deadline_handles[entry.priority] += u32::from(entry.deadline.is_some());
        self.pending[entry.priority].push_back(entry);
    }
    pub fn finish(&mut self, event: E, slot: CompletionSlot) {
        // The slot was reserved at accept, before any ID or effect was published.
        self.events.push_back(Finished {
            event,
            bytes: slot.bytes,
            ordinary: slot.ordinary,
        });
    }
    pub fn drain(&mut self, out: &mut Vec<E>, limit: usize) {
        for _ in 0..limit {
            let Some(f) = self.events.pop_front() else {
                break;
            };
            self.usage.requests -= 1;
            self.ordinary_requests -= u32::from(f.ordinary);
            self.usage.bytes -= f.bytes;
            out.push(f.event);
        }
    }
    /// Removes one terminal owned by the selected consumer, releasing its global
    /// request/byte reservation. Unselected terminals keep their reservations.
    pub fn take_finished(&mut self, owns: impl Fn(&E) -> bool) -> Option<E> {
        let index = self
            .events
            .iter()
            .position(|finished| owns(&finished.event))?;
        let finished = self.events.remove(index).expect("selected terminal exists");
        self.usage.requests -= 1;
        self.ordinary_requests -= u32::from(finished.ordinary);
        self.usage.bytes -= finished.bytes;
        Some(finished.event)
    }
    /// Settles a terminal whose owning consumer has closed, without queuing it.
    pub fn discard(&mut self, slot: CompletionSlot) {
        self.usage.requests -= 1;
        self.ordinary_requests -= u32::from(slot.ordinary);
        self.usage.bytes -= slot.bytes;
    }
    pub fn usage(&self) -> QueueUsage {
        QueueUsage {
            pending: self.pending.iter().map(VecDeque::len).sum(),
            completed: self.events.len(),
            ..self.usage
        }
    }
}

pub fn charge(parts: &[u64]) -> Result<u64, RejectReason> {
    parts
        .iter()
        .try_fold(0u64, |total, n| total.checked_add(*n))
        .ok_or_else(|| {
            RejectReason::new(RejectCode::InvalidRequest, "reservation byte sum overflow")
        })
}

pub fn invalid(message: &str) -> RejectReason {
    RejectReason::new(RejectCode::InvalidRequest, message)
}

#[cfg(test)]
mod deadline_handle_tests {
    use super::*;
    #[test]
    fn fifo_skips_clock_and_counts_cover_active_cancelled_and_paused_owners() {
        let mut queue = Queue::<u8, u8>::new(QueueLimits {
            max_requests: 3,
            max_bytes: 3,
        });
        queue.accept(1, 1, 1, Priority::Normal).unwrap();
        let hint = ReadDeadline::default();
        queue
            .accept_before(2, 2, 1, Priority::Normal, Some(hint.clone()))
            .unwrap();
        queue
            .accept_before(3, 3, 1, Priority::Realtime, Some(hint.clone()))
            .unwrap();
        assert_eq!(queue.deadline_handles, [0, 1, 0, 1]);
        // Cancellation removes a pending handle, and its terminal slot stays owned.
        let cancelled = queue.take(3).unwrap();
        assert_eq!(queue.deadline_handles, [0, 1, 0, 0]);
        queue.finish(3, cancelled.slot);
        let active = queue.take(2).unwrap();
        assert_eq!(queue.deadline_handles, [0; 4]);
        let ordinary = queue
            .pop_with_clock(|| panic!("FIFO must not read time"))
            .unwrap();
        assert_eq!(ordinary.request, 1);
        queue.requeue(ordinary);
        queue.requeue(active);
        assert_eq!(queue.deadline_handles, [0, 1, 0, 0]);
        hint.pause(true);
        let ordinary = queue
            .pop_with_clock(|| panic!("paused hint has no miss time"))
            .unwrap();
        assert_eq!(ordinary.request, 1);
        queue.requeue(ordinary);
        hint.pause(false);
        let active = queue.pop().unwrap();
        assert_eq!(
            active.request, 2,
            "paused handle dynamically becomes urgent"
        );
        assert_eq!(queue.deadline_handles, [0; 4]);
        queue.discard(active.slot);
        assert!(queue.earliest_deadline().is_none());
        assert_eq!(
            queue
                .pop_with_clock(|| panic!("last-handle removal restores FIFO"))
                .unwrap()
                .request,
            1
        );
    }
}
