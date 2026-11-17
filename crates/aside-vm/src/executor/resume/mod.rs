//! Logical continuation access uses the ordinary task, frame, cell and root stores.
use super::*;
use crate::resume::{
    BindingKey, ContinuationDescriptor, FrameDescriptor, ResumeError, ResumeSite,
    VerifiedResumePlan, VerifiedResumeTable,
};
mod capture;
mod descriptor;
mod restore;
pub use capture::{CaptureProgress, CapturedBinding, ResumeCapture, ResumeCaptureCursor};
pub use descriptor::{
    ResumeDescriptor, ResumeDescriptorBound, ResumeFrameRef, ResumeFrames, ResumeSiteRef,
};
pub use restore::{
    ResumeFrameValues, ResumeProgress, ResumeRestore, ResumeRestoreCursor, ResumeValue,
};

fn error(error: impl std::fmt::Debug) -> ResumeError {
    ResumeError::new(format!("resume VM operation: {error:?}"))
}
