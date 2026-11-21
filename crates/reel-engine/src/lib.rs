//! Sessions, reading contexts, interactions, history and author activities.
//!
//! [`Session`] owns dialogue admission, choice keys, input permissions and
//! completion records. The [`aside`] adapter binds this state to VM tasks,
//! rooted waits and the shared stage. Completion delivery validates the session,
//! epoch, live owner and actual wait; owner reuse follows completed retirement.

use std::{collections::HashSet, ops::Range};

pub mod aside;
pub mod assets;
pub mod audio;
mod dialogue_stream;
mod input;
pub mod localization;
pub mod media;
mod owners;
mod reading;
mod reveal;
mod snapshot;
pub mod video;
pub use reading::{ReadProgress, ReadingState};

pub use reel_core::identity::{Epoch, OwnerId, Scope, SessionId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct SessionIdentity {
    pub session_id: SessionId,
    pub epoch: Epoch,
}

/// A scoped owner allocated by this host. IDs are never reused in an epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct HistoryGroupId(InteractionId);

/// A choice row's stable identity combines its interaction with its business key.
/// Only submitted rows create these IDs; they cannot be forged from an index.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
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
    pub history: HistoryInput<'a>,
}

/// History content is chosen at submission; display and read identity stay unchanged.
#[derive(Clone, Copy, Debug, Default)]
pub enum HistoryInput<'a> {
    #[default]
    Display,
    Omit,
    Text(TextInput<'a>),
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
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize, Clone)]
pub struct DialogueLine {
    id: DialogueLineId,
    previously_read: bool,
    speaker_id: Option<String>,
    speaker_name: String,
    content: TextContent,
    /// Every grapheme endpoint, including the initial zero boundary.
    grapheme_boundaries: std::sync::Arc<[usize]>,
    revealed_count: usize,
    advance_requested: bool,
    timing: reveal::RevealTiming,
}

impl DialogueLine {
    /// Whether an accepted manual Advance requested completion of this segment.
    pub fn advance_requested(&self) -> bool {
        self.advance_requested
    }

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
        // Styling and timing retain the same immutable, already resolved body.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DialogueMode {
    Adv,
}

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize, Clone)]
pub struct DialogueInteraction {
    previously_read: bool,
    reveal_skippable: bool,
    id: InteractionId,
    mode: DialogueMode,
    current_line_ids: Vec<DialogueLineId>,
    lines: Vec<DialogueLine>,
    page_locale: String,
    open: bool,
    awaiting_advance: bool,
    revision: u64,
    text_revision: u64,
    pending_read_ids: Vec<String>,
}

impl DialogueInteraction {
    pub fn advance_requested(&self) -> bool {
        self.lines.iter().any(DialogueLine::advance_requested)
    }

    pub fn awaiting_advance(&self) -> bool {
        self.awaiting_advance
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    /// Whether advance may complete the group's text and timing immediately.
    pub fn reveal_skippable(&self) -> bool {
        self.reveal_skippable
    }

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

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, Clone)]
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

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, Clone)]
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
#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize, Clone)]
pub struct HistoryLine {
    id: DialogueLineId,
    speaker_id: Option<String>,
    speaker_name: String,
    content: TextContent,
    #[serde(skip)]
    voice: Option<std::sync::Arc<str>>,
}

