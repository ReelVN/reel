//! Non-RT decoder control. PCM publishes only wanted/serial/block-credit atoms.
use super::*;
use crate::{maintenance::ServiceShared, worker_event::WorkerEvent};
use std::sync::{OnceLock, Weak};

const HELD: usize = 0;
const ACTIVE: usize = 1;
const QUEUED: usize = 2;
/// Weak transport descriptors can survive the PCM owner. Budget their retained
/// control allocation independently of the live decoder/window reservation.
pub(in crate::mixer) const CONTROL_BYTES: u64 =
    (size_of::<Control>() + 2 * size_of::<usize>()) as u64;

pub(in crate::mixer) struct Control {
    available: WorkerEvent,
    ready: WorkerEvent,
    mode: AtomicUsize,
    published: AtomicU64,
    finished: AtomicBool,
    source: OnceLock<Weak<Lease>>,
    host: OnceLock<Weak<ServiceShared>>,
    #[cfg(test)]
    pub(in crate::mixer) waits: AtomicUsize,
}
impl std::fmt::Debug for Control {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecodeControl").finish_non_exhaustive()
    }
}
impl Control {
    pub(in crate::mixer) fn new() -> Result<Self, String> {
        Ok(Self {
            available: WorkerEvent::new()?,
            ready: WorkerEvent::new()?,
            mode: AtomicUsize::new(HELD),
            published: AtomicU64::new(u64::MAX),
            finished: AtomicBool::new(false),
            source: OnceLock::new(),
            host: OnceLock::new(),
            #[cfg(test)]
            waits: AtomicUsize::new(0),
        })
    }
    pub(super) fn bind_source(&self, source: Weak<Lease>) {
        assert!(self.source.set(source).is_ok(), "one decoder source");
    }
    pub(in crate::mixer) fn bind_host(&self, host: &Arc<ServiceShared>) {
        // Admission retries use the same controller. No strong host/source cycle.
        let _ = self.host.set(Arc::downgrade(host));
    }
    fn notify_host(&self) {
        if let Some(host) = self.host.get().and_then(Weak::upgrade) {
            host.notify_control();
        }
    }
    fn signal(&self) {
        if let Err(error) = self.available.signal() {
            if let Some(source) = self.source.get().and_then(Weak::upgrade) {
                source.window.status.fail(error);
                source.window.stopped.store(true, Ordering::Release);
            }
            self.notify_host();
        }
    }
    pub(in crate::mixer) fn mode(&self, active: bool, queued: bool) {
        let mode = if active {
            ACTIVE
        } else if queued {
            QUEUED
        } else {
            HELD
        };
        if self.mode.swap(mode, Ordering::AcqRel) != mode {
            self.signal();
        }
    }
    pub(in crate::mixer) fn seek(&self, index: usize) {
        if let Some(source) = self.source.get().and_then(Weak::upgrade) {
            source.seek(index);
            // An uncached paused seek must return old block owners before the
            // producer can refill; this bounded read runs only on control.
            source.read_frames(index, &mut [Frame::ZERO]);
            self.signal();
        }
    }
    pub(in crate::mixer) fn prepared(&self) -> bool {
        self.source
            .get()
            .and_then(Weak::upgrade)
            .is_none_or(|source| {
                source.window.status.failed()
                    || self.published.load(Ordering::Acquire)
                        == source.window.serial.load(Ordering::Acquire)
            })
    }
    pub(in crate::mixer) fn publish(&self, serial: u64) {
        if self.published.swap(serial, Ordering::AcqRel) != serial {
            let _ = self.ready.signal();
            self.notify_host();
        }
    }
    pub(in crate::mixer) fn finish(&self) {
        self.finished.store(true, Ordering::Release);
        let _ = self.ready.signal();
        self.notify_host();
    }
    pub(in crate::mixer) fn stop(&self) {
        self.signal();
    }
    pub(in crate::mixer) fn wait(&self, moving: bool) -> Result<(), String> {
        let timeout = match self.mode.load(Ordering::Acquire) {
            // These sources cannot consume PCM until an explicit control action.
            HELD => None,
            // Future segments can become current entirely inside the PCM mixer.
            // They retain a finite demand fallback even without a host poll.
            QUEUED => Some(Duration::from_millis(16)),
            _ => Some(Duration::from_millis(if moving { 1 } else { 8 })),
        };
        #[cfg(test)]
        self.waits.fetch_add(1, Ordering::Relaxed);
        self.available.wait(timeout)
    }
    pub(in crate::mixer) fn wait_initial(&self, cancelled: &AtomicBool) -> Result<(), String> {
        while self.published.load(Ordering::Acquire) == u64::MAX
            && !self.finished.load(Ordering::Acquire)
        {
            if cancelled.load(Ordering::Acquire) {
                return Err("audio preparation cancelled".into());
            }
            // The preparation job's borrowed cancellation flag has no event
            // subscription. Only this initialization window polls it at 10 ms.
            self.ready.wait(Some(Duration::from_millis(10)))?;
        }
        if cancelled.load(Ordering::Acquire) {
            return Err("audio preparation cancelled".into());
        }
        if let Some(source) = self.source.get().and_then(Weak::upgrade)
            && let Some(error) = source.window.status.error()
        {
            return Err(error);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
