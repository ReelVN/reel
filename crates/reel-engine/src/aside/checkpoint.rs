//! Code-independent checkpoint storage and coordination.

mod capture;
mod continuation;
mod restore;
pub use capture::{CapturedContinuation, ContinuationCapture, ContinuationError};
pub use continuation::ContinuationRecord;
pub use restore::ContinuationRestore;
