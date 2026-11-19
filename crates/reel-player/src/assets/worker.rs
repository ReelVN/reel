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
    time::{Duration, Instant},
};

struct Task<R> {
    request: R,
    control: Arc<Control>,
}
#[derive(Default)]
struct Control {
    cancelled: AtomicBool,
    promoted: AtomicBool,
}
struct Event<E> {
    client: u64,
    event: E,
}
struct Active {
    control: Arc<Control>,
    client: u64,
}
#[derive(Clone)]
struct Client {
    wake: WakeSignal,
    waiting: Arc<AtomicBool>,
}
impl Client {
    fn new(wake: WakeSignal) -> Self {
        Self {
            wake,
            waiting: Arc::new(AtomicBool::new(false)),
        }
    }
}
struct State<R, E> {
    queue: Queue<Task<R>, Event<E>>,
    active: BTreeMap<u64, Active>,
    clients: BTreeMap<u64, Client>,
    wakes: Arc<[Client]>,
    next_client: u64,
    max_clients: u32,
    handler: Option<Arc<Handler<R, E>>>,
    workers: usize,
    max_workers: usize,
    closed: bool,
}
type Shared<R, E> = Arc<(Mutex<State<R, E>>, Condvar)>;
type Handler<R, E> = dyn Fn(R, &AtomicBool, bool) -> Step<R, E> + Send + Sync;

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

