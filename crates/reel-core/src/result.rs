//! Rejection never transfers ownership; acceptance owes exactly one terminal event.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectCode {
    Busy,
    Unsupported,
    InvalidRequest,
    Unavailable,
    OutOfMemory,
    ServiceFault,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RejectReason {
    pub code: RejectCode,
    pub message: String,
}

impl RejectReason {
    pub fn new(code: RejectCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn retryable(&self) -> bool {
        matches!(self.code, RejectCode::Busy | RejectCode::Unavailable)
    }
}

#[derive(Debug)]
pub struct Rejected<T> {
    pub request: T,
    pub reason: RejectReason,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Terminal<T, E> {
    Completed(T),
    Cancelled,
    Failed(E),
}
