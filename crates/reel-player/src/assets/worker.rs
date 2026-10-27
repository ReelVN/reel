use reel_core::{
    budget::{BudgetCategory, BudgetTicket, QueueLimits},
    identity::Priority,
    queue::{Queue, invalid},
    result::{RejectCode, RejectReason, Rejected},
    wake::WakeSignal,
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

struct Task<R> {
    request: R,
    cancelled: Arc<AtomicBool>,
}
struct State<R, E> {
    queue: Queue<Task<R>, E>,
    active: BTreeMap<u64, Arc<AtomicBool>>,
    closed: bool,
}
type Shared<R, E> = Arc<(Mutex<State<R, E>>, Condvar)>;
type Handler<R, E> = dyn Fn(R, &AtomicBool) -> Step<R, E> + Send + Sync;

/// A continuation retains its queue slot, cancellation flag and request ID.
/// Handlers must settle cancelled work at their next bounded step.
pub(crate) enum Step<R, E> {
    Continue(R),
    Complete(E),
}

pub(super) fn check_completion(ticket: &BudgetTicket) -> Result<(), RejectReason> {
    if ticket.category() != BudgetCategory::Completion || ticket.bytes() < 512 {
        return Err(invalid("completion ticket missing or too small"));
    }
    Ok(())
}

/// One bounded background lane, shared by all resource kinds using the adapter.
pub(crate) struct Worker<R, E> {
    state: Shared<R, E>,
    handler: Arc<Handler<R, E>>,
    cancel_event: fn(&mut E),
    wake: WakeSignal,
    started: bool,
}
impl<R: Send + 'static, E: Send + 'static> Worker<R, E> {
    pub fn new(
        limits: QueueLimits,
        wake: WakeSignal,
        handler: impl Fn(R, &AtomicBool) -> E + Send + Sync + 'static,
        cancel_event: fn(&mut E),
    ) -> Self {
        Self::stepped(
            limits,
            wake,
            0,
            move |request, cancelled| Step::Complete(handler(request, cancelled)),
            cancel_event,
        )
    }
    pub fn stepped(
        limits: QueueLimits,
        wake: WakeSignal,
        realtime_slots: u32,
        handler: impl Fn(R, &AtomicBool) -> Step<R, E> + Send + Sync + 'static,
        cancel_event: fn(&mut E),
    ) -> Self {
        Self {
            state: Arc::new((
                Mutex::new(State {
                    queue: Queue::with_realtime_reserve(limits, realtime_slots),
                    active: BTreeMap::new(),
                    closed: false,
                }),
                Condvar::new(),
            )),
            handler: Arc::new(handler),
            cancel_event,
            wake,
            started: false,
        }
    }
    pub fn accept(
        &mut self,
        id: u64,
        request: R,
        bytes: u64,
        priority: Priority,
    ) -> Result<(), Rejected<R>> {
        if !self.started {
            let shared = self.state.clone();
            let handler = self.handler.clone();
            let cancel_event = self.cancel_event;
            let wake = self.wake.clone();
            if let Err(error) = std::thread::Builder::new()
                .name("reel-resource".into())
                .spawn(move || {
                    'worker: loop {
                        let mut entry = {
                            let mut state = shared.0.lock().unwrap();
                            loop {
                                if state.closed && state.queue.usage().pending == 0 {
                                    break 'worker;
                                }
                                if let Some(entry) = state.queue.pop() {
                                    break entry;
                                }
                                state = shared.1.wait(state).unwrap();
                            }
                        };
                        let mut event =
                            match handler(entry.request.request, &entry.request.cancelled) {
                                Step::Complete(event) => event,
                                Step::Continue(request) => {
                                    entry.request.request = request;
                                    let mut state = shared.0.lock().unwrap();
                                    state.queue.requeue(entry);
                                    continue;
                                }
                            };
                        // Removing the live ID settles cancellation before queuing the terminal.
                        let cancelled = shared
                            .0
                            .lock()
                            .unwrap()
                            .active
                            .remove(&entry.slot.id)
                            .unwrap()
                            .load(Ordering::Acquire);
                        if cancelled {
                            cancel_event(&mut event);
                        }
                        {
                            let mut state = shared.0.lock().unwrap();
                            if state.closed {
                                drop(state);
                                drop(event);
                                // The consumer has closed, but every accepted
                                // request still settles cancellation once. Drop
                                // its terminal here and drain cancelled owners.
                                continue;
                            }
                            state.queue.finish(event, entry.slot);
                        }
                        wake.wake();
                    }
                    drop(shared);
                    wake.wake();
                })
            {
                return Err(Rejected {
                    request,
                    reason: RejectReason::new(RejectCode::Unavailable, error.to_string()),
                });
            }
            self.started = true;
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut state = self.state.0.lock().unwrap();
        state
            .queue
            .accept(
                id,
                Task {
                    request,
                    cancelled: cancelled.clone(),
                },
                bytes,
                priority,
            )
            .map_err(|error| Rejected {
                request: error.request.request,
                reason: error.reason,
            })?;
        state.active.insert(id, cancelled);
        drop(state);
        self.state.1.notify_one();
        Ok(())
    }
    pub fn is_idle(&self) -> bool {
        self.state.0.lock().unwrap().queue.usage().requests == 0
    }
    pub fn cancel(&mut self, id: u64) {
        if let Some(cancelled) = self.state.0.lock().unwrap().active.get(&id) {
            cancelled.store(true, Ordering::Release);
        }
    }
    pub fn drain(&mut self, events: &mut Vec<E>, limit: usize) {
        self.state.0.lock().unwrap().queue.drain(events, limit);
    }
}
impl<R, E> Drop for Worker<R, E> {
    fn drop(&mut self) {
        let mut state = self.state.0.lock().unwrap();
        state.closed = true;
        for flag in state.active.values() {
            flag.store(true, Ordering::Release);
        }
        drop(state);
        self.state.1.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{atomic::AtomicUsize, mpsc},
        time::{Duration, Instant},
    };

    struct Job {
        id: u64,
        left: usize,
        _ticket: BudgetTicket,
    }
    struct Event {
        id: u64,
        cancelled: bool,
        _ticket: BudgetTicket,
    }
    fn cancel(event: &mut Event) {
        event.cancelled = true;
    }

    #[test]
    fn continuations_preserve_slots_tickets_cancellation_and_shutdown_settlement() {
        for shutdown in [false, true] {
            let pool = reel_core::budget::BudgetPool::new(
                BudgetCategory::CpuInput,
                QueueLimits {
                    max_requests: 8,
                    max_bytes: 1024,
                },
            );
            let (started_tx, started_rx) = mpsc::channel();
            let (resume_tx, resume_rx) = mpsc::channel();
            let gate = Mutex::new(resume_rx);
            let terminals = Arc::new(AtomicUsize::new(0));
            let count = terminals.clone();
            let mut worker = Worker::stepped(
                QueueLimits {
                    max_requests: 4,
                    max_bytes: 1024,
                },
                WakeSignal::new(|| {}),
                2,
                move |mut job: Job, cancelled| {
                    if job.id == 1 && job.left == 2 {
                        job.left -= 1;
                        started_tx.send(()).unwrap();
                        gate.lock()
                            .unwrap()
                            .recv_timeout(Duration::from_secs(3))
                            .unwrap();
                        return Step::Continue(job);
                    }
                    if cancelled.load(Ordering::Acquire) || job.left == 0 {
                        count.fetch_add(1, Ordering::Relaxed);
                        Step::Complete(Event {
                            id: job.id,
                            cancelled: false,
                            _ticket: job._ticket,
                        })
                    } else {
                        job.left -= 1;
                        Step::Continue(job)
                    }
                },
                cancel,
            );
            worker
                .accept(
                    1,
                    Job {
                        id: 1,
                        left: 2,
                        _ticket: pool.reserve(128).unwrap(),
                    },
                    128,
                    Priority::Visible,
                )
                .unwrap_or_else(|_| panic!("initial admission"));
            started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            worker
                .accept(
                    2,
                    Job {
                        id: 2,
                        left: 3,
                        _ticket: pool.reserve(128).unwrap(),
                    },
                    128,
                    Priority::Visible,
                )
                .unwrap_or_else(|_| panic!("second admission"));
            if shutdown {
                drop(worker);
                resume_tx.send(()).unwrap();
                let deadline = Instant::now() + Duration::from_secs(3);
                while pool.usage().bytes != 0 && Instant::now() < deadline {
                    std::thread::yield_now();
                }
                assert_eq!(pool.usage().bytes, 0);
                assert_eq!(terminals.load(Ordering::Relaxed), 2);
            } else {
                worker.cancel(2);
                worker
                    .accept(
                        3,
                        Job {
                            id: 3,
                            left: 0,
                            _ticket: pool.reserve(128).unwrap(),
                        },
                        128,
                        Priority::Realtime,
                    )
                    .unwrap_or_else(|_| panic!("reserved media admission"));
                resume_tx.send(()).unwrap();
                let mut events = Vec::new();
                let deadline = Instant::now() + Duration::from_secs(3);
                while events.len() != 3 && Instant::now() < deadline {
                    worker.drain(&mut events, 3);
                    std::thread::yield_now();
                }
                assert_eq!(events.iter().map(|e| e.id).collect::<Vec<_>>(), [3, 2, 1]);
                assert!(events[1].cancelled);
                assert_eq!(terminals.load(Ordering::Relaxed), 3);
                assert_eq!(pool.usage().bytes, 384);
                drop(events);
                assert_eq!(pool.usage().bytes, 0);
                assert!(worker.is_idle());
            }
        }
    }
}
