//! Bounded session/VM coordination with trusted native admission.
//!
//! A player drives one bounded slice per turn and dispatches the returned native
//! ticket through its trusted registry. The adapter owns the main TaskId, each
//! wait association and all independently retained result/key roots. It never
//! interprets bytecode or assumes a table's private storage layout.
mod activity;
mod assets;
mod media;
pub use activity::ActivityProgress;
mod budget;
mod cadence;
mod callbacks;
use budget::SliceMinimum;
pub use budget::{NativeBudget, SessionBudget};
pub mod capture;
pub mod checkpoint;
mod game;
mod input_hints;
pub use game::GameSettings;
mod choice;
mod content_identity;
pub mod data;
mod dialogue;
mod keys;
mod layered_image;
mod lists;
mod localization;
pub use localization::SourceCatalogs;
mod narrative;
mod observable;
mod pages;
mod particles;
mod publication;
mod registry;
mod results;
mod scopes;
pub mod transitions;
pub use results::geometry::{
    GeometryQuery, GeometryReplies, GeometryReply, NodeGeometry, RevealProjection,
};
mod conditions;
mod effects;
mod gradients;
mod masks;
mod rich_text;
pub mod saves;
pub mod snapshot;
pub mod source;
mod stage;
#[cfg(test)]
mod tests;
mod text;
mod views;
pub mod window;

use crate::{
    ChoiceInput, Completion, CompletionDelivery, CompletionOutcome, DialogueLineInput,
    InteractionId, OwnerHandle, OwnerRetirementRequirements, Session, SessionError,
    SessionIdentity,
};
pub use aside_vm::executor::{
    DeliveryId, FaultKind, HostTicket, RunOutcome, SliceBudget, TaskId, VmError, VmLimits,
};
pub use aside_vm::{FaultInfo, GcBudget, GcProgress};
use aside_vm::{
    HeapError, RootKind, RootOwner, RootToken, SessionEpoch, Value,
    bytecode::{Context, Effects, FunctionId, VerifiedProgram},
    executor::{Completion as VmCompletion, HostResponse, NativeInput, Vm, WaitId},
};

