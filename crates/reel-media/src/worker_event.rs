//! One worker's retained notification and relative timeout. Vita's pthread
//! timed waits currently interpret Rust's monotonic deadline as wall time;
//! keep that platform difference at this boundary.
use std::time::Duration;

#[cfg(target_os = "vita")]
mod platform {
    use super::Duration;
    use std::sync::Arc;
    use vita_native::app::wake::Wake;

    pub(super) struct Event(Arc<Wake>);
    impl Event {
        pub fn new() -> Result<Self, String> {
            Wake::new().map(Self).map_err(|error| error.to_string())
        }
        pub fn signal(&self) -> Result<(), String> {
            self.0.signal().map_err(|error| error.to_string())
        }
        pub fn wait(&self, timeout: Option<Duration>) -> Result<(), String> {
            self.0
                .wait(timeout)
                .map(|_| ())
                .map_err(|error| error.to_string())
        }
    }
}

#[cfg(not(target_os = "vita"))]
mod platform {
    use super::Duration;
    use std::sync::{Condvar, Mutex};

    pub(super) struct Event {
        pending: Mutex<bool>,
        available: Condvar,
    }
    impl Event {
        pub fn new() -> Result<Self, String> {
            Ok(Self {
                pending: Mutex::new(false),
                available: Condvar::new(),
            })
        }
        pub fn signal(&self) -> Result<(), String> {
            *self.pending.lock().expect("worker event poisoned") = true;
            self.available.notify_one();
            Ok(())
        }
        pub fn wait(&self, timeout: Option<Duration>) -> Result<(), String> {
            let mut pending = self.pending.lock().expect("worker event poisoned");
            if let Some(timeout) = timeout {
                pending = self
                    .available
                    .wait_timeout_while(pending, timeout, |pending| !*pending)
                    .expect("worker event poisoned")
                    .0;
            } else {
                pending = self
                    .available
                    .wait_while(pending, |pending| !*pending)
                    .expect("worker event poisoned");
            }
            *pending = false;
            Ok(())
        }
    }
}

/// A single waiter's coalescing token. The producer publishes its predicate
/// before signaling; the worker drops any other lock before waiting, then reads
/// its predicate again. This interface is never called from the PCM callback.
pub(crate) struct WorkerEvent(platform::Event);
impl WorkerEvent {
    pub fn new() -> Result<Self, String> {
        platform::Event::new().map(Self)
    }
    pub fn signal(&self) -> Result<(), String> {
        self.0.signal()
    }
    pub fn wait(&self, timeout: Option<Duration>) -> Result<(), String> {
        self.0.wait(timeout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{Arc, mpsc},
        thread,
    };

    #[test]
    fn signal_before_wait_is_retained_and_consumed() {
        let event = Arc::new(WorkerEvent::new().unwrap());
        event.signal().unwrap();
        let (done, observed) = mpsc::channel();
        let worker = thread::spawn(move || {
            event.wait(None).unwrap();
            done.send(()).unwrap();
            event.wait(Some(Duration::from_millis(2))).unwrap();
        });
        observed.recv_timeout(Duration::from_secs(1)).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn signal_interrupts_a_timed_wait() {
        let event = Arc::new(WorkerEvent::new().unwrap());
        let waiting = event.clone();
        let (done, observed) = mpsc::channel();
        let worker = thread::spawn(move || {
            waiting.wait(Some(Duration::from_secs(5))).unwrap();
            done.send(()).unwrap();
        });
        event.signal().unwrap();
        observed.recv_timeout(Duration::from_secs(1)).unwrap();
        worker.join().unwrap();
    }
}