/// A bounded background queue, shared by all clients of the adapter.
pub(crate) struct Worker<R, E> {
    state: Shared<R, E>,
    cancel_event: fn(&mut E),
    client: u64,
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
            move |request, cancelled, _| Step::Complete(handler(request, cancelled)),
            cancel_event,
        )
    }
    pub fn stepped(
        limits: QueueLimits,
        wake: WakeSignal,
        realtime_slots: u32,
        handler: impl Fn(R, &AtomicBool, bool) -> Step<R, E> + Send + Sync + 'static,
        cancel_event: fn(&mut E),
    ) -> Self {
        let client = Client::new(wake);
        Self {
            state: Arc::new((
                Mutex::new(State {
                    queue: Queue::with_realtime_reserve(limits, realtime_slots),
                    active: BTreeMap::new(),
                    clients: BTreeMap::from([(0, client.clone())]),
                    wakes: Arc::from([client]),
                    next_client: 1,
                    max_clients: limits.max_requests,
                    handler: Some(Arc::new(handler)),
                    workers: 0,
                    max_workers: 1,
                    closed: false,
                }),
                Condvar::new(),
            )),
            cancel_event,
            client: 0,
        }
    }
    pub(super) fn with_parallelism(self, workers: usize) -> Self {
        self.state.0.lock().unwrap().max_workers = workers;
        self
    }
    /// Shares the worker, queue and handler without starting another thread.
    /// Client registrations are bounded by the lane's request ceiling.
    pub fn client(&self, wake: WakeSignal) -> Result<Self, RejectReason> {
        let mut state = self.state.0.lock().unwrap();
        if state.closed {
            return Err(closed());
        }
        if state.clients.len() > state.max_clients as usize {
            return Err(RejectReason::new(
                RejectCode::Busy,
                "IO reader limit reached",
            ));
        }
        let client = state.next_client;
        state.next_client = client.checked_add(1).ok_or_else(|| {
            RejectReason::new(
                RejectCode::ServiceFault,
                "IO client identity space exhausted",
            )
        })?;
        state.clients.insert(client, Client::new(wake));
        state.wakes = state.clients.values().cloned().collect();
        Ok(Self {
            state: self.state.clone(),
            cancel_event: self.cancel_event,
            client,
        })
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
    pub fn accept_before(
        &mut self,
        id: u64,
        request: R,
        bytes: u64,
        priority: Priority,
        deadline: Option<reel_core::io::ReadDeadline>,
    ) -> Result<(), Rejected<R>> {
        let mut state = self.state.0.lock().unwrap();
        if state.closed {
            return Err(Rejected {
                request,
                reason: closed(),
            });
        }
        if state.workers <= state.active.len()
            && let Some(handler) = state.handler.clone()
        {
            let shared = self.state.clone();
            let cancel_event = self.cancel_event;
            let worker = std::thread::Builder::new()
                .name("reel-resource".into())
                .spawn(move || {
                    let mut observed_quantum = Duration::ZERO;
                    'worker: loop {
                        let (mut entry, deadline) = {
                            let mut state = shared.0.lock().unwrap();
                            loop {
                                if state.closed && state.queue.usage().pending == 0 {
                                    break 'worker;
                                }
                                if let Some(entry) = state.queue.pop() {
                                    let deadline = entry
                                        .deadline()
                                        .into_iter()
                                        .chain(state.queue.earliest_deadline())
                                        .min();
                                    break (entry, deadline);
                                }
                                state = shared.1.wait(state).unwrap();
                            }
                        };
                        // Read ahead only when the next soft deadline has more
                        // slack than the slowest observed bounded quantum.
                        let started = Instant::now();
                        let read_ahead = deadline.is_none_or(|deadline| {
                            observed_quantum != Duration::ZERO
                                && deadline.saturating_duration_since(started) > observed_quantum
                        });
                        let step = handler(
                            entry.request.request,
                            &entry.request.control.cancelled,
                            read_ahead,
                        );
                        observed_quantum = observed_quantum.max(started.elapsed());
                        let mut event = match step {
                            Step::Complete(event) => event,
                            Step::Continue(request) => {
                                entry.request.request = request;
                                let mut state = shared.0.lock().unwrap();
                                if entry.request.control.promoted.load(Ordering::Relaxed) {
                                    state.queue.requeue_visible(entry);
                                } else {
                                    state.queue.requeue(entry);
                                }
                                continue;
                            }
                        };
                        // Removing the live ID settles cancellation before queuing the terminal.
                        let mut state = shared.0.lock().unwrap();
                        let active = state.active.remove(&entry.slot.id).unwrap();
                        drop(state);
                        if active.control.cancelled.load(Ordering::Acquire) {
                            cancel_event(&mut event);
                        }
                        let mut state = shared.0.lock().unwrap();
                        let wake = state.clients.get(&active.client).cloned();
                        if let Some(wake) = wake {
                            state.queue.finish(
                                Event {
                                    client: active.client,
                                    event,
                                },
                                entry.slot,
                            );
                            drop(state);
                            wake.wake.wake();
                        } else {
                            state.queue.discard(entry.slot);
                            let wakes = state.wakes.clone();
                            drop(state);
                            drop(event);
                            // Discarding a cancelled reader can release input
                            // tickets after its control has already closed. Its
                            // peers may be waiting on that budget, before queue
                            // admission, so they also need this release signal.
                            notify(&wakes);
                        }
                    }
                    let wakes = shared.0.lock().unwrap().wakes.clone();
                    notify(&wakes);
                });
            match worker {
                Ok(_) => {
                    state.workers += 1;
                    if state.workers == state.max_workers {
                        state.handler = None;
                    }
                }
                Err(error) if state.workers == 0 => {
                    return Err(Rejected {
                        request,
                        reason: RejectReason::new(RejectCode::Unavailable, error.to_string()),
                    });
                }
                Err(_) => state.handler = None,
            }
        }
        let control = Arc::new(Control::default());
        state
            .queue
            .accept_before(
                id,
                Task {
                    request,
                    control: control.clone(),
                },
                bytes,
                priority,
                deadline,
            )
            .map_err(|error| {
                if error.reason.code == RejectCode::Busy {
                    state.clients[&self.client]
                        .waiting
                        .store(true, Ordering::Release);
                }
                Rejected {
                    request: error.request.request,
                    reason: error.reason,
                }
            })?;
        state.active.insert(
            id,
            Active {
                control,
                client: self.client,
            },
        );
        drop(state);
        self.state.1.notify_one();
        Ok(())
    }
    pub fn is_idle(&self) -> bool {
        self.state.0.lock().unwrap().queue.usage().requests == 0
    }
    pub fn promote_visible(&mut self, id: u64) {
        let mut state = self.state.0.lock().unwrap();
        if let Some(active) = state.active.get(&id)
            && active.client == self.client
        {
            active.control.promoted.store(true, Ordering::Relaxed);
            state.queue.promote_visible(id);
        }
    }
    pub fn cancel(&mut self, id: u64) {
        if let Some(active) = self.state.0.lock().unwrap().active.get(&id)
            && active.client == self.client
        {
            active.control.cancelled.store(true, Ordering::Release);
        }
    }
    pub fn drain(&mut self, events: &mut Vec<E>, limit: usize) {
        let mut state = self.state.0.lock().unwrap();
        let start = events.len();
        for _ in 0..limit {
            let Some(event) = state
                .queue
                .take_finished(|event| event.client == self.client)
            else {
                break;
            };
            events.push(event.event);
        }
        if events.len() == start {
            return;
        }
        let wakes = state.wakes.clone();
        drop(state);
        notify_capacity(&wakes);
    }
}
impl<R, E> Drop for Worker<R, E> {
    fn drop(&mut self) {
        let mut state = self.state.0.lock().unwrap();
        if self.client == 0 {
            state.closed = true;
        }
        let handler = if state.closed {
            state.handler.take()
        } else {
            None
        };
        state.clients.remove(&self.client);
        state.wakes = state.clients.values().cloned().collect();
        for active in state.active.values() {
            if state.closed || active.client == self.client {
                active.control.cancelled.store(true, Ordering::Release);
            }
        }
        // Drop terminal payloads outside the lane lock. Pending owners keep
        // their slots until the worker reaches their next bounded step.
        loop {
            let event = state
                .queue
                .take_finished(|event| event.client == self.client);
            let Some(event) = event else {
                break;
            };
            drop(state);
            drop(event);
            state = self.state.0.lock().unwrap();
        }
        let wakes = state.wakes.clone();
        drop(state);
        drop(handler);
        self.state.1.notify_all();
        notify(&wakes);
    }
}

