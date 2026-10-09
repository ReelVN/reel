//! Coalesced control-side notification. This generic callback has no realtime guarantee.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

struct WakeState {
    pending: AtomicBool,
    notify: Box<dyn Fn() + Send + Sync>,
}

/// Inject the platform's short, nonblocking notification callback at assembly time.
#[derive(Clone)]
pub struct WakeSignal(Arc<WakeState>);

impl WakeSignal {
    pub fn new(notify: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(WakeState {
            pending: AtomicBool::new(false),
            notify: Box::new(notify),
        }))
    }
    pub fn wake(&self) {
        if !self.0.pending.swap(true, Ordering::AcqRel) {
            (self.0.notify)();
        }
    }
    /// Loop clears *before* draining all sources, then checks pending before sleeping.
    /// A racing producer either joins this drain or sends a new notification.
    pub fn acknowledge(&self) -> bool {
        self.0.pending.swap(false, Ordering::AcqRel)
    }
    pub fn is_pending(&self) -> bool {
        self.0.pending.load(Ordering::Acquire)
    }
}

impl std::fmt::Debug for WakeSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WakeSignal")
            .field("pending", &self.is_pending())
            .finish_non_exhaustive()
    }
}