impl HistoryLine {
    /// Original voice asset, even when fast-forward omitted its playback.
    pub fn voice(&self) -> Option<&str> {
        self.voice.as_deref()
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

    pub fn resolved_text(&self) -> &str {
        self.content.fallback()
    }
}

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize, Clone)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    pub history: &'a reading::HistoryLog,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
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
    /// Consumed while a dialogue's authored reveal timing is still playing.
    Pending,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CancellationReason {
    OwnerRevoked,
    SessionCancelled,
}

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, Clone)]
pub enum CompletionOutcome {
    Advanced,
    Selected(String),
    Cancelled(CancellationReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CompletionDelivery {
    /// The root owner was live at claim. Revalidate it and the real wait at delivery.
    Resume,
    /// Terminal cleanup evidence only. No task or owner may be resumed.
    CleanupOnly,
}

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, Clone)]
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
    SingleDialogueLineRequired,
    InvalidDialogueHandle,
    InvalidTextComposition,
    InvalidTextSpeed,
    InvalidReadingContext,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OwnerRetirementView {
    pub root_cursor: usize,
    pub root_slots_end: usize,
    pub tasks_pending: bool,
    pub external_resources_pending: bool,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
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

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
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

/// One host selects one reading context and its reserved terminal slot.
///
/// Suspended parents retain their original data without copying history.
/// Owner and history storage grows with its contents. An interaction
/// prepares all copied data and grapheme boundaries before publication. Its
/// completion is moved into a single in-struct slot without allocation; the
/// next interaction cannot start until that slot is claimed.
#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
pub struct Session {
    identity: SessionIdentity,
    status: SessionStatus,
    owners: Vec<OwnerEntry>,
    #[serde(skip)]
    owner_revocation_revision: u64,
    next_owner: Option<u64>,
    next_interaction: Option<u64>,
    last_input_sequence: Option<InputSequence>,
    reading_data: reading::ReadingData,
    reading_id: reading::ReadingId,
    next_reading: Option<u64>,
    parked_readings: Vec<reading::ParkedReading>,
    auto_delay: std::time::Duration,
    #[serde(skip)]
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
            owner_revocation_revision: 0,
            next_owner: Some(2),
            next_interaction: Some(1),
            last_input_sequence: None,
            reading_data: reading::ReadingData::default(),
            reading_id: reading::ReadingId::default(),
            next_reading: Some(1),
            parked_readings: Vec::new(),
            auto_delay: std::time::Duration::from_secs(2),
            read_progress,
        })
    }

    pub fn identity(&self) -> SessionIdentity {
        self.identity
    }

    /// Runtime version of owner permission revocation. Restoring a session
    /// starts a new version sequence; lifetime observers must rebuild checks.
    pub fn owner_revocation_revision(&self) -> u64 {
        self.owner_revocation_revision
    }

    pub fn status(&self) -> SessionStatus {
        self.status
    }

    pub fn read_progress(&self) -> &ReadProgress {
        &self.read_progress
    }

    /// Retain at most the final `keep_last` recorded lines, preserving original
    /// group/line IDs even when the boundary cuts a group. Zero clears all.
    /// Display, input, read progress and previously saved snapshots are independent.
    /// This explicit operation synchronously releases discarded storage.
    pub fn clear_history(&mut self, keep_last: usize) {
        if keep_last == 0 {
            self.reading_data.history = reading::HistoryLog::default();
            return;
        }
        let count: usize = self
            .reading_data
            .history
            .iter()
            .map(|group| group.lines.len())
            .sum();
        if keep_last >= count {
            return;
        }
        let mut remaining = keep_last;
        let mut first = self.reading_data.history.len();
        while first != 0 {
            first -= 1;
            let count = self.reading_data.history[first].lines.len();
            if count >= remaining {
                if count > remaining {
                    self.reading_data.history.edit(first, |group| {
                        group.lines.drain(..count - remaining);
                        group.lines.shrink_to_fit();
                    });
                }
                break;
            }
            remaining -= count;
        }
        self.reading_data.history.retain_from(first);
    }

    /// Forget content-read records while preserving the currently displayed text.
    /// Completing the waiting group can still register its IDs without allocating.
    pub fn clear_read_progress(&mut self) -> Result<(), SessionError> {
        let mut progress = ReadProgress::default();
        let contexts =
            std::iter::once(&self.reading_data).chain(self.parked_readings.iter().map(|p| &p.data));
        let reserve = contexts
            .filter_map(|data| match &data.active {
                Some(Interaction::Dialogue(d)) => Some(d.lines.len().max(d.pending_read_ids.len())),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        progress
            .ids
            .try_reserve(reserve)
            .map_err(|_| SessionError::AllocationFailed)?;
        self.read_progress = progress;
        for data in std::iter::once(&mut self.reading_data)
            .chain(self.parked_readings.iter_mut().map(|p| &mut p.data))
        {
            if let Some(Interaction::Dialogue(dialogue)) = &mut data.active {
                dialogue.previously_read = false;
                for line in &mut dialogue.lines {
                    line.previously_read = false;
                }
            }
            if data.control.skipping() && !data.control.skip_unread() {
                data.control.set_skip(false, false, false);
            }
        }
        Ok(())
    }

    pub fn set_skip_reading(&mut self, enabled: bool, unread: bool, after_choices: bool) {
        self.reading_data
            .control
            .set_skip(enabled, unread, after_choices);
        self.check_skip_boundary();
    }

    fn check_skip_boundary(&mut self) {
        if (matches!(&self.reading_data.active, Some(Interaction::Choice(_)))
            && !self.reading_data.control.skip_after_choices())
            || matches!(&self.reading_data.active, Some(Interaction::Dialogue(d)) if !d.previously_read && !self.reading_data.control.skip_unread())
        {
            self.reading_data.control.set_skip(false, false, false);
        }
    }

    pub fn reading(&self) -> ReadingState {
        ReadingState::new(self.reading_data.control, self.auto_delay)
    }

    pub fn set_auto_reading(&mut self, enabled: bool, delay: Option<std::time::Duration>) {
        if let Some(delay) = delay
            && self.auto_delay != delay
        {
            self.auto_delay = delay;
            for data in std::iter::once(&mut self.reading_data)
                .chain(self.parked_readings.iter_mut().map(|p| &mut p.data))
            {
                data.control.changed();
            }
        }
        self.reading_data.control.set_auto(enabled);
    }

    /// Set the resolved author condition without changing reveal or manual input.
    pub fn set_auto_condition(&mut self, ready: bool) {
        self.reading_data.control.set_auto_condition(ready);
    }

    pub fn pause_reading(&mut self, paused: bool) {
        self.reading_data.control.pause(paused);
    }

    /// Inspect allocated backing capacity outside the input/render hot path.
    pub fn storage_usage(&self) -> StorageUsage {
        let mut bytes = vector_capacity_bytes(&self.owners)
            + vector_capacity_bytes(&self.parked_readings)
            + self.read_progress.storage_bytes();
        let mut history_text_bytes = 0;
        let mut active_grapheme_boundary_slots = 0;
        let mut reserved_history_slots = 0;
        for data in
            std::iter::once(&self.reading_data).chain(self.parked_readings.iter().map(|p| &p.data))
        {
            bytes += data.history.retained_bytes();
            reserved_history_slots += data.history.capacity();
            for history in data.history.iter() {
                history_text_bytes += history.page_locale.len();
                for line in &history.lines {
                    history_text_bytes += line.speaker_id.as_ref().map_or(0, String::len)
                        + line.speaker_name.len()
                        + line.content.id.len()
                        + line.content.fallback.len();
                }
            }
            match &data.active {
                Some(Interaction::Dialogue(dialogue)) => {
                    bytes += vector_capacity_bytes(&dialogue.lines)
                        + vector_capacity_bytes(&dialogue.current_line_ids)
                        + vector_capacity_bytes(&dialogue.pending_read_ids)
                        + dialogue
                            .pending_read_ids
                            .iter()
                            .map(String::capacity)
                            .sum::<usize>()
                        + dialogue.page_locale.capacity();
                    for line in &dialogue.lines {
                        bytes += line.speaker_id.as_ref().map_or(0, String::capacity)
                            + line.speaker_name.capacity()
                            + line.content.id.capacity()
                            + line.content.fallback.storage_bytes()
                            + line.grapheme_boundaries.len() * size_of::<usize>();
                        active_grapheme_boundary_slots += line.grapheme_boundaries.len();
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
            }) = &data.completion
            {
                bytes += key.capacity();
            }
        }
        StorageUsage {
            allocated_capacity_bytes: bytes,
            reserved_owner_slots: self.owners.capacity(),
            reserved_history_slots,
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

    pub fn view(&self) -> SessionView<'_> {
        SessionView {
            identity: self.identity,
            status: self.status,
            interaction: self
                .reading_data
                .active
                .as_ref()
                .map(|active| match active {
                    Interaction::Dialogue(dialogue) => InteractionView::Dialogue(dialogue),
                    Interaction::Choice(choice) => InteractionView::Choice(choice),
                }),
            history: &self.reading_data.history,
        }
    }

    /// Submit one ADV group and append history once, in the same transaction.
    /// The locale tag and full plain text remain fixed for this interaction.
    pub fn begin_say(
        &mut self,
        owner: OwnerHandle,
        lines: &[DialogueLineInput<'_>],
        page_locale: &str,
        reveal_skippable: bool,
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
            let boundaries = dialogue_stream::boundaries(&line.text, &[])?;
            let content = copy_content(line)?;
            owned_lines.push(DialogueLine {
                id: line_id,
                previously_read: self.read_progress.contains(line.content_id),
                speaker_id: copy_optional_text(line.speaker_id)?,
                speaker_name: copy_text(line.speaker_name)?,
                content: content.clone(),
                grapheme_boundaries: boundaries.into(),
                revealed_count: 0,
                advance_requested: false,
                timing: Default::default(),
            });
            current_line_ids.push(line_id);
            let history_content = match line.history {
                HistoryInput::Display => Some(content),
                HistoryInput::Omit => None,
                HistoryInput::Text(text) => {
                    Some(copy_content(&DialogueLineInput { text, ..*line })?)
                }
            };
            if let Some(content) = history_content {
                history_lines.push(HistoryLine {
                    id: line_id,
                    speaker_id: copy_optional_text(line.speaker_id)?,
                    speaker_name: copy_text(line.speaker_name)?,
                    content,
                    voice: None,
                });
            }
        }
        self.read_progress
            .ids
            .try_reserve(lines.len())
            .map_err(|_| SessionError::AllocationFailed)?;
        let previously_read = owned_lines.iter().all(DialogueLine::previously_read);
        let dialogue = DialogueInteraction {
            previously_read,
            reveal_skippable,
            id,
            mode: DialogueMode::Adv,
            current_line_ids,
            lines: owned_lines,
            page_locale: copy_text(page_locale)?,
            open: false,
            awaiting_advance: true,
            revision: 0,
            text_revision: 0,
            pending_read_ids: Vec::new(),
        };
        let history = HistoryGroup {
            group_id: HistoryGroupId(id),
            interaction_id: id,
            lines: history_lines,
            page_locale: copy_text(page_locale)?,
        };
        self.reading_data
            .history
            .reserve_append()
            .map_err(|_| SessionError::AllocationFailed)?;
        // Everything fallible is complete. Both publications use reserved storage.
        self.reading_data.active = Some(Interaction::Dialogue(dialogue));
        self.check_skip_boundary();
        // Preserve the Rust empty-group operation, but omit groups whose authored
        // lines explicitly opted out of history.
        if !history.lines.is_empty() || lines.is_empty() {
            self.reading_data.history.push(history);
        }
        self.next_interaction = id.generation.checked_add(1);
        Ok(id)
    }

    /// Append history without changing the active dialogue, choices or read progress.
    pub fn record_history(
        &mut self,
        lines: &[DialogueLineInput<'_>],
        locale: &str,
    ) -> Result<(), SessionError> {
        let group = self.prepare_history_record(lines, locale)?;
        self.commit_history_record(group);
        Ok(())
    }

    fn prepare_history_record(
        &mut self,
        lines: &[DialogueLineInput<'_>],
        locale: &str,
    ) -> Result<HistoryGroup, SessionError> {
        self.ensure_running()?;
        if locale.is_empty() {
            return Err(SessionError::EmptyLocale);
        }
        let id = InteractionId {
            identity: self.identity,
            generation: self
                .next_interaction
                .ok_or(SessionError::IdentityExhausted)?,
        };
        let mut records = reserved_vec(lines.len())?;
        for (index, line) in lines.iter().enumerate() {
            if line.content_id.is_empty() {
                return Err(SessionError::EmptyContentId);
            }
            let text = match line.history {
                HistoryInput::Display => line.text,
                HistoryInput::Omit => continue,
                HistoryInput::Text(text) => text,
            };
            records.push(HistoryLine {
                id: DialogueLineId {
                    interaction_id: id,
                    index,
                },
                speaker_id: copy_optional_text(line.speaker_id)?,
                speaker_name: copy_text(line.speaker_name)?,
                content: copy_content(&DialogueLineInput { text, ..*line })?,
                voice: None,
            });
        }
        let group = HistoryGroup {
            group_id: HistoryGroupId(id),
            interaction_id: id,
            lines: records,
            page_locale: copy_text(locale)?,
        };
        self.reading_data
            .history
            .reserve_append()
            .map_err(|_| SessionError::AllocationFailed)?;
        Ok(group)
    }

    fn commit_history_record(&mut self, group: HistoryGroup) {
        if !group.lines.is_empty() {
            self.next_interaction = group.interaction_id.generation.checked_add(1);
            self.reading_data.history.push(group);
        }
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
        self.reading_data.active = Some(Interaction::Choice(ChoiceInteraction {
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
        let paused = self.reading_data.control.paused();
        let dialogue = self.reveal_dialogue_mut(owner, id)?;
        if paused {
            return Ok(false);
        }
        let mut changed = false;
        for line in &mut dialogue.lines {
            let total_count = line.grapheme_boundaries.len() - 1;
            let next =
                line.revealed_count + graphemes_per_line.min(total_count - line.revealed_count);
            if graphemes_per_line != 0 {
                changed |= next != line.revealed_count || !line.playback_complete();
                line.revealed_count = next;
                line.skip_timing();
            }
        }
        Ok(changed)
    }

    /// Advance each line's own speed and pause cues on the shared logical clock.
    pub fn advance_reveal(
        &mut self,
        owner: OwnerHandle,
        id: InteractionId,
        elapsed: std::time::Duration,
        player_speed: f64,
    ) -> Result<bool, SessionError> {
        if !player_speed.is_finite() || player_speed < 0. {
            return Err(SessionError::InvalidTextSpeed);
        }
        let paused = self.reading_data.control.paused();
        let dialogue = self.reveal_dialogue_mut(owner, id)?;
        if paused {
            return Ok(false);
        }
        let mut changed = false;
        for line in &mut dialogue.lines {
            changed |= line.advance_reveal(elapsed, player_speed, dialogue.reveal_skippable);
        }
        Ok(changed)
    }

    fn reveal_dialogue_mut(
        &mut self,
        owner: OwnerHandle,
        id: InteractionId,
    ) -> Result<&mut DialogueInteraction, SessionError> {
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
            .reading_data
            .active
            .as_mut()
            .ok_or(SessionError::NoActiveInteraction)?;
        if id != active.id() {
            return Err(SessionError::StaleInteraction);
        }
        let Interaction::Dialogue(dialogue) = active else {
            return Err(SessionError::WrongInteractionKind);
        };
        Ok(dialogue)
    }

    /// Whether a terminal interaction record is ready for its waiting task.
    pub fn completion_pending(&self) -> bool {
        self.reading_data.completion.is_some()
    }

    /// Claim the single terminal record once. This does not execute or resume a task.
    pub fn take_completion(&mut self) -> Option<Completion> {
        self.reading_data.completion.take()
    }

    /// End a successfully idle pure-Rust session after its result was claimed.
    /// This zero-obligation shortcut asserts all still-live owners have no
    /// external cleanup. Previously declared revoked records remain pending.
    pub fn end(&mut self) -> Result<(), SessionError> {
        self.ensure_running()?;
        if self.reading_data.active.as_ref().is_some_and(|active| {
            !matches!(active,
            Interaction::Dialogue(d) if d.open && !d.awaiting_advance)
        }) {
            return Err(SessionError::InteractionActive);
        }
        if self.reading_data.completion.is_some() {
            return Err(SessionError::CompletionPending);
        }
        self.status = SessionStatus::Ended;
        self.reading_data.active = None;
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
        if self.reading_data.active.as_ref().is_some_and(|active| {
            !matches!(active,
            Interaction::Dialogue(d) if d.open && !d.awaiting_advance)
        }) {
            return Err(SessionError::InteractionActive);
        }
        if self.reading_data.completion.is_some() {
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
        let mut interaction = self
            .reading_data
            .active
            .take()
            .expect("active interaction reserves completion");
        let interaction_id = interaction.id();
        if matches!(outcome, CompletionOutcome::Advanced)
            && let Interaction::Dialogue(dialogue) = &mut interaction
        {
            // Admission reserved the hash table; move owned IDs without allocating on input.
            if dialogue.open {
                for id in dialogue.pending_read_ids.drain(..) {
                    self.read_progress.ids.insert(id);
                }
                dialogue.awaiting_advance = false;
                dialogue.revision = dialogue.revision.wrapping_add(1);
            } else {
                for line in &mut dialogue.lines {
                    self.read_progress
                        .ids
                        .insert(std::mem::take(&mut line.content.id));
                }
            }
        }
        if matches!(&interaction, Interaction::Dialogue(d) if d.open) {
            self.reading_data.active = Some(interaction);
        }
        debug_assert!(self.reading_data.completion.is_none());
        self.reading_data.completion = Some(Completion {
            interaction_id,
            owner: self.root_owner(),
            outcome,
            delivery: CompletionDelivery::Resume,
        });
    }

    fn cancel_interaction(&mut self, reason: CancellationReason) {
        if let Some(interaction) = self.reading_data.active.take() {
            debug_assert!(self.reading_data.completion.is_none());
            self.reading_data.completion = Some(Completion {
                interaction_id: interaction.id(),
                owner: self.root_owner(),
                outcome: CompletionOutcome::Cancelled(reason),
                delivery: CompletionDelivery::CleanupOnly,
            });
        } else if let Some(completion) = &mut self.reading_data.completion {
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
        let id = session.begin_say(root, &[], "en", true).unwrap();
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
            session.begin_say(root, &[], "en", true),
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
