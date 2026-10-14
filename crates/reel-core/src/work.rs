//! Registered finite operations only; never a script closure or borrowed VM value.
use crate::{
    budget::{BudgetTicket, OwnedBytes, QueueLimits},
    clock::MonoTime,
    identity::{Priority, Scope, WorkId},
    result::{Rejected, Terminal},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkOperation {
    Decode,
    Encode,
    Verify,
}

/// Registration selects both stable operation identity and its input format version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkKind {
    pub name: String,
    pub version: u32,
    pub operation: WorkOperation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkMode {
    Threaded,
    Cooperative,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkCaps {
    pub mode: WorkMode,
    pub kinds: Vec<WorkKind>,
    pub queue: QueueLimits,
}

/// Obtained before submission. Unknown sizes require a separate bounded probe first.
#[derive(Debug)]
pub struct WorkReservations {
    pub output: BudgetTicket,
    pub temporary: BudgetTicket,
    pub completion: BudgetTicket,
}

#[derive(Debug)]
pub struct WorkRequest {
    pub kind: WorkKind,
    pub input: std::sync::Arc<OwnedBytes>,
    pub scope: Scope,
    pub priority: Priority,
    pub reservations: WorkReservations,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkBudget {
    pub max_units: u32,
    pub max_bytes: u64,
    pub deadline: Option<MonoTime>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkProgress {
    pub units: u32,
    /// Logical input bytes handled this slice, counted once despite scratch/output
    /// copies. Maximum output and temporary residency are reserved separately.
    pub bytes: u64,
    pub has_work: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkErrorCode {
    InvalidInput,
    OutOfMemory,
    OperationFailed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkError {
    pub code: WorkErrorCode,
    pub message: String,
}

#[derive(Debug)]
pub struct WorkEvent {
    pub id: WorkId,
    pub scope: Scope,
    pub result: Terminal<OwnedBytes, WorkError>,
    pub completion: BudgetTicket,
}

/// Owning bounded admission and at most one terminal event per accepted request.
/// The business layer revokes waits/subscriptions before calling `cancel`. The
/// engine checks both wait activity and Scope before restoring business execution.
/// Settled events remain available for resource accounting after business cancel.
pub trait WorkExecutor {
    fn capabilities(&self) -> WorkCaps;
    fn submit(&mut self, work: WorkRequest) -> Result<WorkId, Rejected<WorkRequest>>;
    /// Unsettled work produces Cancelled once, even if underlying work cannot stop
    /// immediately. An already queued terminal keeps its actual result; it must
    /// not resume a revoked business wait. Repeated cancellation is a no-op.
    fn cancel(&mut self, id: WorkId);
    fn advance(&mut self, budget: WorkBudget) -> WorkProgress;
    fn drain(&mut self, out: &mut Vec<WorkEvent>, limit: usize);
}
