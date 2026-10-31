//! Opt-in, thread-local wall ledger. Boundaries reuse Scope's existing clock
//! reads. Each nanosecond belongs to one innermost scope, unscoped work, or loss.
use super::{NAMES, Phase};
use std::{cell::RefCell, marker::PhantomData, rc::Rc, time::Instant};

const CAPACITY: usize = 64;
thread_local! {
    static STATE: RefCell<Option<Box<State>>> = const { RefCell::new(None) };
}
static EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Main-thread interval counters, in nanoseconds. These are exclusive wall
/// durations, including descheduling and waits, rather than CPU/GPU service time.
/// `calls` counts entries, including entries whose scope outlives the snapshot.
#[derive(Clone, Debug)]
pub struct WallSnapshot {
    epoch: u64,
    /// Elapsed wall time, partitioned exactly by `nanos`, `unscoped_ns`, `lost_ns`.
    pub wall_ns: u64,
    /// Time charged to each innermost named phase.
    pub nanos: [u64; NAMES.len()],
    /// Phase entry counts, on the recording thread only.
    pub calls: [u64; NAMES.len()],
    /// Time outside all instrumented scopes, including instrumentation overhead.
    pub unscoped_ns: u64,
    /// Time whose owner is unavailable because the bounded stack overflowed.
    pub lost_ns: u64,
    /// Stack overflow or out-of-order scope destruction events.
    pub errors: u64,
    /// Clock reads reused from scopes plus explicit snapshot reads.
    pub clock_reads: u64,
}
impl WallSnapshot {
    /// Whether this thread currently records exclusive wall intervals.
    pub fn is_active() -> bool {
        STATE.with_borrow(|state| state.is_some())
    }
    fn new(epoch: u64) -> Self {
        Self {
            epoch,
            wall_ns: 0,
            nanos: [0; NAMES.len()],
            calls: [0; NAMES.len()],
            unscoped_ns: 0,
            lost_ns: 0,
            errors: 0,
            clock_reads: 0,
        }
    }

    /// Snapshot only the calling thread. Active scopes are charged up to this
    /// boundary, so completed and unfinished work share the same interval.
    pub fn capture() -> Option<Self> {
        STATE.with_borrow_mut(|slot| {
            let state = slot.as_mut()?;
            state.sample.clock_reads += 1;
            let now = state.time(Instant::now());
            state.charge(now);
            Some(state.sample.clone())
        })
    }

    /// Difference within one recording. Replaced or disabled recordings yield
    /// no observation; stack errors remain explicit in the returned counters.
    pub fn checked_delta(&self, before: &Self) -> Option<Self> {
        if self.epoch != before.epoch || self.wall_ns < before.wall_ns {
            return None;
        }
        Some(Self {
            epoch: self.epoch,
            wall_ns: self.wall_ns - before.wall_ns,
            nanos: std::array::from_fn(|i| self.nanos[i] - before.nanos[i]),
            calls: std::array::from_fn(|i| self.calls[i] - before.calls[i]),
            unscoped_ns: self.unscoped_ns - before.unscoped_ns,
            lost_ns: self.lost_ns - before.lost_ns,
            errors: self.errors - before.errors,
            clock_reads: self.clock_reads - before.clock_reads,
        })
    }
}

/// Enables a bounded wall ledger on this thread until dropped. A second live
/// recording is rejected. Allocation occurs once here, never at scope boundaries.
/// Uninstrumented builds omit this API; profiling builds keep it opt-in.
pub struct WallRecording {
    epoch: u64,
    owner: PhantomData<Rc<()>>,
}
impl WallRecording {
    /// Start on the current thread; returns None when it already owns a ledger.
    pub fn start() -> Option<Self> {
        STATE.with_borrow_mut(|slot| {
            if slot.is_some() {
                return None;
            }
            let epoch = EPOCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            *slot = Some(Box::new(State::new(epoch)));
            Some(Self {
                epoch,
                owner: PhantomData,
            })
        })
    }
}
impl Drop for WallRecording {
    fn drop(&mut self) {
        STATE.with_borrow_mut(|slot| {
            if slot.as_ref().is_some_and(|s| s.sample.epoch == self.epoch) {
                *slot = None;
            }
        });
    }
}

pub(super) struct Token {
    epoch: u64,
    id: u64,
    overflow: bool,
}
#[derive(Clone, Copy)]
struct Entry {
    id: u64,
    phase: usize,
}
struct State {
    origin: Instant,
    sample: WallSnapshot,
    stack: [Entry; CAPACITY],
    depth: usize,
    overflow: usize,
    serial: u64,
}
impl State {
    fn new(epoch: u64) -> Self {
        Self {
            origin: Instant::now(),
            sample: WallSnapshot::new(epoch),
            stack: [Entry { id: 0, phase: 0 }; CAPACITY],
            depth: 0,
            overflow: 0,
            serial: 0,
        }
    }
    fn time(&self, at: Instant) -> u64 {
        at.saturating_duration_since(self.origin).as_nanos() as u64
    }
    fn charge(&mut self, now: u64) {
        let elapsed = now - self.sample.wall_ns;
        if self.overflow != 0 {
            self.sample.lost_ns += elapsed;
        } else if self.depth != 0 {
            self.sample.nanos[self.stack[self.depth - 1].phase] += elapsed;
        } else {
            self.sample.unscoped_ns += elapsed;
        }
        self.sample.wall_ns = now;
    }
    fn enter(&mut self, phase: Phase, now: u64) -> Token {
        self.charge(now);
        self.sample.clock_reads += 1;
        self.sample.calls[phase as usize] += 1;
        self.serial += 1;
        let overflow = self.depth == CAPACITY || self.overflow != 0;
        if overflow {
            self.overflow += 1;
            self.sample.errors += 1;
        } else {
            self.stack[self.depth] = Entry {
                id: self.serial,
                phase: phase as usize,
            };
            self.depth += 1;
        }
        Token {
            epoch: self.sample.epoch,
            id: self.serial,
            overflow,
        }
    }
    fn leave(&mut self, token: Token, now: u64) {
        if token.epoch != self.sample.epoch {
            return;
        }
        self.charge(now);
        self.sample.clock_reads += 1;
        if token.overflow {
            self.overflow -= 1;
        } else if self.depth != 0 && self.stack[self.depth - 1].id == token.id {
            if self.overflow != 0 {
                self.sample.errors += 1;
            }
            self.depth -= 1;
        } else if let Some(index) = self.stack[..self.depth]
            .iter()
            .position(|s| s.id == token.id)
        {
            if index + 1 != self.depth || self.overflow != 0 {
                self.sample.errors += 1;
            }
            self.stack.copy_within(index + 1..self.depth, index);
            self.depth -= 1;
        } else {
            self.sample.errors += 1;
        }
    }
}
pub(super) fn begin(phase: Phase, at: Instant) -> Option<Token> {
    STATE.with_borrow_mut(|slot| {
        let state = slot.as_mut()?;
        Some(state.enter(phase, state.time(at)))
    })
}
pub(super) fn end(token: Option<Token>, at: Instant) {
    if let Some(token) = token {
        STATE.with_borrow_mut(|slot| {
            if let Some(state) = slot {
                state.leave(token, state.time(at));
            }
        });
    }
}

#[cfg(test)]
mod tests;