#[derive(Debug)]
pub enum AdapterError {
    Source(source::SourceError),
    Vm(VmError),
    Heap(HeapError),
    Session(SessionError),
    Input(&'static str),
    Asset(String),
    InputQueue(reel_core::result::RejectReason),
    Markup(reel_core::text::MarkupError),
    Allocation,
    Stage(reel_ui::stage::StageError),
    Snapshot(aside_vm::snapshot::SnapshotError),
    State(aside_vm::typed_data::SchemaError),
    Resume(aside_vm::resume::ResumeError),
}
impl From<aside_vm::resume::ResumeError> for AdapterError {
    fn from(error: aside_vm::resume::ResumeError) -> Self {
        Self::Resume(error)
    }
}
impl From<aside_vm::typed_data::SchemaError> for AdapterError {
    fn from(error: aside_vm::typed_data::SchemaError) -> Self {
        match error {
            aside_vm::typed_data::SchemaError::Heap(error) => Self::Heap(error),
            aside_vm::typed_data::SchemaError::Allocation => Self::Allocation,
            error => Self::State(error),
        }
    }
}
impl From<aside_vm::snapshot::SnapshotError> for AdapterError {
    fn from(error: aside_vm::snapshot::SnapshotError) -> Self {
        Self::Snapshot(error)
    }
}
impl From<reel_ui::stage::StageError> for AdapterError {
    fn from(error: reel_ui::stage::StageError) -> Self {
        Self::Stage(error)
    }
}
impl From<VmError> for AdapterError {
    fn from(e: VmError) -> Self {
        Self::Vm(e)
    }
}
impl From<HeapError> for AdapterError {
    fn from(e: HeapError) -> Self {
        Self::Heap(e)
    }
}
impl From<aside_host::DeclarationError> for AdapterError {
    fn from(error: aside_host::DeclarationError) -> Self {
        Self::Input(error.0)
    }
}
impl From<aside_host::Error> for AdapterError {
    fn from(error: aside_host::Error) -> Self {
        match error {
            aside_host::Error::Heap(error) => Self::Heap(error),
            aside_host::Error::Vm(error) => Self::Vm(error),
            aside_host::Error::Allocation => Self::Allocation,
            aside_host::Error::Arguments => {
                Self::Input("native argument count differs from declaration")
            }
        }
    }
}
impl From<SessionError> for AdapterError {
    fn from(e: SessionError) -> Self {
        Self::Session(e)
    }
}
impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Input(message) => f.write_str(message),
            Self::Asset(message) => f.write_str(message),
            Self::Snapshot(error) => write!(f, "{error}"),
            Self::State(error) => write!(f, "{error}"),
            Self::Resume(error) => write!(f, "{error}"),
            _ => write!(f, "{self:?}"),
        }
    }
}
impl std::error::Error for AdapterError {}
impl AdapterError {
    /// Language failures at the pending native's source location. Identity or
    /// internal ownership failures remain host errors and are not disguised.
    pub fn native_fault_kind(&self) -> Option<FaultKind> {
        match self {
            Self::Vm(VmError::Effect) => Some(FaultKind::Effect),
            Self::Vm(VmError::Limit) => Some(FaultKind::Resource),
            Self::InputQueue(_) => Some(FaultKind::Resource),
            Self::Input(_) | Self::Markup(_) | Self::State(_) => Some(FaultKind::Type),
            Self::Stage(_) => Some(FaultKind::Type),
            Self::Allocation | Self::Session(SessionError::AllocationFailed) => {
                Some(FaultKind::Resource)
            }
            Self::Session(
                SessionError::EmptyChoiceKey
                | SessionError::EmptyChoices
                | SessionError::DuplicateChoiceKey
                | SessionError::SingleDialogueLineRequired
                | SessionError::InvalidTextComposition,
            ) => Some(FaultKind::Type),
            Self::Session(SessionError::InvalidDialogueHandle) => Some(FaultKind::InvalidState),
            Self::Heap(HeapError::ConcurrentModification) => {
                Some(FaultKind::ConcurrentModification)
            }
            Self::Heap(HeapError::WrongKind | HeapError::TypeMismatch) => Some(FaultKind::Type),
            Self::Heap(HeapError::RetiredHost | HeapError::BindingCycle) => {
                Some(FaultKind::InvalidState)
            }
            Self::Heap(HeapError::ReadOnly) => Some(FaultKind::ReadOnly),
            Self::Heap(HeapError::OutOfBounds) => Some(FaultKind::OutOfBounds),
            Self::Heap(
                HeapError::ObjectCapacity
                | HeapError::RootCapacity
                | HeapError::ResourceLimit { .. }
                | HeapError::OperationLimit { .. },
            ) => Some(FaultKind::Resource),
            _ => None,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct RetainedKey {
    key: String,
    value: Value,
}
#[derive(serde::Serialize, serde::Deserialize)]
struct RetainedKeys {
    root: RootToken,
    #[serde(skip)]
    entries: Vec<RetainedKey>,
}
#[derive(serde::Serialize, serde::Deserialize)]
enum PendingInteractionKind {
    Advance,
    Reveal,
}
#[derive(serde::Serialize, serde::Deserialize)]
struct PendingInteraction {
    kind: PendingInteractionKind,
    interaction: InteractionId,
    task: TaskId,
    wait: WaitId,
    keys: Option<RetainedKeys>,
    return_value: Option<Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeDispatch {
    Ready {
        work: u32,
    },
    Decoding {
        work: u32,
    },
    /// The native retained its preparation in a VM continuation. A later slice
    /// needs at least `required_work` to perform its next indivisible step.
    BudgetYield {
        work: u32,
        required_work: u32,
    },
    Published {
        interaction: InteractionId,
        work: u32,
    },
}

#[derive(Debug)]
pub enum SessionStep {
    Vm(RunOutcome),
    Progress,
    Idle,
}

/// Owns the story and node callback tasks, their waits and retirement obligations.
/// Native tickets must belong to an admitted live task; dispatch never selects
/// work by the VM's most recently active task.
pub struct SessionVm {
    vm: Vm,
    resume: aside_vm::resume::VerifiedResumeTable,
    flow: checkpoint::FlowRuntime,
    reading_resume: Option<checkpoint::ReadingResume>,
    identity: SessionIdentity,
    owner: OwnerHandle,
    root_owner: RootOwner,
    task: Option<TaskId>,
    main_minimum: SliceMinimum,
    main_wait: Option<WaitId>,
    tasks: Vec<callbacks::ScriptTask>,
    checked_callback_lifetimes: Option<(u64, u64, u64)>,
    blocked_tasks_revision: Option<u64>,
    #[cfg(test)]
    callback_lifetime_checks: u64,
    scopes: scopes::Scopes,
    transitions: transitions::Transitions,
    schedule_cursor: usize,
    derived_task: Option<TaskId>,
    derived_minimum: SliceMinimum,
    reactive_ready: bool,
    ordinary_step_executed: bool,
    feedback_only: bool,
    // Source of the projected version survives subsequent task scheduling.
    projected_stage_revision: u64,
    authored_stage_revision: u64,
    issued: Option<HostTicket>,
    pending: Option<PendingInteraction>,
    choice_reader: Option<choice::ChoiceReader>,
    dialogue_reader: Option<dialogue::DialogueReader>,
    // Constructed speech handles and their VM objects, without production counters.
    #[cfg(test)]
    speech_costs: [usize; 2],
    claimed: Option<Completion>,
    delivery: Option<DeliveryId>,
    delivery_task: Option<TaskId>,
    result_root: Option<RootToken>,
    read_limit: usize,
    revoked: bool,
    root_cursor: u32,
    root_end: u32,
    stage: stage::StageBinding,
    activities: activity::Activities,
    results: results::Results,
    saves: saves::Saves,
    captures: capture::Captures,
    asset_reads: assets::AssetReads,
    window: window::Window,
    input_hints: input_hints::State,
    particles: particles::Particles,
    standard: aside_std::StandardLibrary,
    media: media::Media,
    translations: localization::Translations,
    game: std::sync::Arc<GameSettings>,
    narrative: Option<narrative::Narrative>,
    narrative_contexts: narrative::Contexts,
    skip_input: Option<reel_ui::stage::NodeId>,
    exit_requested: bool,
    start_requested: Option<source::StoryFunction>,
}

mod dispatch;

impl SessionVm {
    /// Bounds only accepted input waiting for an asynchronous editor operation.
    /// Ordinary typing/paste retain their usual text capacity. Existing accepted
    /// queues keep their admission limits until drained; new queues use this value.
    pub fn set_text_input_editing_limits(
        &mut self,
        limits: reel_ui::widgets::TextInputEditingLimits,
    ) {
        self.stage.editing_limits = limits;
    }
    /// Admit a source compilation with its generated locale catalogs through
    /// one loading boundary. Catalog text is shared, parsed only when requested,
    /// and remains available when the host selects another asset loader.
    pub fn load_story(
        session: &Session,
        story: source::CompiledStory,
        limits: VmLimits,
    ) -> Result<Self, AdapterError> {
        story.validate()?;
        let mut vm = Self::load(session, story.program, limits)?;
        vm.resume = story.resume;
        vm.stage.set_effect_products(vm.vm.heap(), story.effects)?;
        if !story.catalogs.catalogs.is_empty() {
            vm.set_source_catalogs(localization::SourceCatalogs::from_products(story.catalogs))?;
        }
        Ok(vm)
    }
    /// Share compiled effect definitions when starting or restoring this program.
    pub fn effect_products(&self) -> std::sync::Arc<source::EffectProducts> {
        self.stage.effect_products.clone()
    }
    /// Share current-code continuation metadata when starting or restoring this story.
    pub fn resume_table(&self) -> &aside_vm::resume::VerifiedResumeTable {
        &self.resume
    }
    pub fn load(
        session: &Session,
        program: VerifiedProgram,
        limits: VmLimits,
    ) -> Result<Self, AdapterError> {
        let support = aside_host::runtime_support(
            &program,
            source::contracts(),
            source::types(),
            &registry::capabilities(),
        )?;
        let identity = session.identity();
        let owner = session.root_owner();
        if !session.is_owner_live(owner) {
            return Err(AdapterError::Input("owner revoked"));
        }
        let vm = Vm::load(program, support, limits, SessionEpoch(identity.epoch.0))?;
        Self::from_vm(session, vm, limits)
    }
    fn from_vm(session: &Session, vm: Vm, limits: VmLimits) -> Result<Self, AdapterError> {
        let identity = session.identity();
        let owner = session.root_owner();
        let stage = stage::StageBinding::new(vm.heap())?;
        let standard = aside_std::StandardLibrary::new(vm.program());
        let resume = vm
            .program()
            .verify_resume(
                Default::default(),
                crate::aside::source::types(),
                Default::default(),
            )
            .expect("the empty continuation table is valid for every verified program");
        Ok(Self {
            vm,
            resume,
            flow: checkpoint::FlowRuntime::default(),
            reading_resume: None,
            identity,
            owner,
            root_owner: RootOwner(owner.owner_id().0),
            task: None,
            main_minimum: SliceMinimum::default(),
            main_wait: None,
            tasks: Vec::new(),
            checked_callback_lifetimes: None,
            blocked_tasks_revision: None,
            #[cfg(test)]
            callback_lifetime_checks: 0,
            scopes: scopes::Scopes::default(),
            transitions: transitions::Transitions::default(),
            schedule_cursor: 0,
            derived_task: None,
            derived_minimum: SliceMinimum::default(),
            reactive_ready: true,
            ordinary_step_executed: false,
            feedback_only: false,
            projected_stage_revision: 0,
            authored_stage_revision: 0,
            issued: None,
            pending: None,
            choice_reader: None,
            dialogue_reader: None,
            #[cfg(test)]
            speech_costs: [0; 2],
            claimed: None,
            delivery: None,
            delivery_task: None,
            result_root: None,
            read_limit: limits.heap.max_mutation_bytes.min(4096) as usize,
            revoked: false,
            root_cursor: 0,
            root_end: 0,
            stage,
            activities: activity::Activities::default(),
            results: results::Results::default(),
            saves: saves::Saves::default(),
            captures: capture::Captures::default(),
            asset_reads: assets::AssetReads::default(),
            window: window::Window::default(),
            input_hints: input_hints::State::default(),
            particles: particles::Particles::default(),
            standard,
            media: media::Media::default(),
            translations: localization::Translations::default(),
            game: std::sync::Arc::new(GameSettings::default()),
            narrative: None,
            narrative_contexts: narrative::Contexts::default(),
            skip_input: None,
            exit_requested: false,
            start_requested: None,
        })
    }

    fn same_session(&self, session: &Session) -> Result<(), AdapterError> {
        if session.identity() != self.identity {
            return Err(AdapterError::Input("foreign session"));
        }
        Ok(())
    }
    fn live(&self, session: &Session) -> Result<(), AdapterError> {
        self.same_session(session)?;
        if self.revoked || !session.is_owner_live(self.owner) {
            return Err(AdapterError::Input("owner revoked"));
        }
        Ok(())
    }
    pub fn vm(&self) -> &Vm {
        &self.vm
    }
    pub fn logical_time(&self) -> f64 {
        self.activities.time()
    }

    /// The work requested shutdown; the embedding host owns cleanup and its window.
    pub fn exit_requested(&self) -> bool {
        self.exit_requested
    }

    /// A validated entry in this program. The host replaces the session before
    /// executing any more script and reruns the module initializer first.
    pub fn start_requested(&self) -> Option<source::StoryFunction> {
        self.start_requested
    }

    /// Format a still-rooted fault's author value without traversing object graphs.
    pub fn format_fault_value(&self, fault: FaultInfo) -> Result<String, AdapterError> {
        use aside_vm::{ObjectKind, ValueTag};

        let heap = self.vm.heap();
        let value = heap.fault_value(fault.id)?;
        Ok(match value.tag() {
            ValueTag::Null => "null".into(),
            ValueTag::Bool => value.as_bool().unwrap().to_string(),
            ValueTag::Int => value.as_int().unwrap().to_string(),
            ValueTag::Float => value.as_float().unwrap().to_string(),
            ValueTag::Object => {
                let info = heap.info(value)?;
                if info.kind == ObjectKind::String {
                    text::read_string(heap, value, self.read_limit)?
                } else if info.kind == ObjectKind::CoreError {
                    let error = heap.core_error_info(value)?;
                    if error.details.tag() == ValueTag::Object
                        && heap.info(error.details)?.kind == ObjectKind::String
                    {
                        text::read_string(heap, error.details, self.read_limit)?
                    } else {
                        error.message.into()
                    }
                } else {
                    format!("<{:?}>", info.kind)
                }
            }
        })
    }
    pub fn task(&self) -> Option<TaskId> {
        self.task
    }
    /// Last projected stage revision containing an application update.
    /// Compare with the host's consumed watermark before merging stage versions.
    /// Revisions produced without a projection boundary are application updates.
    pub fn stage_authored_revision(&self) -> u64 {
        let revision = self.stage.scene.revision();
        if revision == self.projected_stage_revision {
            self.authored_stage_revision
        } else {
            revision
        }
    }
    pub fn stage(&self) -> &reel_ui::stage::Stage {
        &self.stage.scene
    }

    pub(super) fn reactive_settled(&self) -> bool {
        self.derived_task.is_none() && !self.vm.heap().derived_pending()
    }

    /// A whole virtual row has returned and ordinary observers have settled.
    /// The host may now synchronize mounted properties before recording feedback.
    pub fn virtual_feedback_boundary(&self) -> bool {
        if !self.reactive_ready
            || self.derived_task.is_some()
            || self.activities.pending()
            || self.transitions.pending(self.activities.time())
            || self.particles.needs_work(self.stage.retirement_revision)
        {
            return false;
        }
        let mut outputs = self.vm.heap().pending_derived_outputs().peekable();
        outputs.peek().is_some() && outputs.all(|output| self.virtual_feedback_output(output))
    }

    /// Only a committed mounted tree may paint while detached virtual rows wait.
    pub fn virtual_feedback_ready(&self) -> bool {
        self.virtual_feedback_boundary()
            && self
                .stage
                .mounted_properties_current(self.vm.heap())
                .unwrap_or(false)
    }

    /// Publish the properties of the same mounted objects after script work.
    pub fn sync_stage(&mut self, session: &Session) -> Result<(), AdapterError> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::UiStageSync);
        self.same_session(session)?;
        if !self.reactive_settled() && !self.virtual_feedback_boundary() {
            return Ok(());
        }
        self.sync_stage_properties(self.feedback_only)
    }

    pub(super) fn sync_stage_properties(&mut self, feedback: bool) -> Result<(), AdapterError> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::StageProperties);
        let before = self.stage.scene.revision();
        self.stage.sync(self.vm.heap_mut(), self.read_limit)?;
        let revision = self.stage.scene.revision();
        // Native topology edits precede property projection and are authored.
        if before != self.projected_stage_revision {
            self.authored_stage_revision = before;
        }
        if revision != before && !feedback {
            self.authored_stage_revision = revision;
        }
        self.projected_stage_revision = revision;
        Ok(())
    }
    /// Author execution has reached a commit boundary and the stage's complete
    /// activity/reactive batch has settled. Synchronous construction and writes
    /// retain their transaction until await, return or another ordinary boundary.
    pub fn stage_ready(&self) -> bool {
        self.reactive_ready
            && self.reactive_settled()
            && !self.activities.pending()
            && !self.transitions.pending(self.activities.time())
            && !self.particles.needs_work(self.stage.retirement_revision)
            && self.stage.pages_ready(self.vm.heap(), self.read_limit)
    }
    /// Whether the latest scheduler step executed ordinary task VM instructions.
    /// Reactive evaluation, maintenance and host-only continuation dispatch return
    /// false. Both [`Self::step`] and [`Self::step_reactive`] clear this transient
    /// result on entry; restoration starts false.
    pub fn ordinary_step_executed(&self) -> bool {
        self.ordinary_step_executed
    }
    pub fn cleanup_pending(&self) -> bool {
        self.revoked
            && (self.root_cursor < self.root_end
                || self.task.is_some()
                || !self.tasks.is_empty()
                || self.derived_task.is_some()
                || self.media.cleanup_pending())
    }

