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
type Handler<R, E> = dyn Fn(R, &AtomicBool) -> E + Send + Sync;

pub(super) fn check_completion(ticket: &BudgetTicket) -> Result<(), RejectReason> {
    if ticket.category() != BudgetCategory::Completion || ticket.bytes() < 512 {
        return Err(invalid("completion ticket missing or too small"));
    }
    Ok(())
}

/// One bounded background lane, shared by all resource kinds using the adapter.
pub(super) struct Worker<R, E> {
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
        Self {
            state: Arc::new((
                Mutex::new(State {
                    queue: Queue::new(limits),
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
                        let entry = {
                            let mut state = shared.0.lock().unwrap();
                            loop {
                                if state.closed {
                                    break 'worker;
                                }
                                if let Some(entry) = state.queue.pop() {
                                    break entry;
                                }
                                state = shared.1.wait(state).unwrap();
                            }
                        };
                        let mut event = handler(entry.request.request, &entry.request.cancelled);
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
                                break;
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
