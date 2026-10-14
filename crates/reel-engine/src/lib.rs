//! The Rust session host, design baseline 1.1, host interface 0.1.
//!
//! This owns ADV dialogue, copied choice rows, input permission and one terminal
//! record. The separate [`aside`] adapter binds this model to actual VM tickets
//! and rooted waits, covered by real compiler/VM execution checks.
//! `CleanupOnly` never grants resume permission. A claimed `Resume` record must
//! still be checked against session/epoch, owner liveness and the actual wait.

use std::{collections::HashSet, ops::Range};

pub mod aside;
pub mod audio;
pub mod localization;
mod reading;
pub use reading::{ReadProgress, ReadingState};

pub use reel_core::identity::{Epoch, OwnerId, Scope, SessionId};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SessionIdentity {
    pub session_id: SessionId,
    pub epoch: Epoch,
}

/// A scoped owner allocated by this host. IDs are never reused in an epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OwnerHandle {
    identity: SessionIdentity,
    id: OwnerId,
}

impl OwnerHandle {
    pub fn owner_id(self) -> OwnerId {
        self.id
    }

    pub fn scope(self) -> Scope {
        Scope::Session {
            session_id: self.identity.session_id,
            epoch: self.identity.epoch,
            owner_id: self.id,
        }
    }
}

/// Opaque identity: a new generation for each successfully submitted interaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InteractionId {
    identity: SessionIdentity,
    generation: u64,
}

impl InteractionId {
    pub fn session_id(self) -> SessionId {
        self.identity.session_id
    }

    pub fn epoch(self) -> Epoch {
        self.identity.epoch
    }

    pub fn generation(self) -> u64 {
        self.generation
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DialogueLineId {
    interaction_id: InteractionId,
    index: usize,
}

impl DialogueLineId {
    pub fn interaction_id(self) -> InteractionId {
        self.interaction_id
    }

    pub fn index(self) -> usize {
        self.index
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HistoryGroupId(InteractionId);

/// A choice row's stable identity combines its interaction with its business key.
/// Only submitted rows create these IDs; they cannot be forged from an index.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ChoiceId {
    interaction_id: InteractionId,
    key: String,
}

impl ChoiceId {
    pub fn interaction_id(&self) -> InteractionId {
        self.interaction_id
    }

    pub fn key(&self) -> &str {
        &self.key
    }
}

/// Borrowed text; submission retains an immutable snapshot before committing.
#[derive(Clone, Copy, Debug)]
pub struct DialogueLineInput<'a> {
    pub speaker_id: Option<&'a str>,
    pub speaker_name: &'a str,
    pub content_id: &'a str,
    pub text: TextInput<'a>,
}

#[derive(Clone, Copy, Debug)]
pub enum TextInput<'a> {
    Plain(&'a str),
    Styled(&'a reel_core::text::Text),
}
impl<'a> From<&'a str> for TextInput<'a> {
    fn from(text: &'a str) -> Self {
        Self::Plain(text)
    }
}
impl<'a> From<&'a String> for TextInput<'a> {
    fn from(text: &'a String) -> Self {
        Self::Plain(text)
    }
}
impl<'a> From<&'a reel_core::text::Text> for TextInput<'a> {
    fn from(text: &'a reel_core::text::Text) -> Self {
        Self::Styled(text)
    }
}
impl std::ops::Deref for TextInput<'_> {
    type Target = str;
    fn deref(&self) -> &str {
        match self {
            Self::Plain(text) => text,
            Self::Styled(text) => text,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ChoiceInput<'a> {
    pub key: &'a str,
    pub label: &'a str,
}

/// Submitted text preserves its content identity, styles and resolved locale snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct TextContent {
    id: String,
    fallback: reel_core::text::Text,
}