fn closed() -> RejectReason {
    RejectReason::new(RejectCode::Unavailable, "IO lane closed")
}
fn notify(wakes: &[Client]) {
    for wake in wakes {
        wake.wake.wake();
    }
}
fn notify_capacity(wakes: &[Client]) {
    for client in wakes {
        if client.waiting.swap(false, Ordering::AcqRel) {
            client.wake.wake();
        }
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
                move |mut job: Job, cancelled, _| {
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

    #[test]
    fn clients_share_global_capacity_and_drain_only_their_own_terminals() {
        let limits = QueueLimits {
            max_requests: 2,
            max_bytes: 1024,
        };
        let pool = reel_core::budget::BudgetPool::new(
            BudgetCategory::CpuInput,
            QueueLimits {
                max_requests: 8,
                ..limits
            },
        );
        let mut owner = Worker::new(
            limits,
            WakeSignal::new(|| {}),
            |job: Job, _| Event {
                id: job.id,
                cancelled: false,
                _ticket: job._ticket,
            },
            cancel,
        );
        let mut first = owner.client(WakeSignal::new(|| {})).unwrap();
        let mut second = owner.client(WakeSignal::new(|| {})).unwrap();
        assert!(
            matches!(owner.client(WakeSignal::new(|| {})), Err(reason) if reason.code == RejectCode::Busy)
        );
        for (worker, id) in [(&mut first, 1), (&mut second, 2)] {
            worker
                .accept(
                    id,
                    Job {
                        id,
                        left: 0,
                        _ticket: pool.reserve(128).unwrap(),
                    },
                    128,
                    Priority::Realtime,
                )
                .unwrap_or_else(|_| panic!("client admission"));
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while owner.state.0.lock().unwrap().queue.usage().completed != 2 {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        let mut events = Vec::new();
        owner.drain(&mut events, 2);
        assert!(events.is_empty());
        assert_eq!(owner.state.0.lock().unwrap().queue.usage().requests, 2);
        let denied = owner
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
            .unwrap_err();
        assert_eq!(denied.reason.code, RejectCode::Busy);
        drop(denied);
        first.drain(&mut events, 2);
        assert_eq!(events.iter().map(|event| event.id).collect::<Vec<_>>(), [1]);
        assert_eq!(owner.state.0.lock().unwrap().queue.usage().requests, 1);
        drop(first);
        drop(second); // Undrained client terminal releases its own slot/ticket.
        assert!(owner.is_idle());
        assert_eq!(pool.usage().bytes, 128);
        drop(events);
        assert_eq!(pool.usage().bytes, 0);
        assert!(owner.client(WakeSignal::new(|| {})).is_ok());
    }

    #[test]
    fn owner_close_cancels_live_clients_and_releases_handler_before_client_drop() {
        let limits = QueueLimits {
            max_requests: 2,
            max_bytes: 1024,
        };
        let pool = reel_core::budget::BudgetPool::new(
            BudgetCategory::CpuInput,
            QueueLimits {
                max_requests: 8,
                ..limits
            },
        );
        let residency = pool.reserve(128).unwrap();
        let (entered, ready) = mpsc::channel();
        let (resume, gate) = mpsc::channel();
        let gate = Mutex::new(gate);
        let owner = Worker::new(
            limits,
            WakeSignal::new(|| {}),
            move |job: Job, _| {
                std::hint::black_box(&residency);
                entered.send(()).unwrap();
                gate.lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap();
                Event {
                    id: job.id,
                    cancelled: false,
                    _ticket: job._ticket,
                }
            },
            cancel,
        );
        let mut client = owner.client(WakeSignal::new(|| {})).unwrap();
        client
            .accept(
                1,
                Job {
                    id: 1,
                    left: 0,
                    _ticket: pool.reserve(128).unwrap(),
                },
                128,
                Priority::Realtime,
            )
            .unwrap_or_else(|_| panic!("client admission"));
        ready.recv_timeout(Duration::from_secs(3)).unwrap();
        drop(owner);
        let denied = client
            .accept(
                2,
                Job {
                    id: 2,
                    left: 0,
                    _ticket: pool.reserve(128).unwrap(),
                },
                128,
                Priority::Realtime,
            )
            .unwrap_err();
        assert_eq!(denied.reason.code, RejectCode::Unavailable);
        drop(denied);
        resume.send(()).unwrap();
        let mut events = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(3);
        while events.is_empty() {
            assert!(Instant::now() < deadline);
            client.drain(&mut events, 1);
            std::thread::yield_now();
        }
        assert!(events[0].cancelled);
        drop(events);
        while pool.usage().bytes != 0 {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(client.is_idle());
    }

    #[test]
    fn cancelled_client_release_wakes_peers_waiting_before_queue_admission() {
        let limits = QueueLimits {
            max_requests: 2,
            max_bytes: 1024,
        };
        let input = reel_core::budget::BudgetPool::new(
            BudgetCategory::CpuInput,
            QueueLimits {
                max_requests: 2,
                max_bytes: 128,
            },
        );
        let (entered, ready) = mpsc::channel();
        let (resume, gate) = mpsc::channel();
        let gate = Mutex::new(gate);
        let owner = Worker::new(
            limits,
            WakeSignal::new(|| {}),
            move |job: Job, _| {
                entered.send(()).unwrap();
                gate.lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap();
                Event {
                    id: job.id,
                    cancelled: false,
                    _ticket: job._ticket,
                }
            },
            cancel,
        );
        let mut active = owner.client(WakeSignal::new(|| {})).unwrap();
        let wake = WakeSignal::new(|| {});
        let _waiting = owner.client(wake.clone()).unwrap();
        active
            .accept(
                1,
                Job {
                    id: 1,
                    left: 0,
                    _ticket: input.reserve(128).unwrap(),
                },
                128,
                Priority::Realtime,
            )
            .unwrap_or_else(|_| panic!("client admission"));
        ready.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(input.reserve(128).is_err());
        drop(active);
        wake.acknowledge();
        assert_eq!(
            input.usage().bytes,
            128,
            "control cancellation precedes bounded worker settlement"
        );
        resume.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !wake.is_pending() {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(input.usage().bytes, 0);
        assert!(owner.is_idle());
    }
}