    /// Advance the session's monotonic logical clock, independently of VM slice
    /// timing. Work counts graph nodes, transitions and cleanup steps, each with
    /// fixed-schema property work. Drain pending before publishing the new frame.
    pub fn advance_time(
        &mut self,
        session: &Session,
        seconds: f64,
        work: u32,
    ) -> Result<ActivityProgress, AdapterError> {
        self.live(session)?;
        self.activities.observe_time(seconds)?;
        let sample = self.reactive_settled();
        let media_woke = sample && self.sample_media_positions()?;
        self.activities.set_skipping(session.reading().skipping());
        if sample {
            self.particles.begin(seconds)?;
        }
        let particle_work = self.maintain_particles(work)?;
        // An existing time batch keeps its target, even after its first write
        // invalidates observers. New time is sampled only once those observers
        // have settled; cleanup and partially sampled graph work still drain.
        let mut progress = if sample {
            self.activities
                .advance(&mut self.vm, work - particle_work)?
        } else {
            self.activities.drain(&mut self.vm, work - particle_work)?
        };
        progress.work += particle_work;
        progress.woke |= media_woke | self.particles.take_wake();
        if !self.activities.pending() {
            let transition = self
                .transitions
                .advance(self.activities.time(), work - progress.work);
            progress.work += transition.work;
            progress.woke |= transition.woke;
            progress.pending |= transition.pending;
        }
        progress.pending |= self.particles.needs_work(self.stage.retirement_revision);
        if self.reactive_ready && sample {
            self.sync_narrative(session)?;
        }
        progress.woke |= self.vm.heap().derived_pending() || self.derived_task.is_some();
        Ok(progress)
    }

