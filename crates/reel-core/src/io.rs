//! Fast, owning IO submission. Adapters reserve a completion slot before accepting.
use crate::{
    budget::{BudgetTicket, OwnedBytes, QueueLimits},
    identity::{AssetSource, ContentVersion, IoId, Priority, ProcessRequestId, Scope},
    result::{RejectCode, RejectReason, Rejected, Terminal},
    wake::WakeSignal,
};

/// A decoder-worker hint derived from actual buffered source duration. Deadlines
/// are monotonic and soft: expiry changes scheduling order, never IO success.
/// Clearing it while paused prevents a stale deadline surviving suspension.
#[derive(Clone, Debug, Default)]
pub struct ReadDeadline(std::sync::Arc<DeadlineState>);
#[derive(Debug, Default)]
struct DeadlineState {
    value: std::sync::Mutex<DeadlineValue>,
    _ticket: Option<BudgetTicket>,
}
#[derive(Debug)]
struct DeadlineValue {
    deadline: Option<std::time::Instant>,
    paused: bool,
    rate: f64,
}
impl Default for DeadlineValue {
    fn default() -> Self {
        Self {
            deadline: None,
            paused: false,
            rate: 1.,
        }
    }
}
impl ReadDeadline {
    /// Account this shared allocation until its last publisher or queued reader
    /// releases it. Ticket lifetime is independent of stream cancellation.
    pub fn charged(pool: &crate::budget::BudgetPool) -> Result<Self, RejectReason> {
        let ticket = pool.reserve((size_of::<DeadlineState>() + 2 * size_of::<usize>()) as u64)?;
        Ok(Self(std::sync::Arc::new(DeadlineState {
            value: Default::default(),
            _ticket: Some(ticket),
        })))
    }
    /// Create an absolute monotonic hint for externally measured buffers.
    pub fn at(deadline: std::time::Instant) -> Self {
        Self(std::sync::Arc::new(DeadlineState {
            value: std::sync::Mutex::new(DeadlineValue {
                deadline: Some(deadline),
                ..Default::default()
            }),
            _ticket: None,
        }))
    }
    /// Publish decoded duration from a preparation worker, never the audio callback.
    pub fn buffered(&self, duration: std::time::Duration) {
        let mut value = self.0.value.lock().unwrap();
        if !value.paused {
            value.deadline = std::time::Instant::now().checked_add(duration.div_f64(value.rate));
        }
    }
    /// Clear the current hint during suspension, cancellation or EOF.
    pub fn clear(&self) {
        self.0.value.lock().unwrap().deadline = None;
    }
    /// Suspend publication while playback is paused or a queued segment is held.
    /// Resume requests prompt service until the decoder can publish a fresh buffer
    /// measurement, including when it is already blocked inside an owning read.
    pub fn pause(&self, paused: bool) {
        let mut value = self.0.value.lock().unwrap();
        if value.paused != paused {
            value.paused = paused;
            value.deadline = (!paused).then(std::time::Instant::now);
        }
    }
    /// Set the maximum source speed across the active rate transition. A faster
    /// consumer gets an earlier service time; callers validate playback rates.
    pub fn rate(&self, rate: f64) {
        assert!(rate.is_finite() && rate > 0.);
        let mut value = self.0.value.lock().unwrap();
        if value.rate != rate {
            value.rate = rate;
            value.deadline = (!value.paused).then(std::time::Instant::now);
        }
    }
    /// Read the current absolute deadline, including updates after admission.
    pub fn get(&self) -> Option<std::time::Instant> {
        self.0.value.lock().unwrap().deadline
    }
}

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
    pub resource: AssetSource,
    pub priority: Priority,
    pub scope: Scope,
    pub completion: BudgetTicket,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetInfo {
    pub length: Option<u64>,
    pub range_read: bool,
    pub seekable: bool,
    pub version: Option<ContentVersion>,
    pub media_type: Option<String>,
}

#[derive(Debug)]
pub struct ReadRequest {
    /// Soft monotonic service deadline from the consumer's decoded buffer.
    pub deadline: Option<ReadDeadline>,
    pub resource: AssetSource,
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
    /// Creates an independently polled reader on this adapter's existing bounded
    /// IO lane. Submission, cancellation and completion delivery must progress
    /// without polling the original adapter. All readers share its queue limits
    /// and request identity domain; dropping the original adapter closes them.
    /// `wake` signals completions and newly available admission capacity. The
    /// reader must acknowledge it before polling and recheck it before sleeping.
    /// Unsupported adapters reject media streaming at reader creation.
    fn background_reader(
        &self,
        _wake: WakeSignal,
    ) -> Result<Box<dyn AssetIo + Send>, RejectReason> {
        Err(RejectReason::new(
            RejectCode::Unsupported,
            "asset adapter does not provide a background reader",
        ))
    }
    fn capabilities(&self) -> IoCaps;
    fn describe(&mut self, request: DescribeRequest) -> Result<IoId, Rejected<DescribeRequest>>;
    fn read(&mut self, request: ReadRequest) -> Result<IoId, Rejected<ReadRequest>>;
    fn replace_save(&mut self, request: SaveWrite) -> Result<IoId, Rejected<SaveWrite>>;
    /// If unsettled, settle Cancelled once and suppress later business output.
    /// If already settled/queued, preserve that terminal; cancellation is a no-op.
    /// A caller must first revoke its business wait, even if the IO cannot abort.
    fn cancel(&mut self, id: IoId);
    /// With a nonzero limit, ready read/save lanes must make progress under
    /// continuing traffic in the other lane. Append at most `limit` total events.
    fn poll(&mut self, out: &mut Vec<IoEvent>, limit: usize);
}
