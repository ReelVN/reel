//! Code-independent checkpoint storage and coordination.

mod capture;
mod context;
mod flow;
mod interaction;
mod load;
mod record;
mod restore;
pub use capture::{CapturedCheckpoint, CheckpointCapture, CheckpointError};
#[cfg(test)]
pub(in crate::aside) use flow::FlowPreparation;
pub(in crate::aside) use flow::{FlowRuntime, flow_continuation};
pub(in crate::aside) use flow::{contracts, describe, documentation};
pub(in crate::aside) use interaction::ReadingResume;
pub use load::CheckpointLoad;
pub use record::{CheckpointBinding, CheckpointRecord};
pub use restore::{CheckpointDataRestore, CheckpointResumeCursor, RestoredCheckpoint};