impl TextContent {
    pub fn styled(&self) -> &reel_core::text::Text {
        &self.fallback
    }
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn fallback(&self) -> &str {
        &self.fallback
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reveal {
    pub revealed_count: usize,
    pub total_count: usize,
    /// UTF-8 range of the visible prefix in the fixed full text.
    pub visible_bytes: Range<usize>,
}

#[derive(Debug, PartialEq)]
pub struct DialogueLine {
    id: DialogueLineId,
    previously_read: bool,
    speaker_id: Option<String>,
    speaker_name: String,
    content: TextContent,
    /// Every grapheme endpoint, including the initial zero boundary.
    grapheme_boundaries: Vec<usize>,
    revealed_count: usize,
}

impl DialogueLine {
    /// Whether this content was completed before this group was submitted.
    pub fn previously_read(&self) -> bool {
        self.previously_read
    }

    pub fn id(&self) -> DialogueLineId {
        self.id
    }

    pub fn speaker_id(&self) -> Option<&str> {
        self.speaker_id.as_deref()
    }

    pub fn speaker_name(&self) -> &str {
        &self.speaker_name
    }

    pub fn content(&self) -> &TextContent {
        &self.content
    }

    pub fn content_id(&self) -> &str {
        self.content.id()
    }

    pub fn fallback_text(&self) -> &str {
        self.content.fallback()
    }

    pub fn resolved_text(&self) -> &str {
        // This version accepts only plain text, already resolved by the caller.
        self.content.fallback()
    }

    pub fn reveal(&self) -> Reveal {
        Reveal {
            revealed_count: self.revealed_count,
            total_count: self.grapheme_boundaries.len() - 1,
            visible_bytes: 0..self.grapheme_boundaries[self.revealed_count],
        }
    }

    pub fn grapheme_boundaries(&self) -> &[usize] {
        &self.grapheme_boundaries
    }

    pub fn visible_text(&self) -> &str {
        &self.resolved_text()[self.reveal().visible_bytes]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogueMode {
    Adv,
}

#[derive(Debug, PartialEq)]
pub struct DialogueInteraction {
    previously_read: bool,
    id: InteractionId,
    mode: DialogueMode,
    current_line_ids: Vec<DialogueLineId>,
    lines: Vec<DialogueLine>,
    page_locale: String,
}

impl DialogueInteraction {
    /// All content IDs were completed before this group was submitted.
    pub fn previously_read(&self) -> bool {
        self.previously_read
    }

    pub fn id(&self) -> InteractionId {
        self.id
    }

    pub fn mode(&self) -> DialogueMode {
        self.mode
    }

    pub fn current_line_ids(&self) -> &[DialogueLineId] {
        &self.current_line_ids
    }

    pub fn lines(&self) -> &[DialogueLine] {
        &self.lines
    }

    pub fn page_locale(&self) -> &str {
        &self.page_locale
    }

    pub fn speaker_name(&self) -> &str {
        self.lines.first().map_or("", DialogueLine::speaker_name)
    }

    pub fn text(&self) -> &str {
        self.lines.first().map_or("", DialogueLine::resolved_text)
    }

    pub fn reveal(&self) -> Reveal {
        self.lines.first().map_or(
            Reveal {
                revealed_count: 0,
                total_count: 0,
                visible_bytes: 0..0,
            },
            DialogueLine::reveal,
        )
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct ChoiceOption {
    id: ChoiceId,
    label: String,
}

impl ChoiceOption {
    pub fn id(&self) -> &ChoiceId {
        &self.id
    }

    pub fn key(&self) -> &str {
        self.id.key()
    }

    pub fn label(&self) -> &str {
        &self.label
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct ChoiceInteraction {
    id: InteractionId,
    options: Vec<ChoiceOption>,
}

impl ChoiceInteraction {
    pub fn id(&self) -> InteractionId {
        self.id
    }

    pub fn options(&self) -> &[ChoiceOption] {
        &self.options
    }
}

/// History owns its submitted content independently of reveal and view lifetime.
#[derive(Debug, PartialEq)]
pub struct HistoryLine {
    id: DialogueLineId,
    speaker_id: Option<String>,
    speaker_name: String,
    content: TextContent,
}

impl HistoryLine {
    pub fn id(&self) -> DialogueLineId {
        self.id
    }

    pub fn speaker_id(&self) -> Option<&str> {
        self.speaker_id.as_deref()
    }

    pub fn speaker_name(&self) -> &str {
        &self.speaker_name
    }

    pub fn content(&self) -> &TextContent {
        &self.content
    }

    pub fn resolved_text(&self) -> &str {
        self.content.fallback()
    }
}

#[derive(Debug, PartialEq)]
pub struct HistoryGroup {
    group_id: HistoryGroupId,
    interaction_id: InteractionId,
    lines: Vec<HistoryLine>,
    page_locale: String,
}

impl HistoryGroup {
    pub fn group_id(&self) -> HistoryGroupId {
        self.group_id
    }

    pub fn interaction_id(&self) -> InteractionId {
        self.interaction_id
    }

    pub fn lines(&self) -> &[HistoryLine] {
        &self.lines
    }

    pub fn page_locale(&self) -> &str {
        &self.page_locale
    }

    pub fn speaker_name(&self) -> &str {
        self.lines.first().map_or("", HistoryLine::speaker_name)
    }

    pub fn text(&self) -> &str {
        self.lines.first().map_or("", HistoryLine::resolved_text)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStatus {
    Running,
    Ended,
    Cancelled,
}

#[derive(Clone, Copy, Debug)]
pub enum InteractionView<'a> {
    Dialogue(&'a DialogueInteraction),
    Choice(&'a ChoiceInteraction),
}

impl InteractionView<'_> {
    pub fn id(self) -> InteractionId {
        match self {
            Self::Dialogue(dialogue) => dialogue.id(),
            Self::Choice(choice) => choice.id(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SessionView<'a> {
    pub identity: SessionIdentity,
    pub status: SessionStatus,
    pub interaction: Option<InteractionView<'a>>,
    pub history: &'a [HistoryGroup],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct InputSequence(pub u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputAction {
    Advance(InteractionId),
    Select(ChoiceId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputEvent {
    pub source_owner: OwnerHandle,
    pub sequence: InputSequence,
    pub action: InputAction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputOutcome {
    Revealed,
    Completed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputRejection {
    ReadingPaused,
    SessionNotRunning,
    ForeignOwner,
    OwnerNotLive,
    DuplicateOrOutOfOrder,
    SequenceExhausted,
    NoActiveInteraction,
    StaleInteraction,
    WrongInteractionKind,
    UnknownChoice,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancellationReason {
    OwnerRevoked,
    SessionCancelled,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CompletionOutcome {
    Advanced,
    Selected(String),
    Cancelled(CancellationReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionDelivery {
    /// The root owner was live at claim. Revalidate it and the real wait at delivery.
    Resume,
    /// Terminal cleanup evidence only. No task or owner may be resumed.
    CleanupOnly,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Completion {
    interaction_id: InteractionId,
    owner: OwnerHandle,
    outcome: CompletionOutcome,
    delivery: CompletionDelivery,
}

impl Completion {
    pub fn interaction_id(&self) -> InteractionId {
        self.interaction_id
    }

    pub fn owner(&self) -> OwnerHandle {
        self.owner
    }

    pub fn outcome(&self) -> &CompletionOutcome {
        &self.outcome
    }

    pub fn delivery(&self) -> CompletionDelivery {
        self.delivery
    }

    /// Move a selected business key to a future adapter without copying it.
    pub fn into_outcome(self) -> CompletionOutcome {
        self.outcome
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionError {
    SessionNotRunning,
    ForeignOwner,
    OwnerNotLive,
    MainOwnerRequired,
    InteractionActive,
    CompletionPending,
    NoActiveInteraction,
    StaleInteraction,
    WrongInteractionKind,
    EmptyLocale,
    EmptyContentId,
    EmptyChoiceKey,
    EmptyChoices,
    DuplicateChoiceKey,
    AllocationFailed,
    IdentityExhausted,
    OwnerUnknown,
    OwnerNotRevoked,
    PeerOwnerRequirementsMissing,
    RetirementCursorRegression,
    RetirementCursorOutOfRange,
    RetirementBudgetExceeded,
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "session host: {self:?}")
    }
}

impl std::error::Error for SessionError {}

impl std::fmt::Display for InputRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "session input: {self:?}")
    }
}

impl std::error::Error for InputRejection {}

/// Conservative backing-storage estimate, not OS resident memory or allocator
/// overhead. Includes dialogue, history and the pending selected-key buffer;
/// shared text is counted per retained reference. Excludes Arc headers, the
/// in-struct `Session` and a completion already moved to its caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StorageUsage {
    pub allocated_capacity_bytes: usize,
    pub reserved_owner_slots: usize,
    pub reserved_history_slots: usize,
    pub active_grapheme_boundary_slots: usize,
    pub retained_history_text_bytes: usize,
    /// One retirement record is reserved inside every owner storage slot.
    pub reserved_retirement_records: usize,
    pub pending_owner_retirements: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerLifecycle {
    Live,
    Revoked,
    Retired,
}

/// Obligations declared by a future real cleanup coordinator before revocation.
/// `root_slots_end` freezes the single-root slot upper bound, not a root count
/// or a claim that roots have been released. A scanned empty slot still advances
/// the cursor and consumes work. Host-owned non-task root ranges are a separate
/// obligation: the real coordinator must use its own bounded range-release
/// cursor and keep `external_resources_pending` set until those ranges and all
/// other external resources are released. This host does not allocate VM roots.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OwnerRetirementRequirements {
    pub root_slots_end: usize,
    pub tasks_pending: bool,
    pub external_resources_pending: bool,
}

/// Rust lifecycle bookkeeping only. These fields do not prove VM task stopping,
/// GC, root release or device/resource cleanup; the real adapter reports them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnerRetirementView {
    pub root_cursor: usize,
    pub root_slots_end: usize,
    pub tasks_pending: bool,
    pub external_resources_pending: bool,
}

#[derive(Debug)]
struct OwnerEntry {
    handle: OwnerHandle,
    lifecycle: OwnerLifecycle,
    retirement: Option<OwnerRetirementView>,
}

impl OwnerEntry {
    fn revoke(&mut self, requirements: OwnerRetirementRequirements) {
        debug_assert_eq!(self.lifecycle, OwnerLifecycle::Live);
        self.lifecycle = OwnerLifecycle::Revoked;
        self.retirement = Some(OwnerRetirementView {
            root_cursor: 0,
            root_slots_end: requirements.root_slots_end,
            tasks_pending: requirements.tasks_pending,
            external_resources_pending: requirements.external_resources_pending,
        });
        self.retire_if_complete();
    }

    fn retire_if_complete(&mut self) {
        if self.retirement.is_some_and(|record| {
            record.root_cursor == record.root_slots_end
                && !record.tasks_pending
                && !record.external_resources_pending
        }) {
            debug_assert_ne!(self.lifecycle, OwnerLifecycle::Live);
            self.lifecycle = OwnerLifecycle::Retired;
        }
    }
}

#[derive(Debug)]
enum Interaction {
    Dialogue(DialogueInteraction),
    Choice(ChoiceInteraction),
}

impl Interaction {
    fn id(&self) -> InteractionId {
        match self {
            Self::Dialogue(dialogue) => dialogue.id,
            Self::Choice(choice) => choice.id,
        }
    }
}

/// One host owns exactly one main interaction, plus a reserved terminal slot.
///
/// Owner and history storage grows with its contents. An interaction
/// prepares all copied data and grapheme boundaries before publication. Its
/// completion is moved into a single in-struct slot without allocation; the
/// next interaction cannot start until that slot is claimed.
#[derive(Debug)]
pub struct Session {
    identity: SessionIdentity,
    status: SessionStatus,
    owners: Vec<OwnerEntry>,
    next_owner: Option<u64>,
    next_interaction: Option<u64>,
    last_input_sequence: Option<InputSequence>,
    active: Option<Interaction>,
    completion: Option<Completion>,
    history: Vec<HistoryGroup>,
    reading: ReadingState,
    read_progress: ReadProgress,
}

impl Session {
    /// Construct a new identity namespace. The runtime must assign a fresh
    /// `(SessionId, Epoch)` pair when replacing or restarting a session; reusing
    /// that pair would deliberately make the old scoped identities equivalent.
    pub fn new(session_id: SessionId, epoch: Epoch) -> Result<Self, SessionError> {
        Self::new_with_read_progress(session_id, epoch, ReadProgress::default())
    }

    pub fn new_with_read_progress(
        session_id: SessionId,
        epoch: Epoch,
        read_progress: ReadProgress,
    ) -> Result<Self, SessionError> {
        let identity = SessionIdentity { session_id, epoch };
        let root_owner = OwnerHandle {
            identity,
            id: OwnerId(1),
        };
        let mut owners = reserved_vec(1)?;
        owners.push(OwnerEntry {
            handle: root_owner,
            lifecycle: OwnerLifecycle::Live,
            retirement: None,
        });
        Ok(Self {
            identity,
            status: SessionStatus::Running,
            owners,
            next_owner: Some(2),
            next_interaction: Some(1),
            last_input_sequence: None,
            active: None,
            completion: None,
            history: Vec::new(),
            reading: ReadingState::default(),
            read_progress,
        })
    }

    pub fn identity(&self) -> SessionIdentity {
        self.identity
    }

    pub fn status(&self) -> SessionStatus {
        self.status
    }

    pub fn read_progress(&self) -> &ReadProgress {
        &self.read_progress
    }

    pub fn set_skip_reading(&mut self, enabled: bool, unread: bool) {
        self.reading.set_skip(enabled, unread);
        self.check_skip_boundary();
    }

    fn check_skip_boundary(&mut self) {
        if matches!(&self.active, Some(Interaction::Choice(_)))
            || matches!(&self.active, Some(Interaction::Dialogue(d)) if !d.previously_read && !self.reading.skip_unread())
        {
            self.reading.set_skip(false, false);
        }
    }

    pub fn reading(&self) -> ReadingState {
        self.reading
    }

    pub fn set_auto_reading(&mut self, enabled: bool, delay: Option<std::time::Duration>) {
        self.reading.set_auto(enabled, delay);
    }

    pub fn pause_reading(&mut self, paused: bool) {
        self.reading.pause(paused);
    }

    /// Inspect allocated backing capacity outside the input/render hot path.
    pub fn storage_usage(&self) -> StorageUsage {
        let mut bytes = vector_capacity_bytes(&self.owners)
            + vector_capacity_bytes(&self.history)
            + self.read_progress.storage_bytes();
        let mut history_text_bytes = 0;
        for history in &self.history {
            bytes += vector_capacity_bytes(&history.lines) + history.page_locale.capacity();
            history_text_bytes += history.page_locale.len();
            for line in &history.lines {
                bytes += line.speaker_id.as_ref().map_or(0, String::capacity)
                    + line.speaker_name.capacity()
                    + line.content.id.capacity()
                    + line.content.fallback.storage_bytes();
                history_text_bytes += line.speaker_id.as_ref().map_or(0, String::len)
                    + line.speaker_name.len()
                    + line.content.id.len()
                    + line.content.fallback.len();
            }
        }
        let mut active_grapheme_boundary_slots = 0;
        match &self.active {
            Some(Interaction::Dialogue(dialogue)) => {
                bytes += vector_capacity_bytes(&dialogue.lines)
                    + vector_capacity_bytes(&dialogue.current_line_ids)
                    + dialogue.page_locale.capacity();
                for line in &dialogue.lines {
                    bytes += line.speaker_id.as_ref().map_or(0, String::capacity)
                        + line.speaker_name.capacity()
                        + line.content.id.capacity()
                        + line.content.fallback.storage_bytes()
                        + vector_capacity_bytes(&line.grapheme_boundaries);
                    active_grapheme_boundary_slots += line.grapheme_boundaries.capacity();
                }
            }
            Some(Interaction::Choice(choice)) => {
                bytes += vector_capacity_bytes(&choice.options);
                for option in &choice.options {
                    bytes += option.id.key.capacity() + option.label.capacity();
                }
            }
            None => {}
        }
        if let Some(Completion {
            outcome: CompletionOutcome::Selected(key),
            ..
        }) = &self.completion
        {
            bytes += key.capacity();
        }
        StorageUsage {
            allocated_capacity_bytes: bytes,
            reserved_owner_slots: self.owners.capacity(),
            reserved_history_slots: self.history.capacity(),
            active_grapheme_boundary_slots,
            retained_history_text_bytes: history_text_bytes,
            reserved_retirement_records: self.owners.capacity(),
            pending_owner_retirements: self
                .owners
                .iter()
                .filter(|entry| entry.lifecycle == OwnerLifecycle::Revoked)
                .count(),
        }
    }

    pub fn root_owner(&self) -> OwnerHandle {
        self.owners[0].handle
    }

    pub fn is_owner_live(&self, owner: OwnerHandle) -> bool {
        self.status == SessionStatus::Running
            && owner.identity == self.identity
            && self
                .owners
                .iter()
                .any(|entry| entry.handle == owner && entry.lifecycle == OwnerLifecycle::Live)
    }

    /// Retired IDs remain observable until their storage slot is reused.
    pub fn owner_lifecycle(&self, owner: OwnerHandle) -> Option<OwnerLifecycle> {
        self.owners
            .iter()
            .find(|entry| entry.handle == owner)
            .map(|entry| entry.lifecycle)
    }

    /// A live owner has no retirement record; revoked and retired records are
    /// kept until actual retirement permits a later storage-slot reuse.
    pub fn owner_retirement(&self, owner: OwnerHandle) -> Option<OwnerRetirementView> {
        self.owners
            .iter()
            .find(|entry| entry.handle == owner)
            .and_then(|entry| entry.retirement)
    }

    /// Register a UI node owner. Retired storage slots may be reused, IDs never are.
    pub fn create_owner(&mut self) -> Result<OwnerHandle, SessionError> {
        self.ensure_running()?;
        let retired_index = self
            .owners
            .iter()
            .position(|entry| entry.lifecycle == OwnerLifecycle::Retired);
        let id = self.next_owner.ok_or(SessionError::IdentityExhausted)?;
        if retired_index.is_none() {
            self.owners
                .try_reserve(1)
                .map_err(|_| SessionError::AllocationFailed)?;
        }
        let handle = OwnerHandle {
            identity: self.identity,
            id: OwnerId(id),
        };
        let entry = OwnerEntry {
            handle,
            lifecycle: OwnerLifecycle::Live,
            retirement: None,
        };
        if let Some(index) = retired_index {
            self.owners[index] = entry;
        } else {
            self.owners.push(entry);
        }
        self.next_owner = id.checked_add(1);
        Ok(handle)
    }

    /// Pure-Rust zero-obligation shortcut: the caller guarantees that the owner
    /// has no executor tasks, host roots or external resources. A root shortcut
    /// makes the same assertion for every still-live peer. It never relaxes or
    /// overwrites an already-revoked owner's declared cleanup obligations.
    /// A dead main owner produces cleanup, never a wakeup.
    pub fn revoke_owner(&mut self, owner: OwnerHandle) -> Result<bool, SessionError> {
        self.revoke_owner_internal(owner, OwnerRetirementRequirements::default(), false)
    }

    /// Reserve the in-slot retirement record, revoke permissions, then wait for
    /// all declared cleanup categories. Repeated revoke cannot replace a record.
    /// Before revoking the root, a real coordinator must declare and revoke all
    /// live peers; otherwise this returns a conflict before any mutation.
    pub fn revoke_owner_with_retirement(
        &mut self,
        owner: OwnerHandle,
        requirements: OwnerRetirementRequirements,
    ) -> Result<bool, SessionError> {
        self.revoke_owner_internal(owner, requirements, true)
    }

    fn revoke_owner_internal(
        &mut self,
        owner: OwnerHandle,
        requirements: OwnerRetirementRequirements,
        require_peer_declarations: bool,
    ) -> Result<bool, SessionError> {
        if owner.identity != self.identity {
            return Err(SessionError::ForeignOwner);
        }
        let Some(index) = self.owners.iter().position(|entry| entry.handle == owner) else {
            return Ok(false);
        };
        if self.owners[index].lifecycle != OwnerLifecycle::Live {
            return Ok(false);
        }
        let root = owner == self.root_owner();
        if root
            && require_peer_declarations
            && self
                .owners
                .iter()
                .any(|entry| entry.handle != owner && entry.lifecycle == OwnerLifecycle::Live)
        {
            return Err(SessionError::PeerOwnerRequirementsMissing);
        }
        self.owners[index].revoke(requirements);
        if root {
            self.status = SessionStatus::Cancelled;
            if !require_peer_declarations {
                self.retire_live_owners_without_obligations();
            }
            self.cancel_interaction(CancellationReason::OwnerRevoked);
        }
        Ok(true)
    }

    /// Report an actual adapter's cursor, including every traversed empty slot.
    /// Its forward delta must fit this step's root work budget and the declared
    /// single-root snapshot bound. The adapter limits its real scan to remaining
    /// work and `root_slots_end - root_cursor`; this is not an extra heap API
    /// parameter. This bookkeeping performs no root, range-release or GC operation.
    pub fn report_owner_root_progress(
        &mut self,
        owner: OwnerHandle,
        new_cursor: usize,
        work_budget: usize,
    ) -> Result<OwnerLifecycle, SessionError> {
        let entry = self.retirement_entry_mut(owner)?;
        let record = entry
            .retirement
            .as_mut()
            .expect("revoked owner has reserved record");
        if new_cursor < record.root_cursor {
            return Err(SessionError::RetirementCursorRegression);
        }
        if new_cursor > record.root_slots_end {
            return Err(SessionError::RetirementCursorOutOfRange);
        }
        if new_cursor - record.root_cursor > work_budget {
            return Err(SessionError::RetirementBudgetExceeded);
        }
        record.root_cursor = new_cursor;
        entry.retire_if_complete();
        Ok(entry.lifecycle)
    }

    /// Confirm actual task cleanup, not just a cancel request or stop return.
    /// A coordinator may call this only after all task slices and their owned
    /// cleanup obligations are released. This never recreates a live owner.
    pub fn confirm_owner_task_cleanup(
        &mut self,
        owner: OwnerHandle,
    ) -> Result<OwnerLifecycle, SessionError> {
        let entry = self.retirement_entry_mut(owner)?;
        entry
            .retirement
            .as_mut()
            .expect("revoked owner has reserved record")
            .tasks_pending = false;
        entry.retire_if_complete();
        Ok(entry.lifecycle)
    }

    /// Confirm all actual external cleanup, including independently tracked
    /// host-owned non-task root ranges. This only clears the declared category;
    /// it performs no IO, range release, device retirement or heap reclamation.
    pub fn confirm_owner_resources_released(
        &mut self,
        owner: OwnerHandle,
    ) -> Result<OwnerLifecycle, SessionError> {
        let entry = self.retirement_entry_mut(owner)?;
        entry
            .retirement
            .as_mut()
            .expect("revoked owner has reserved record")
            .external_resources_pending = false;
        entry.retire_if_complete();
        Ok(entry.lifecycle)
    }

    fn retirement_entry_mut(
        &mut self,
        owner: OwnerHandle,
    ) -> Result<&mut OwnerEntry, SessionError> {
        if owner.identity != self.identity {
            return Err(SessionError::ForeignOwner);
        }
        let entry = self
            .owners
            .iter_mut()
            .find(|entry| entry.handle == owner)
            .ok_or(SessionError::OwnerUnknown)?;
        if entry.lifecycle == OwnerLifecycle::Live {
            return Err(SessionError::OwnerNotRevoked);
        }
        Ok(entry)
    }

    fn retire_live_owners_without_obligations(&mut self) {
        for entry in &mut self.owners {
            if entry.lifecycle == OwnerLifecycle::Live {
                entry.revoke(OwnerRetirementRequirements::default());
            }
        }
    }

    pub fn view(&self) -> SessionView<'_> {
        SessionView {
            identity: self.identity,
            status: self.status,
            interaction: self.active.as_ref().map(|active| match active {
                Interaction::Dialogue(dialogue) => InteractionView::Dialogue(dialogue),
                Interaction::Choice(choice) => InteractionView::Choice(choice),
            }),
            history: &self.history,
        }
    }

    /// Submit one ADV group and append history once, in the same transaction.
    /// The locale tag and full plain text remain fixed for this interaction.
    pub fn begin_say(
        &mut self,
        owner: OwnerHandle,
        lines: &[DialogueLineInput<'_>],
        page_locale: &str,
    ) -> Result<InteractionId, SessionError> {
        let id = self.prepare_interaction(owner)?;
        if page_locale.is_empty() {
            return Err(SessionError::EmptyLocale);
        }
        for line in lines {
            if line.content_id.is_empty() {
                return Err(SessionError::EmptyContentId);
            }
        }

        let mut owned_lines = reserved_vec(lines.len())?;
        let mut current_line_ids = reserved_vec(lines.len())?;
        let mut history_lines = reserved_vec(lines.len())?;
        for (index, line) in lines.iter().enumerate() {
            let line_id = DialogueLineId {
                interaction_id: id,
                index,
            };
            let total_count = line.text.graphemes(true).count();
            let mut boundaries = reserved_vec(
                total_count
                    .checked_add(1)
                    .ok_or(SessionError::AllocationFailed)?,
            )?;
            boundaries.push(0);
            boundaries.extend(
                line.text
                    .grapheme_indices(true)
                    .map(|(offset, grapheme)| offset + grapheme.len()),
            );
            let content = copy_content(line)?;
            owned_lines.push(DialogueLine {
                id: line_id,
                previously_read: self.read_progress.contains(line.content_id),
                speaker_id: copy_optional_text(line.speaker_id)?,
                speaker_name: copy_text(line.speaker_name)?,
                content: content.clone(),
                grapheme_boundaries: boundaries,
                revealed_count: 0,
            });
            current_line_ids.push(line_id);
            history_lines.push(HistoryLine {
                id: line_id,
                speaker_id: copy_optional_text(line.speaker_id)?,
                speaker_name: copy_text(line.speaker_name)?,
                content,
            });
        }
        self.read_progress
            .ids
            .try_reserve(lines.len())
            .map_err(|_| SessionError::AllocationFailed)?;
        let previously_read = owned_lines.iter().all(DialogueLine::previously_read);
        let dialogue = DialogueInteraction {
            previously_read,
            id,
            mode: DialogueMode::Adv,
            current_line_ids,
            lines: owned_lines,
            page_locale: copy_text(page_locale)?,
        };
        let history = HistoryGroup {
            group_id: HistoryGroupId(id),
            interaction_id: id,
            lines: history_lines,
            page_locale: copy_text(page_locale)?,
        };
        self.history
            .try_reserve(1)
            .map_err(|_| SessionError::AllocationFailed)?;
        // Everything fallible is complete. Both publications use reserved storage.
        self.active = Some(Interaction::Dialogue(dialogue));
        self.check_skip_boundary();
        self.history.push(history);
        self.next_interaction = id.generation.checked_add(1);
        Ok(id)
    }

    /// Copy option rows, preserving order and returning their original business keys.
    pub fn begin_choice(
        &mut self,
        owner: OwnerHandle,
        options: &[ChoiceInput<'_>],
    ) -> Result<InteractionId, SessionError> {
        let id = self.prepare_interaction(owner)?;
        if options.is_empty() {
            return Err(SessionError::EmptyChoices);
        }
        let mut keys = HashSet::new();
        keys.try_reserve(options.len())
            .map_err(|_| SessionError::AllocationFailed)?;
        for option in options {
            if option.key.is_empty() {
                return Err(SessionError::EmptyChoiceKey);
            }
            if !keys.insert(option.key) {
                return Err(SessionError::DuplicateChoiceKey);
            }
        }
        let mut copied = reserved_vec(options.len())?;
        for option in options {
            copied.push(ChoiceOption {
                id: ChoiceId {
                    interaction_id: id,
                    key: copy_text(option.key)?,
                },
                label: copy_text(option.label)?,
            });
        }
        self.active = Some(Interaction::Choice(ChoiceInteraction {
            id,
            options: copied,
        }));
        self.check_skip_boundary();
        self.next_interaction = id.generation.checked_add(1);
        Ok(id)
    }

    /// Reveal a bounded number of logical graphemes on every current ADV line.
    /// A timer callback must retain this interaction ID, so an old timer cannot
    /// reveal a successor. Zero units and an already-full group are no-ops.
    pub fn reveal_more(
        &mut self,
        owner: OwnerHandle,
        id: InteractionId,
        graphemes_per_line: usize,
    ) -> Result<bool, SessionError> {
        self.ensure_running()?;
        if owner.identity != self.identity {
            return Err(SessionError::ForeignOwner);
        }
        if !self.is_owner_live(owner) {
            return Err(SessionError::OwnerNotLive);
        }
        if owner != self.root_owner() {
            return Err(SessionError::MainOwnerRequired);
        }
        let active = self
            .active
            .as_mut()
            .ok_or(SessionError::NoActiveInteraction)?;
        if id != active.id() {
            return Err(SessionError::StaleInteraction);
        }
        let Interaction::Dialogue(dialogue) = active else {
            return Err(SessionError::WrongInteractionKind);
        };
        if self.reading.paused() {
            return Ok(false);
        }
        let mut changed = false;
        for line in &mut dialogue.lines {
            let total_count = line.grapheme_boundaries.len() - 1;
            let next =
                line.revealed_count + graphemes_per_line.min(total_count - line.revealed_count);
            changed |= next != line.revealed_count;
            line.revealed_count = next;
        }
        Ok(changed)
    }

    /// Process each live same-session sequence at most once, even if its action is
    /// stale or invalid. Foreign/dead sources do not change the high-water mark.
    pub fn handle_input(&mut self, event: InputEvent) -> Result<InputOutcome, InputRejection> {
        self.validate_input_source(event.source_owner)?;
        self.apply_sequenced_input(event)
    }

    /// Dispatch one new local action, assigning its sequence from this Session's
    /// shared watermark under the same exclusive borrow as processing the event.
    /// This follows every prior live-source event, including stale/invalid actions
    /// submitted through `handle_input`; UI attachment or replacement never resets
    /// the sequence. Exhaustion rejects explicitly without wrapping or mutation.
    ///
    /// Use `handle_input` for events that already carry a sequence, retaining that
    /// original sequence on replay. Replaying through this entry would create a
    /// new event instead of checking whether the original was already consumed.
    pub fn handle_local_input(
        &mut self,
        source_owner: OwnerHandle,
        action: InputAction,
    ) -> Result<InputOutcome, InputRejection> {
        self.validate_input_source(source_owner)?;
        let sequence = match self.last_input_sequence {
            Some(last) => InputSequence(
                last.0
                    .checked_add(1)
                    .ok_or(InputRejection::SequenceExhausted)?,
            ),
            None => InputSequence(1),
        };
        self.apply_sequenced_input(InputEvent {
            source_owner,
            sequence,
            action,
        })
    }

    fn validate_input_source(&self, source_owner: OwnerHandle) -> Result<(), InputRejection> {
        if self.status != SessionStatus::Running {
            return Err(InputRejection::SessionNotRunning);
        }
        if source_owner.identity != self.identity {
            return Err(InputRejection::ForeignOwner);
        }
        if !self.is_owner_live(source_owner) {
            return Err(InputRejection::OwnerNotLive);
        }
        Ok(())
    }

    fn apply_sequenced_input(&mut self, event: InputEvent) -> Result<InputOutcome, InputRejection> {
        if self
            .last_input_sequence
            .is_some_and(|last| event.sequence <= last)
        {
            return Err(InputRejection::DuplicateOrOutOfOrder);
        }
        self.last_input_sequence = Some(event.sequence);
        if self.reading.paused() {
            return Err(InputRejection::ReadingPaused);
        }
        let active = self
            .active
            .as_mut()
            .ok_or(InputRejection::NoActiveInteraction)?;
        let target = match &event.action {
            InputAction::Advance(id) => *id,
            InputAction::Select(id) => id.interaction_id,
        };
        if target != active.id() {
            return Err(InputRejection::StaleInteraction);
        }
        match (active, event.action) {
            (Interaction::Dialogue(dialogue), InputAction::Advance(_)) => {
                let mut revealed = false;
                for line in &mut dialogue.lines {
                    let total_count = line.grapheme_boundaries.len() - 1;
                    if line.revealed_count < total_count {
                        line.revealed_count = total_count;
                        revealed = true;
                    }
                }
                if revealed {
                    return Ok(InputOutcome::Revealed);
                }
                self.complete(CompletionOutcome::Advanced);
            }
            (Interaction::Choice(choice), InputAction::Select(id)) => {
                let selected = choice
                    .options
                    .iter_mut()
                    .find(|option| option.id == id)
                    .ok_or(InputRejection::UnknownChoice)?;
                // Move the reserved key; no terminal allocation or duplicate copy.
                let key = std::mem::take(&mut selected.id.key);
                self.complete(CompletionOutcome::Selected(key));
            }
            _ => return Err(InputRejection::WrongInteractionKind),
        }
        Ok(InputOutcome::Completed)
    }

    /// Claim the single terminal record once. This does not execute or resume a task.
    pub fn take_completion(&mut self) -> Option<Completion> {
        self.completion.take()
    }

    /// End a successfully idle pure-Rust session after its result was claimed.
    /// This zero-obligation shortcut asserts all still-live owners have no
    /// external cleanup. Previously declared revoked records remain pending.
    pub fn end(&mut self) -> Result<(), SessionError> {
        self.ensure_running()?;
        if self.active.is_some() {
            return Err(SessionError::InteractionActive);
        }
        if self.completion.is_some() {
            return Err(SessionError::CompletionPending);
        }
        self.status = SessionStatus::Ended;
        self.retire_live_owners_without_obligations();
        Ok(())
    }

    /// Pure-Rust zero-obligation session shortcut. This asserts that still-live
    /// owners have no executor tasks, host roots or external resources; pending
    /// records already declared for revoked owners are never overwritten.
    /// Cancellation is idempotent and preserves already-written history.
    pub fn cancel(&mut self) -> bool {
        if self.status != SessionStatus::Running {
            return false;
        }
        self.status = SessionStatus::Cancelled;
        self.retire_live_owners_without_obligations();
        self.cancel_interaction(CancellationReason::SessionCancelled);
        true
    }

    fn ensure_running(&self) -> Result<(), SessionError> {
        if self.status == SessionStatus::Running {
            Ok(())
        } else {
            Err(SessionError::SessionNotRunning)
        }
    }

    fn prepare_interaction(&self, owner: OwnerHandle) -> Result<InteractionId, SessionError> {
        self.ensure_running()?;
        if owner.identity != self.identity {
            return Err(SessionError::ForeignOwner);
        }
        if !self.is_owner_live(owner) {
            return Err(SessionError::OwnerNotLive);
        }
        if owner != self.root_owner() {
            return Err(SessionError::MainOwnerRequired);
        }
        if self.active.is_some() {
            return Err(SessionError::InteractionActive);
        }
        if self.completion.is_some() {
            return Err(SessionError::CompletionPending);
        }
        Ok(InteractionId {
            identity: self.identity,
            generation: self
                .next_interaction
                .ok_or(SessionError::IdentityExhausted)?,
        })
    }

    fn complete(&mut self, outcome: CompletionOutcome) {
        let interaction = self
            .active
            .take()
            .expect("active interaction reserves completion");
        let interaction_id = interaction.id();
        if matches!(outcome, CompletionOutcome::Advanced)
            && let Interaction::Dialogue(dialogue) = interaction
        {
            // Admission reserved the hash table; move owned IDs without allocating on input.
            for line in dialogue.lines {
                self.read_progress.ids.insert(line.content.id);
            }
        }
        debug_assert!(self.completion.is_none());
        self.completion = Some(Completion {
            interaction_id,
            owner: self.root_owner(),
            outcome,
            delivery: CompletionDelivery::Resume,
        });
    }

    fn cancel_interaction(&mut self, reason: CancellationReason) {
        if let Some(interaction) = self.active.take() {
            debug_assert!(self.completion.is_none());
            self.completion = Some(Completion {
                interaction_id: interaction.id(),
                owner: self.root_owner(),
                outcome: CompletionOutcome::Cancelled(reason),
                delivery: CompletionDelivery::CleanupOnly,
            });
        } else if let Some(completion) = &mut self.completion {
            // A result already committed stays the unique terminal result. Its
            // dead owner makes it cleanup-only, regardless of the original result.
            completion.delivery = CompletionDelivery::CleanupOnly;
        }
    }
}

fn reserved_vec<T>(capacity: usize) -> Result<Vec<T>, SessionError> {
    let mut data = Vec::new();
    data.try_reserve_exact(capacity)
        .map_err(|_| SessionError::AllocationFailed)?;
    Ok(data)
}

fn copy_text(text: &str) -> Result<String, SessionError> {
    let mut copy = String::new();
    copy.try_reserve_exact(text.len())
        .map_err(|_| SessionError::AllocationFailed)?;
    copy.push_str(text);
    Ok(copy)
}

fn copy_optional_text(text: Option<&str>) -> Result<Option<String>, SessionError> {
    text.map(copy_text).transpose()
}

fn copy_content(line: &DialogueLineInput<'_>) -> Result<TextContent, SessionError> {
    Ok(TextContent {
        id: copy_text(line.content_id)?,
        fallback: match line.text {
            TextInput::Plain(text) => text.into(),
            TextInput::Styled(text) => text.clone(),
        },
    })
}

fn vector_capacity_bytes<T>(data: &Vec<T>) -> usize {
    data.capacity() * std::mem::size_of::<T>()
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    #[test]
    fn interaction_generation_exhaustion_never_wraps_or_changes_history() {
        let mut session = Session::new(SessionId(1), Epoch(1)).unwrap();
        let root = session.root_owner();
        session.next_interaction = Some(u64::MAX);
        let id = session.begin_say(root, &[], "en").unwrap();
        assert_eq!(id.generation(), u64::MAX);
        session
            .handle_input(InputEvent {
                source_owner: root,
                sequence: InputSequence(1),
                action: InputAction::Advance(id),
            })
            .unwrap();
        session.take_completion().unwrap();
        let before = session.storage_usage();
        assert_eq!(
            session.begin_say(root, &[], "en"),
            Err(SessionError::IdentityExhausted)
        );
        assert_eq!(session.storage_usage(), before);
        assert_eq!(session.view().history.len(), 1);
        assert!(session.view().interaction.is_none());
    }

    #[test]
    fn owner_generation_exhaustion_does_not_reuse_a_retired_id() {
        let mut session = Session::new(SessionId(1), Epoch(1)).unwrap();
        session.next_owner = Some(u64::MAX);
        let last = session.create_owner().unwrap();
        assert_eq!(last.owner_id(), OwnerId(u64::MAX));
        session.revoke_owner(last).unwrap();
        let before = session.storage_usage();
        assert_eq!(session.create_owner(), Err(SessionError::IdentityExhausted));
        assert_eq!(session.storage_usage(), before);
        assert!(!session.is_owner_live(last));
    }
}