    pub fn activities_pending(&self) -> bool {
        !self.revoked
            && (self.activities.pending()
                || self.particles.needs_work(self.stage.retirement_revision)
                || self.transitions.pending(self.activities.time()))
    }

    pub fn activities_need_time(&self) -> bool {
        !self.revoked
            && (self.activities.needs_time()
                || self.particles.needs_time()
                || self.transitions.needs_time())
    }

    /// Whether active clocks, tweens, particles or transitions require repeated
    /// time sampling. This constant-time query excludes pure delay deadlines;
    /// [`Self::activities_poll_after`] still reports those deadlines normally.
    pub fn continuous_time_pending(&self) -> bool {
        !self.revoked
            && (self.activities.continuous_time_pending()
                || self.particles.needs_time()
                || self.transitions.needs_time())
    }

    /// Delay from the latest logical sample to the next presentation cadence sample.
    pub fn animation_sample_after(&self) -> std::time::Duration {
        let rate = self
            .game
            .animation_rate(self.window_info().refresh_rate_millihertz);
        rate.map_or(std::time::Duration::ZERO, |rate| {
            cadence::poll_after(self.activities.time(), rate)
        })
    }

    /// Delay from the latest logical sample to the next producer deadline.
    pub fn activities_poll_after(&self) -> Option<std::time::Duration> {
        if self.revoked {
            return None;
        }
        let sampling = self.animation_sample_after();
        let particles =
            (self.particles.needs_time() || self.transitions.needs_time()).then_some(sampling);
        match (self.activities.poll_after(sampling), particles) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Next finite activity endpoint or periodic clock awaited by business code.
    /// Unobserved clock intervals and intermediate animation samples may be
    /// combined while a previous drawing occupies the frame production slot.
    pub fn activities_deadline_after(&self) -> Option<std::time::Duration> {
        (!self.revoked)
            .then(|| self.activities.deadline_poll_after(&self.vm))
            .flatten()
    }

    pub fn start(&mut self, session: &Session, entry: FunctionId) -> Result<TaskId, AdapterError> {
        self.live(session)?;
        if self.task.is_some() || self.pending.is_some() || self.delivery.is_some() {
            return Err(AdapterError::Input("main task already occupied"));
        }
        let task = self
            .vm
            .start(entry, &[], self.root_owner, Context::Ordinary)?;
        self.task = Some(task);
        self.blocked_tasks_revision = None;
        self.main_minimum = SliceMinimum::default();
        self.main_wait = None;
        Ok(task)
    }

    /// Preserve BudgetYield and ResourceBlocked verbatim; neither is terminal.
    /// Pending input is routed before the next slice, never to vm.task_id().
    /// The host polls media, localization and asset reads through their explicit
    /// maintenance methods at each host turn and after external wakeups. Native
    /// and derived reentry within that turn only advances local execution work.
    pub fn step(
        &mut self,
        session: &mut Session,
        budget: SessionBudget,
        now: impl FnMut() -> u64,
    ) -> Result<SessionStep, AdapterError> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::SessionSchedule);
        self.ordinary_step_executed = false;
        if self.exit_requested || self.start_requested.is_some() {
            self.live(session)?;
            return Ok(SessionStep::Idle);
        }
        self.step_tasks(session, budget, now)
    }

    fn check_ticket(&self, session: &Session, ticket: HostTicket) -> Result<(), AdapterError> {
        self.live(session)?;
        if self.issued != Some(ticket)
            || if self.task == Some(ticket.task) || self.derived_task == Some(ticket.task) {
                ticket.owner != self.root_owner
            } else {
                self.callback_owner(session, ticket.task) != Some(ticket.owner)
            }
        {
            return Err(AdapterError::Input("foreign or consumed native ticket"));
        }
        self.vm.host_arguments(ticket)?; // validate opaque identity and complete metadata
        Ok(())
    }

    fn check_wait(&self, session: &Session, ticket: HostTicket) -> Result<(), AdapterError> {
        self.check_ticket(session, ticket)?;
        // Vm::respond(Pending) may return Ok after installing an effect fault.
        // Perform the same check BEFORE publishing an interaction/history row.
        if ticket.context.no_wait
            || ticket.context.init
            || ticket.context.derived
            || ticket.effects.0 & Effects::WAIT == 0
        {
            return Err(AdapterError::Vm(VmError::Effect));
        }
        Ok(())
    }

    fn check_story_wait(&self, session: &Session, ticket: HostTicket) -> Result<(), AdapterError> {
        self.check_wait(session, ticket)?;
        if self.task != Some(ticket.task) {
            return Err(AdapterError::Vm(VmError::Effect));
        }
        if self.pending.is_some() {
            return Err(AdapterError::Input("wait already occupied"));
        }
        Ok(())
    }

    pub fn reject_native(
        &mut self,
        session: &Session,
        ticket: HostTicket,
        error: AdapterError,
    ) -> Result<(), AdapterError> {
        self.check_ticket(session, ticket)?;
        let Some(kind) = error.native_fault_kind() else {
            return Err(error);
        };
        let details = if kind == FaultKind::Resource {
            Value::NULL
        } else {
            text::alloc_string(self.vm.heap_mut(), &error.to_string(), self.read_limit)?
        };
        self.vm.respond_language_error(ticket, kind, details)?;
        self.reject_transition(ticket)?;
        self.activities
            .cancel_native(self.vm.heap_mut(), ticket.task)?;
        if let Some(reader) = self.choice_reader.take() {
            reader.release(self.vm.heap_mut())?;
        }
        if let Some(reader) = self.dialogue_reader.take() {
            reader.release(self.vm.heap_mut())?;
        }
        self.saves.capture = None;
        self.issued = None;
        Ok(())
    }

    /// Publish a narrator line using its complete logical caller path as the
    /// content identity. The identity is independent of locale and source maps.
    /// An insufficient exact budget retains the native ticket without publishing.
    pub fn dispatch_narrator(
        &mut self,
        session: &mut Session,
        ticket: HostTicket,
        locale: &str,
        budget: NativeBudget,
    ) -> Result<NativeDispatch, AdapterError> {
        self.check_story_wait(session, ticket)?;
        let p = self.vm.program().package();
        let native = &p.natives[ticket.native.index()];
        if p.symbols[native.symbol.index()] != "reel::narrator" {
            return Err(AdapterError::Input("not narrator"));
        }
        let (content_id, work) = match self.prepare_content_id(ticket, budget)? {
            content_identity::Preparation::Ready { id, work } => (id, work),
            content_identity::Preparation::BudgetYield { required_work } => {
                return Ok(NativeDispatch::BudgetYield {
                    work: 0,
                    required_work,
                });
            }
        };
        let arguments = self.vm.host_arguments(ticket)?;
        let [text, history, reveal_skippable, wait] = arguments else {
            return Err(AdapterError::Input("narrator arity"));
        };
        let reveal_skippable = reveal_skippable
            .as_bool()
            .ok_or(AdapterError::Input("reveal_skippable must be bool"))?;
        let wait = wait
            .as_bool()
            .ok_or(AdapterError::Input("wait must be bool"))?;
        let text = rich_text::read(self.vm.heap(), *text, self.read_limit)?;
        let history = dialogue::HistoryText::read(self.vm.heap(), *history, self.read_limit)?;
        let interaction = self.publish_say(
            session,
            ticket,
            &[DialogueLineInput {
                history: history.input(),
                speaker_id: None,
                speaker_name: "",
                content_id: &content_id,
                text: (&text).into(),
            }],
            locale,
            reveal_skippable,
            wait,
        )?;
        Ok(NativeDispatch::Published { interaction, work })
    }

    /// Publish already decoded ADV lines from a trusted native implementation.
    /// The host decoder must use the real author schema; this is not a script API.
    fn publish_say(
        &mut self,
        session: &mut Session,
        ticket: HostTicket,
        lines: &[DialogueLineInput<'_>],
        locale: &str,
        reveal_skippable: bool,
        wait: bool,
    ) -> Result<InteractionId, AdapterError> {
        self.check_story_wait(session, ticket)?;
        let selected_locale = self.locale();
        let locale = selected_locale.as_deref().unwrap_or(locale);
        let publication = publication::Publication::stage(session, |s| {
            if wait {
                s.begin_say(self.owner, lines, locale, reveal_skippable)
            } else {
                s.begin_open_say(self.owner, lines, locale, reveal_skippable)
            }
        })?;
        let interaction = publication.id();
        if wait {
            self.accept_wait(session, ticket, publication, None)?;
        } else {
            let handle = self.speech_handle(interaction)?;
            publication.commit(session);
            self.sync_narrative(session)?;
            self.vm.respond(ticket, HostResponse::Ready(handle))?;
            self.issued = None;
        }
        Ok(interaction)
    }

    /// Decoder output uses the original immutable key Values, not reconstructed
    /// strings. One rooted array retains every key before releasing native entry.
    fn publish_choice(
        &mut self,
        session: &mut Session,
        ticket: HostTicket,
        rows: &[(Value, Value)],
    ) -> Result<InteractionId, AdapterError> {
        self.check_story_wait(session, ticket)?;
        let mut decoded = Vec::new();
        decoded
            .try_reserve_exact(rows.len())
            .map_err(|_| AdapterError::Allocation)?;
        for &(key, label) in rows {
            let key = text::read_string(self.vm.heap(), key, self.read_limit)?;
            let label = text::read_string(self.vm.heap(), label, self.read_limit)?;
            decoded.push((key, label));
        }
        let mut keys = Vec::new();
        keys.try_reserve_exact(rows.len())
            .map_err(|_| AdapterError::Allocation)?;
        let mut inputs = Vec::new();
        inputs
            .try_reserve_exact(decoded.len())
            .map_err(|_| AdapterError::Allocation)?;
        inputs.extend(
            decoded
                .iter()
                .map(|(key, label)| ChoiceInput { key, label }),
        );
        let references = self.vm.heap_mut().alloc_array(&[])?;
        for &(value, _) in rows {
            self.vm.heap_mut().array_push(references, value)?;
        }
        let publication =
            publication::Publication::stage(session, |s| s.begin_choice(self.owner, &inputs))?;
        let root = self
            .vm
            .heap_mut()
            .add_root(self.root_owner, RootKind::Host, references)?;
        for (&(value, _), (key, _)) in rows.iter().zip(decoded) {
            keys.push(RetainedKey { key, value });
        }
        let interaction = publication.id();
        self.accept_wait(
            session,
            ticket,
            publication,
            Some(RetainedKeys {
                root,
                entries: keys,
            }),
        )?;
        Ok(interaction)
    }

    fn accept_wait(
        &mut self,
        session: &mut Session,
        ticket: HostTicket,
        publication: publication::Publication,
        keys: Option<RetainedKeys>,
    ) -> Result<(), AdapterError> {
        let wait = ticket.wait_id();
        // The prepared interaction/history is not visible during VM response.
        // Respond validates before mutation and cannot run scripts/callbacks.
        // If it fails, Session counters/history remain exactly unchanged.
        if let Err(error) = self.vm.respond(ticket, HostResponse::Pending(wait)) {
            self.release_keys(keys)?;
            return Err(error.into());
        }
        self.pending = Some(PendingInteraction {
            kind: PendingInteractionKind::Advance,
            interaction: publication.id(),
            task: ticket.task,
            wait,
            keys,
            return_value: None,
        });
        self.feedback_only = false;
        publication.commit(session);
        self.issued = None;
        Ok(())
    }

    fn release_keys(&mut self, keys: Option<RetainedKeys>) -> Result<(), AdapterError> {
        if let Some(keys) = keys {
            self.vm.heap_mut().release_root(keys.root)?;
        }
        Ok(())
    }

    fn deliver_completion(&mut self, session: &mut Session) -> Result<(), AdapterError> {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| matches!(pending.kind, PendingInteractionKind::Reveal))
        {
            return self.deliver_speech_reveal(session);
        }
        if self.pending.is_none() {
            return Ok(());
        }
        if self.claimed.is_none() {
            self.claimed = session.take_completion();
        }
        let Some(completion) = self.claimed.as_ref() else {
            return Ok(());
        };
        let pending = self.pending.as_ref().unwrap();
        if completion.interaction_id() != pending.interaction
            || completion.owner() != self.owner
            || self.task != Some(pending.task)
        {
            return Err(AdapterError::Input("completion identity mismatch"));
        }
        self.live(session)?;
        if completion.delivery() != CompletionDelivery::Resume {
            return Err(AdapterError::Input("cleanup-only completion"));
        }
        let value = match completion.outcome() {
            CompletionOutcome::Advanced if pending.keys.is_none() => {
                pending.return_value.unwrap_or(Value::NULL)
            }
            CompletionOutcome::Selected(selected) => {
                let key = pending
                    .keys
                    .as_ref()
                    .ok_or(AdapterError::Input("choice completion without keys"))?
                    .entries
                    .iter()
                    .find(|key| &key.key == selected)
                    .ok_or(AdapterError::Input("unknown choice key"))?;
                key.value
            }
            _ => return Err(AdapterError::Input("completion kind mismatch")),
        };
        self.vm.complete(pending.wait, VmCompletion::Ready(value))?;
        // complete publishes into the NativeFrame before independent roots go.
        let pending = self.pending.take().unwrap();
        self.claimed = None;
        self.release_keys(pending.keys)?;
        Ok(())
    }

    pub fn retain_return(&mut self, session: &Session) -> Result<Value, AdapterError> {
        self.live(session)?;
        let delivery = self.delivery.ok_or(AdapterError::Input("no delivery"))?;
        if self.vm.delivery_fault(delivery)?.is_some() {
            return Err(AdapterError::Input("fault delivery"));
        }
        let root = match self.result_root {
            Some(root) => root,
            None => {
                let root =
                    self.vm
                        .heap_mut()
                        .add_root(self.root_owner, RootKind::Host, Value::NULL)?;
                self.result_root = Some(root);
                root
            }
        };
        self.vm.take_delivery_root(delivery, root)?;
        self.delivery = None;
        self.delivery_task = None;
        self.task = None;
        self.main_wait = None;
        Ok(self.vm.heap().root_value(root)?)
    }

    pub fn discard_delivery(&mut self, session: &mut Session) -> Result<(), AdapterError> {
        self.live(session)?;
        let delivery = self.delivery.ok_or(AdapterError::Input("no delivery"))?;
        self.vm.discard_delivery(delivery)?;
        self.delivery = None;
        let task = self.delivery_task.take();
        if task == self.task {
            self.task = None;
            self.main_wait = None;
        } else if task == self.derived_task {
            self.derived_task = None;
        } else if let Some(index) = self
            .tasks
            .iter()
            .position(|callback| Some(callback.task) == task)
        {
            self.retire_callback(session, index)?;
        }
        Ok(())
    }

    /// End through the same retirement path as cancellation, retaining all
    /// declared cleanup obligations. A successful result must be claimed first.
    pub fn finish(&mut self, session: &mut Session) -> Result<(), AdapterError> {
        self.live(session)?;
        if self.task.is_some()
            || self.pending.is_some()
            || self.delivery.is_some()
            || !self.tasks.is_empty()
            || self.derived_task.is_some()
            || self.vm.heap().derived_pending()
        {
            return Err(AdapterError::Input("main task has not finished"));
        }
        self.revoke(session)?;
        session.status = crate::SessionStatus::Ended;
        Ok(())
    }

    /// Stop publication first, cancel tasks second; root retirement is separate
    /// and remains necessary when the last task has already delivered.
    pub fn revoke(&mut self, session: &mut Session) -> Result<(), AdapterError> {
        self.same_session(session)?;
        if self.revoked {
            // Publication stays blocked, but a previous cancellation error must
            // not turn a retry into a false success.
            return match self.vm.cancel(self.root_owner) {
                Ok(()) | Err(VmError::Idle) => Ok(()),
                Err(error) => Err(error.into()),
            };
        }
        for index in 0..self.tasks.len() {
            self.cancel_callback(session, index)?;
        }
        self.flow.clear(self.vm.heap_mut())?;
        if let Some(task) = self.task {
            self.cancel_task_transitions(task, None)?;
            self.cancel_task_scopes(session, task)?;
        }
        for index in 0..session.owners.len() {
            let peer = session.owners[index].handle;
            if peer != self.owner {
                session
                    .revoke_owner_with_retirement(peer, OwnerRetirementRequirements::default())?;
            }
        }
        self.root_end = self.vm.heap().root_slots();
        self.media.close();
        self.translations.close();
        self.results.clear();
        self.saves.clear();
        self.captures = capture::Captures::default();
        self.asset_reads.close();
        self.transitions = transitions::Transitions::default();
        self.window = window::Window::default();
        self.input_hints = input_hints::State::default();
        self.particles.clear();
        self.activities.revoke(self.vm.heap_mut())?;
        self.stage.revoke(self.vm.heap_mut())?;
        session.revoke_owner_with_retirement(
            self.owner,
            OwnerRetirementRequirements {
                root_slots_end: self.root_end as usize,
                tasks_pending: self.task.is_some() || self.derived_task.is_some(),
                external_resources_pending: self.media.cleanup_pending(),
            },
        )?;
        self.revoked = true;
        self.main_minimum = SliceMinimum::default();
        self.derived_minimum = SliceMinimum::default();
        self.issued = None;
        // Tokens are invalidated by the owner scan; never release them twice.
        self.pending = None;
        self.reading_resume = None;
        self.choice_reader = None;
        self.dialogue_reader = None;
        self.claimed = None;
        self.result_root = None;
        self.delivery = None;
        self.delivery_task = None;
        match self.vm.cancel(self.root_owner) {
            Ok(()) | Err(VmError::Idle) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// Returns actual root slots scanned, counting holes. Task cleanup receives
    /// its own slice budget. No task is required for host-root retirement.
    pub fn cleanup_step(
        &mut self,
        session: &mut Session,
        root_work: u32,
        budget: SessionBudget,
        now: impl FnMut() -> u64,
    ) -> Result<u32, AdapterError> {
        self.same_session(session)?;
        if !self.revoked {
            return Err(AdapterError::Input("owner not revoked"));
        }
        self.maintain_media()?;
        if !self.media.cleanup_pending() {
            session.confirm_owner_resources_released(self.owner)?;
        }
        let before = self.root_cursor;
        let slots = root_work.min(self.root_end - self.root_cursor);
        self.vm
            .heap_mut()
            .release_owner_step(self.root_owner, &mut self.root_cursor, slots);
        session.report_owner_root_progress(
            self.owner,
            self.root_cursor as usize,
            slots as usize,
        )?;
        if let Some(callback) = self.tasks.first_mut() {
            match callback
                .minimum
                .run(&mut self.vm, callback.task, budget, now)
            {
                Err(VmError::Idle) => {
                    self.retire_callback(session, 0)?;
                }
                Ok(RunOutcome::BudgetYield(_)) | Ok(RunOutcome::ResourceBlocked(_)) => {}
                Err(error) => return Err(error.into()),
                Ok(_) => {
                    return Err(AdapterError::Input(
                        "unexpected callback cancellation outcome",
                    ));
                }
            }
            return Ok(self.root_cursor - before);
        }
        if let Some(task) = self.derived_task {
            match self.derived_minimum.run(&mut self.vm, task, budget, now) {
                Err(VmError::Idle) => {
                    self.derived_task = None;
                    if self.task.is_none() {
                        session.confirm_owner_task_cleanup(self.owner)?;
                    }
                }
                Ok(RunOutcome::BudgetYield(_)) | Ok(RunOutcome::ResourceBlocked(_)) => {}
                Err(error) => return Err(error.into()),
                Ok(_) => {
                    return Err(AdapterError::Input(
                        "unexpected derived cancellation outcome",
                    ));
                }
            }
            return Ok(self.root_cursor - before);
        }
        if let Some(task) = self.task {
            match self.main_minimum.run(&mut self.vm, task, budget, now) {
                Err(VmError::Idle) => {
                    self.task = None;
                    session.confirm_owner_task_cleanup(self.owner)?;
                }
                Ok(RunOutcome::BudgetYield(_)) | Ok(RunOutcome::ResourceBlocked(_)) => {}
                Err(error) => return Err(error.into()),
                Ok(_) => return Err(AdapterError::Input("unexpected cancellation outcome")),
            }
        }
        Ok(self.root_cursor - before)
    }

    pub fn begin_collection(&mut self) -> Result<(), AdapterError> {
        Ok(self.vm.begin_collection()?)
    }
    pub fn gc_step(&mut self, budget: GcBudget) -> Result<GcProgress, AdapterError> {
        let progress = self.vm.gc_step(budget)?;
        if progress.cycle_complete {
            self.results.owners_changed();
        }
        Ok(progress)
    }
}
