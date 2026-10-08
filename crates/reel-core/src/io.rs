//! Fast, owning IO submission. Adapters reserve a completion slot before accepting.
use crate::{
    budget::{BudgetTicket, OwnedBytes, QueueLimits},
    identity::{ContentVersion, IoId, Priority, ProcessRequestId, ResourceVersion, Scope},
    result::{Rejected, Terminal},
};

/// Visible replacement alone does not establish durability across process/device loss.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SaveDurability {
    Visible,
    Durable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IoCaps {
    pub range_read: bool,
    pub read_queue: QueueLimits,
    pub save_queue: QueueLimits,
    pub save_levels: Vec<SaveDurability>,
}

#[derive(Debug)]
pub struct DescribeRequest {
    pub resource: ResourceVersion,
    pub priority: Priority,
    pub scope: Scope,
    pub completion: BudgetTicket,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetInfo {
    pub length: Option<u64>,
    pub range_read: bool,
    pub seekable: bool,
    pub version: ContentVersion,
    pub media_type: Option<String>,
}

#[derive(Debug)]
pub struct ReadRequest {
    pub resource: ResourceVersion,
    pub offset: u64,
    pub len: u32,
    pub priority: Priority,
    pub scope: Scope,
    pub output: BudgetTicket,
    pub completion: BudgetTicket,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SaveSlot {
    pub namespace: String,
    pub slot: String,
}

/// Always process-owned; cancellation of a session does not cancel captured writes.
#[derive(Debug)]
pub struct SaveWrite {
    pub identity: ProcessRequestId,
    pub slot: SaveSlot,
    pub commit_sequence: u64,
    pub data: OwnedBytes,
    pub minimum_durability: SaveDurability,
    pub completion: BudgetTicket,
}

#[derive(Debug)]
pub struct ReadResult {
    pub bytes: OwnedBytes,
    pub eof: bool,
    pub version: ContentVersion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SaveCommit {
    pub identity: ProcessRequestId,
    pub commit_sequence: u64,
    pub durability: SaveDurability,
}

#[derive(Debug)]
pub enum IoPayload {
    Described(AssetInfo),
    Read(ReadResult),
    Saved(SaveCommit),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IoErrorCode {
    NotFound,
    VersionUnavailable,
    StorageFailure,
    OutOfMemory,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IoError {
    pub code: IoErrorCode,
    pub message: String,
}

#[derive(Debug)]
pub struct IoEvent {
    pub id: IoId,
    pub scope: Scope,
    /// Kept even for failed/cancelled writes, for exact process-ledger correlation.
    pub process_request: Option<ProcessRequestId>,
    pub result: Terminal<IoPayload, IoError>,
    pub completion: BudgetTicket,
}

/// `poll` appends at most `limit` events. Callers preallocate their output vector.
/// Before cancellation, the business layer revokes its wait/subscription. The
/// engine delivers a result only if that wait is still active and Scope matches.
/// Cancelled events release/account resources without resuming cancelled business.
/// Already settled events retain their real terminal result for resource and
/// process-ledger accounting, including a save that actually committed.
pub trait AssetIo {
    fn capabilities(&self) -> IoCaps;
    fn describe(&mut self, request: DescribeRequest) -> Result<IoId, Rejected<DescribeRequest>>;
    fn read(&mut self, request: ReadRequest) -> Result<IoId, Rejected<ReadRequest>>;
    fn replace_save(&mut self, request: SaveWrite) -> Result<IoId, Rejected<SaveWrite>>;
    /// If unsettled, settle Cancelled once and suppress later business output.
    /// If already settled/queued, preserve that terminal; cancellation is a no-op.
    /// A caller must first revoke its business wait, even if the IO cannot abort.
    fn cancel(&mut self, id: IoId);
    fn poll(&mut self, out: &mut Vec<IoEvent>, limit: usize);
}
