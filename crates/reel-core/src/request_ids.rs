//! A service-owned, shareable counter; replacement adapters reuse the same stream.
use crate::{
    identity::{IoId, RequestId, WorkId},
    result::{RejectCode, RejectReason, Rejected},
};
use std::{
    marker::PhantomData,
    sync::{Arc, Mutex},
};

pub trait RequestIdentity: Copy {
    fn from_counter(value: u64) -> Self;
}
impl RequestIdentity for IoId {
    fn from_counter(value: u64) -> Self {
        Self(value)
    }
}
impl RequestIdentity for WorkId {
    fn from_counter(value: u64) -> Self {
        Self(value)
    }
}
impl RequestIdentity for RequestId {
    fn from_counter(value: u64) -> Self {
        Self(value)
    }
}

/// No wraparound, no 64-bit atomics, and no counter advancement on rejection.
/// Share clones across adapters in one service/request domain. Independent process
/// ledgers are additionally distinguished by `ProcessId`.
#[derive(Clone, Debug)]
pub struct RequestIds<I> {
    next: Arc<Mutex<Option<u64>>>,
    marker: PhantomData<fn() -> I>,
}

impl<I: RequestIdentity> Default for RequestIds<I> {
    fn default() -> Self {
        Self::starting_at(1)
    }
}

impl<I: RequestIdentity> RequestIds<I> {
    /// Resume from a service's recorded high-water mark, or use a reduced range in tests.
    pub fn starting_at(first: u64) -> Self {
        Self {
            next: Arc::new(Mutex::new(Some(first))),
            marker: PhantomData,
        }
    }

    /// The callback performs only admission, never user code or a nested allocation
    /// from this same ID stream. Publish an accepted ID only after it succeeds.
    pub fn accept<R>(
        &self,
        request: R,
        admit: impl FnOnce(I, R) -> Result<(), Rejected<R>>,
    ) -> Result<I, Rejected<R>> {
        let mut next = self.next.lock().expect("request ID stream poisoned");
        let Some(value) = *next else {
            return Err(Rejected {
                request,
                reason: RejectReason::new(
                    RejectCode::ServiceFault,
                    "request identity space exhausted",
                ),
            });
        };
        let id = I::from_counter(value);
        admit(id, request)?;
        *next = value.checked_add(1);
        Ok(id)
    }
}
