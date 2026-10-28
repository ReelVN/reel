//! One native output port, retained across output/session ownership transfers.
use crate::{AudioError, AudioErrorKind};
use std::sync::Mutex;

pub(crate) trait RetiringStream: Send {
    fn begin_stop(&self) -> Result<(), AudioError>;
    fn poll_stop(&mut self) -> Result<bool, AudioError>;
}
enum Slot<S> {
    Available,
    Active(u64),
    Closing(u64, S),
}
struct Inner<S> {
    slot: Slot<S>,
    next: u64,
    completed: u64,
    error: Option<AudioError>,
}
pub(crate) struct NativePort<S>(Mutex<Inner<S>>);
impl<S: RetiringStream> NativePort<S> {
    pub fn new() -> Self {
        Self(Mutex::new(Inner {
            slot: Slot::Available,
            next: 1,
            completed: 0,
            error: None,
        }))
    }
    fn poll(inner: &mut Inner<S>) {
        if let Slot::Closing(id, stream) = &mut inner.slot {
            match stream.poll_stop() {
                Ok(true) => {
                    inner.completed = *id;
                    inner.slot = Slot::Available;
                    inner.error = None;
                }
                Ok(false) => {}
                Err(error) => inner.error = Some(error),
            }
        }
    }
    pub fn reserve(&self) -> Result<u64, AudioError> {
        let mut inner = self.0.lock().expect("native port poisoned");
        Self::poll(&mut inner);
        if !matches!(inner.slot, Slot::Available) {
            return Err(inner.error.clone().unwrap_or_else(|| {
                AudioError::new(
                    AudioErrorKind::Busy,
                    "previous native audio port has not closed",
                )
            }));
        }
        let next = inner.next.checked_add(1).ok_or_else(|| {
            AudioError::new(
                AudioErrorKind::Unavailable,
                "native audio port generation exhausted",
            )
        })?;
        let id = inner.next;
        inner.next = next;
        inner.slot = Slot::Active(id);
        Ok(id)
    }
    pub fn reject_open(&self, id: u64) {
        let mut inner = self.0.lock().expect("native port poisoned");
        assert!(matches!(inner.slot, Slot::Active(active) if active == id));
        inner.completed = id;
        inner.slot = Slot::Available;
    }
    pub fn retire(&self, id: u64, stream: S) -> Result<(), AudioError> {
        let mut inner = self.0.lock().expect("native port poisoned");
        assert!(matches!(inner.slot, Slot::Active(active) if active == id));
        let result = stream.begin_stop();
        inner.error = result.as_ref().err().cloned();
        inner.slot = Slot::Closing(id, stream);
        result
    }
    pub fn closed(&self, id: u64) -> bool {
        let mut inner = self.0.lock().expect("native port poisoned");
        Self::poll(&mut inner);
        inner.completed >= id
    }
    pub fn retry(&self, id: u64) -> Result<(), AudioError> {
        let mut inner = self.0.lock().expect("native port poisoned");
        if let Slot::Closing(closing, stream) = &inner.slot
            && *closing == id
        {
            stream.begin_stop()?;
            inner.error = None;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    struct Stream {
        ready: Arc<AtomicBool>,
        failure: Arc<AtomicBool>,
        drops: Arc<AtomicUsize>,
    }
    impl RetiringStream for Stream {
        fn begin_stop(&self) -> Result<(), AudioError> {
            self.failure.store(false, Ordering::Release);
            Ok(())
        }
        fn poll_stop(&mut self) -> Result<bool, AudioError> {
            if self.failure.load(Ordering::Acquire) {
                Err(AudioError::new(
                    AudioErrorKind::DeviceFailure,
                    "release rejected",
                ))
            } else {
                Ok(self.ready.load(Ordering::Acquire))
            }
        }
    }
    impl Drop for Stream {
        fn drop(&mut self) {
            assert!(self.ready.load(Ordering::Acquire));
            self.drops.fetch_add(1, Ordering::Release);
        }
    }
    #[test]
    fn pending_and_failed_close_retain_one_real_owner_and_generation_receipt_survives_replacement()
    {
        let ready = Arc::new(AtomicBool::new(false));
        let failure = Arc::new(AtomicBool::new(false));
        let drops = Arc::new(AtomicUsize::new(0));
        let port = NativePort::new();
        let first = port.reserve().unwrap();
        assert_eq!(port.reserve().unwrap_err().kind, AudioErrorKind::Busy);
        port.retire(
            first,
            Stream {
                ready: ready.clone(),
                failure: failure.clone(),
                drops: drops.clone(),
            },
        )
        .unwrap();
        assert!(!port.closed(first));
        assert_eq!(port.reserve().unwrap_err().kind, AudioErrorKind::Busy);
        assert_eq!(drops.load(Ordering::Acquire), 0);
        failure.store(true, Ordering::Release);
        assert!(!port.closed(first));
        assert_eq!(
            port.reserve().unwrap_err().kind,
            AudioErrorKind::DeviceFailure
        );
        assert_eq!(drops.load(Ordering::Acquire), 0);
        port.retry(first).unwrap();
        ready.store(true, Ordering::Release);
        assert!(port.closed(first));
        assert_eq!(drops.load(Ordering::Acquire), 1);
        let second = port.reserve().unwrap();
        assert_ne!(first, second);
        assert!(
            port.closed(first),
            "older output receipt is independent of newer active port"
        );
        assert!(!port.closed(second));
        port.reject_open(second);
        assert!(port.closed(second));
    }
}
