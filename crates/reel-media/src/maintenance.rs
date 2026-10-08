use crate::{AudioError, AudioErrorKind};
use reel_core::wake::WakeSignal;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

/// Only atomics are reachable from PCM. The generic wake callback stays on a
/// control notifier, and publication is signalled after Kira finishes retirement.
#[derive(Default)]
pub(crate) struct RetirementSignal {
    pub observed: AtomicBool,
    pub pending: AtomicBool,
}
pub(crate) struct ReclaimNotifier {
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl ReclaimNotifier {
    pub fn new(signal: Arc<RetirementSignal>, wake: WakeSignal) -> Result<Self, AudioError> {
        let shutdown = Arc::new(AtomicBool::new(false));
        let quitting = shutdown.clone();
        let thread = thread::Builder::new()
            .name("reel-audio-reclaim".into())
            .spawn(move || {
                while !quitting.load(Ordering::Acquire) {
                    if signal.pending.swap(false, Ordering::AcqRel) {
                        wake.wake();
                    }
                    thread::park_timeout(Duration::from_millis(2));
                }
            })
            .map_err(|e| AudioError::new(AudioErrorKind::Unavailable, e.to_string()))?;
        Ok(Self {
            shutdown,
            thread: Some(thread),
        })
    }
}
impl Drop for ReclaimNotifier {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}
