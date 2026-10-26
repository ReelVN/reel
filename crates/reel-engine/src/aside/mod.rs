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
mod callbacks;
use budget::SliceMinimum;
pub use budget::{NativeBudget, SessionBudget};
pub mod capture;
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
mod particles;
mod publication;
mod registry;
mod results;
mod scopes;
pub mod transitions;
pub use results::geometry::{GeometryQuery, GeometryReplies, GeometryReply, NodeGeometry};
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
            Self::InputQueue(_) => Some(FaultKind::Resource),
            Self::Input(_) | Self::Markup(_) => Some(FaultKind::Type),
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
    identity: SessionIdentity,
    owner: OwnerHandle,
    root_owner: RootOwner,
    task: Option<TaskId>,
    main_minimum: SliceMinimum,
    main_wait: Option<WaitId>,
    tasks: Vec<callbacks::ScriptTask>,
    scopes: scopes::Scopes,
    transitions: transitions::Transitions,
    schedule_cursor: usize,
    derived_task: Option<TaskId>,
    derived_minimum: SliceMinimum,
    reactive_ready: bool,
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
        Ok(Self {
            vm,
            identity,
            owner,
            root_owner: RootOwner(owner.owner_id().0),
            task: None,
            main_minimum: SliceMinimum::default(),
            main_wait: None,
            tasks: Vec::new(),
            scopes: scopes::Scopes::default(),
            transitions: transitions::Transitions::default(),
            schedule_cursor: 0,
            derived_task: None,
            derived_minimum: SliceMinimum::default(),
            reactive_ready: true,
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
        self.same_session(session)?;
        if !self.reactive_settled() && !self.virtual_feedback_boundary() {
            return Ok(());
        }
        self.sync_stage_properties(self.feedback_only)
    }

    pub(super) fn sync_stage_properties(&mut self, feedback: bool) -> Result<(), AdapterError> {
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
    /// Reactive factories must settle before a host acknowledges their narrative frame.
    pub fn stage_ready(&self) -> bool {
        !self.activities.pending()
            && !self.transitions.pending(self.activities.time())
            && !self.particles.needs_work(self.stage.retirement_revision)
            && self.derived_task.is_none()
            && !self.vm.heap().derived_pending()
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

    pub fn activities_poll_after(&self) -> Option<std::time::Duration> {
        if self.revoked {
            return None;
        }
        let particles = (self.particles.needs_time() || self.transitions.needs_time())
            .then_some(std::time::Duration::from_secs_f64(1. / 60.));
        match (self.activities.poll_after(), particles) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    pub fn dispatch(
        &mut self,
        session: &mut Session,
        ticket: HostTicket,
        locale: &str,
        budget: NativeBudget,
    ) -> Result<NativeDispatch, AdapterError> {
        let work = budget.work();
        self.check_ticket(session, ticket)?;
        if let Some(work) = self.standard.dispatch(&mut self.vm, ticket, work)? {
            self.issued = None;
            return Ok(NativeDispatch::Ready { work });
        }
        let p = self.vm.program().package();
        let media_read = match p.symbols[p.natives[ticket.native.index()].symbol.index()].as_str() {
            "reel::audio::position" => Some("reel::audio::position"),
            "reel::video::position" => Some("reel::video::position"),
            _ => None,
        };
        if let Some(symbol) = media_read {
            self.dispatch_media(ticket, symbol)?;
            return Ok(NativeDispatch::Ready { work: 1 });
        }
        let symbol = p.symbols[p.natives[ticket.native.index()].symbol.index()].clone();
        match symbol.as_str() {
            "reel::game::start" => {
                let value = self.vm.host_arguments(ticket)?[0];
                let entry = self.vm.unbound_function(value)?;
                source::validate_entry(self.vm.program(), entry)
                    .map_err(|_| AdapterError::Input("game.start requires an ordinary function with no captures or required parameters"))?;
                self.vm.respond(ticket, HostResponse::Ready(Value::NULL))?;
                self.issued = None;
                self.start_requested = Some(entry);
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::stage::capture" => {
                self.dispatch_capture(ticket)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::stage::transition" => {
                self.dispatch_transition(ticket)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::stage::input" => {
                self.dispatch_input_hints(ticket)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::window::info"
            | "reel::window::configure"
            | "reel::stage::pointer"
            | "reel::stage::move_pointer" => {
                self.dispatch_window(ticket, &symbol)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::assets::read" => {
                self.dispatch_asset_read(ticket)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::saves::save"
            | "reel::saves::load"
            | "reel::saves::copy"
            | "reel::saves::delete"
            | "reel::saves::list"
            | "reel::saves::thumbnail" => self.dispatch_save(ticket, &symbol, work),
            "reel::persistent::read" | "reel::persistent::write" | "reel::persistent::delete" => {
                self.dispatch_persistent(ticket, &symbol, work)
            }
            "reel::game::exit" => {
                self.vm.respond(ticket, HostResponse::Ready(Value::NULL))?;
                self.issued = None;
                self.exit_requested = true;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::linear_gradient" => {
                let args = self.vm.host_arguments(ticket)?.to_vec();
                let value = gradients::create(self.vm.heap_mut(), &args, self.read_limit)?;
                self.stage
                    .types
                    .mark(self.vm.heap_mut(), value, "reel::types::LinearGradient")?;
                self.vm.respond(ticket, HostResponse::Ready(value))?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::appearance" => {
                let args = self.vm.host_arguments(ticket)?.to_vec();
                let value = stage::interaction_appearance::create(
                    self.vm.heap_mut(),
                    &args,
                    self.read_limit,
                )?;
                self.stage
                    .types
                    .mark(self.vm.heap_mut(), value, "reel::types::Appearance")?;
                self.vm.respond(ticket, HostResponse::Ready(value))?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::texture" => {
                let path = self.vm.host_arguments(ticket)?[0];
                let value = effects::create_texture(self.vm.heap_mut(), path, self.read_limit)?;
                self.stage
                    .types
                    .mark(self.vm.heap_mut(), value, "reel::types::Texture")?;
                self.vm.respond(ticket, HostResponse::Ready(value))?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            name if name.starts_with("reel::texture::") => Err(AdapterError::Input(
                "texture sampling is evaluated inside a compiled effect",
            )),
            "reel::effect" => {
                let args = self.vm.host_arguments(ticket)?.to_vec();
                let function = self.vm.unbound_function(args[0])?;
                let definition =
                    self.stage
                        .effect_products
                        .get(function)
                        .ok_or(AdapterError::Input(
                            "effect function has no compiled definition",
                        ))?;
                let value = effects::create(self.vm.heap_mut(), function, definition, args[1])?;
                self.stage
                    .types
                    .mark(self.vm.heap_mut(), value, "reel::types::Effect")?;
                self.vm.respond(ticket, HostResponse::Ready(value))?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::fragment::sample" => Err(AdapterError::Input(
                "fragment sampling is evaluated inside a compiled effect",
            )),
            name if name.starts_with("reel::mask::") => {
                let args = self.vm.host_arguments(ticket)?.to_vec();
                let value = masks::create(self.vm.heap_mut(), &name[12..], &args, self.read_limit)?;
                self.stage
                    .types
                    .mark(self.vm.heap_mut(), value, "reel::types::Mask")?;
                self.vm.respond(ticket, HostResponse::Ready(value))?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::rich" => {
                let source = self.vm.host_arguments(ticket)?[0];
                let response = match rich_text::create(self.vm.heap_mut(), source, self.read_limit)
                {
                    Ok(value) => HostResponse::Ready(value),
                    Err(AdapterError::Markup(error)) => {
                        let value = text::alloc_string(
                            self.vm.heap_mut(),
                            &error.to_string(),
                            self.read_limit,
                        )?;
                        HostResponse::Error(aside_vm::NewFault {
                            value,
                            code: aside_vm::FaultCode::core(FaultKind::Type),
                            cause: None,
                        })
                    }
                    Err(error) => return Err(error),
                };
                self.vm.respond(ticket, response)?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::rich_escape" => {
                let input = self.vm.host_arguments(ticket)?[0];
                let source = text::read_string(self.vm.heap(), input, self.read_limit)?;
                let value = text::alloc_string(
                    self.vm.heap_mut(),
                    &source.replace('[', "[["),
                    self.read_limit,
                )?;
                self.vm.respond(ticket, HostResponse::Ready(value))?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::tr"
            | "reel::localization::load"
            | "reel::localization::locale"
            | "reel::localization::set_locale" => {
                if symbol != "reel::localization::locale" && symbol != "reel::tr" {
                    self.check_wait(session, ticket)?;
                }
                self.dispatch_localization(ticket, &symbol)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::video::play"
            | "reel::video::seek"
            | "reel::video::position"
            | "reel::audio::play"
            | "reel::audio::queue"
            | "reel::audio::volume"
            | "reel::audio::await"
            | "reel::audio::group_volume"
            | "reel::audio::pan"
            | "reel::audio::rate"
            | "reel::audio::seek"
            | "reel::audio::at"
            | "reel::video::at"
            | "reel::audio::position"
            | "reel::audio::stop" => {
                if symbol == "reel::audio::await" {
                    self.check_wait(session, ticket)?;
                }
                self.dispatch_media(ticket, &symbol)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::game::dialogue"
            | "reel::game::choice"
            | "reel::game::history"
            | "reel::reading::clear_history"
            | "reel::game::reading"
            | "reel::game::clear_read_progress"
            | "reel::reading::set_auto"
            | "reel::reading::set_skip"
            | "reel::reading::pause"
            | "reel::dialogue::clear"
            | "reel::choice::select" => {
                self.dispatch_narrative(session, ticket, &symbol)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::view" => {
                self.dispatch_view(ticket)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::when" => {
                self.dispatch_when(ticket)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::when::evaluate" => {
                self.evaluate_condition(ticket)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::completion"
            | "reel::stage::presented"
            | "reel::spawn"
            | "reel::scope::spawn"
            | "reel::task::cancel"
            | "reel::completion::resolve"
            | "reel::completion::reject"
            | "reel::completion::cancel"
            | "reel::completion::await" => {
                if symbol == "reel::completion::await" {
                    self.check_wait(session, ticket)?;
                }
                self.dispatch_result(session, ticket, &symbol)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::list::scroll_to" => {
                self.dispatch_list_scroll_to(ticket)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::stage::geometry" => {
                self.dispatch_geometry(ticket)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::list" => {
                self.dispatch_list(ticket)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::layered_image" => {
                let work = self.dispatch_layered_image(ticket)?;
                Ok(NativeDispatch::Ready { work })
            }
            "reel::derive" => {
                self.vm.respond_derive(ticket)?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::model" => {
                let work = self.vm.respond_model(ticket, work)?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work })
            }
            "reel::game::configure" => {
                self.configure_game(ticket)?;
                Ok(NativeDispatch::Ready { work: 4 })
            }
            "reel::sequence"
            | "reel::parallel"
            | "reel::action::then"
            | "reel::action::together" => {
                let pair = symbol.starts_with("reel::action::");
                let kind = if matches!(symbol.as_str(), "reel::sequence" | "reel::action::then") {
                    activity::action::Kind::Sequence
                } else {
                    activity::action::Kind::Parallel
                };
                let work = activity::action::compose(
                    &mut self.vm,
                    &self.stage.types,
                    ticket,
                    kind,
                    pair,
                    work,
                )?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work })
            }
            "reel::scope" | "reel::reading" => {
                self.dispatch_scope(session, ticket, symbol == "reel::reading")?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::scope::cancel" => {
                self.dispatch_scope_exit(ticket, scopes::ScopeExit::Cancel)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::scope::finish" => {
                self.dispatch_scope_exit(ticket, scopes::ScopeExit::Finish)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::scope::override" => {
                self.dispatch_scope_override(ticket)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::scope::offset" => {
                self.dispatch_scope_offset(ticket)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::override::remove" => {
                self.dispatch_override_remove(ticket)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::scope::add" => {
                self.dispatch_scope_add(ticket)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::run" | "reel::clock" | "reel::scope::run" => {
                let args = self.vm.host_arguments(ticket)?;
                let [action, owner, paused] = if symbol == "reel::scope::run" {
                    [args[1], args[0], args[2]]
                } else {
                    [args[0], args[1], args[2]]
                };
                let owner = if owner.tag() == aside_vm::ValueTag::Null {
                    self.default_activity_owner(ticket.task)?
                } else {
                    owner
                };
                let start = if symbol == "reel::clock" {
                    activity::Activities::clock
                } else {
                    activity::Activities::run
                };
                let (done, work) = start(
                    &mut self.activities,
                    &mut self.vm,
                    &self.stage,
                    self.root_owner,
                    ticket,
                    [action, owner, paused],
                    work,
                )?;
                if done {
                    self.issued = None;
                    Ok(NativeDispatch::Ready { work })
                } else {
                    Ok(NativeDispatch::Decoding { work })
                }
            }
            "reel::tween" | "reel::repeat" | "reel::delay" => {
                let args = self.vm.host_arguments(ticket)?.to_vec();
                let value = match symbol.as_str() {
                    "reel::delay" => activity::action::delay(self.vm.heap_mut(), args[0])?,
                    "reel::tween" => activity::action::tween(
                        self.vm.heap_mut(),
                        &self.stage,
                        &args,
                        self.read_limit,
                    )?,
                    "reel::repeat" => {
                        activity::action::repeat(self.vm.heap_mut(), args[0], args[1])?
                    }
                    _ => unreachable!("registered activity constructor"),
                };
                self.stage
                    .types
                    .mark(self.vm.heap_mut(), value, "reel::types::Action")?;
                self.vm.respond(ticket, HostResponse::Ready(value))?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::emit" => self.dispatch_emit(ticket, budget),
            "reel::clock::next" => {
                self.check_wait(session, ticket)?;
                self.activities.next_tick(&mut self.vm, ticket)?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::pause"
            | "reel::resume"
            | "reel::stop"
            | "reel::finish"
            | "reel::activity::await" => {
                if results::kind(self.vm.heap(), self.vm.host_arguments(ticket)?[0]).ok()
                    == Some(results::Kind::Particle)
                {
                    self.control_particles(ticket, &symbol)?;
                    return Ok(NativeDispatch::Ready { work: 1 });
                }
                if media::is_media(self.vm.heap(), self.vm.host_arguments(ticket)?[0]) {
                    self.dispatch_media(ticket, &symbol)?;
                    return Ok(NativeDispatch::Ready { work: 1 });
                }
                let command = match symbol.as_str() {
                    "reel::pause" => activity::Command::Pause,
                    "reel::resume" => activity::Command::Resume,
                    "reel::stop" => activity::Command::Stop,
                    "reel::finish" => activity::Command::Finish,
                    _ => {
                        self.check_wait(session, ticket)?;
                        activity::Command::Await
                    }
                };
                let (done, work) = self.activities.command(
                    &mut self.vm,
                    &self.stage,
                    self.root_owner,
                    ticket,
                    command,
                    work,
                )?;
                if done {
                    self.issued = None;
                    Ok(NativeDispatch::Ready { work })
                } else {
                    Ok(NativeDispatch::Decoding { work })
                }
            }
            "reel::character" | "reel::character::line" => {
                if symbol == "reel::character" {
                    self.character(ticket)?;
                } else {
                    self.character_line(ticket)?;
                }
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::character::speak" => self.character_speak(session, ticket, locale, budget),
            "reel::speech::append" | "reel::speech::wait" | "reel::speech::revealed" => {
                let command = match symbol.as_str() {
                    "reel::speech::append" => dialogue::SpeechCommand::Append,
                    "reel::speech::wait" => dialogue::SpeechCommand::Wait,
                    _ => dialogue::SpeechCommand::Revealed,
                };
                self.dispatch_speech(session, ticket, command, budget)
            }
            "reel::unbind" => {
                let reference = self.vm.host_arguments(ticket)?[0];
                let value = self.vm.heap_mut().unbind_property(reference)?;
                self.vm.respond(ticket, HostResponse::Ready(value))?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::say" => self.dispatch_say(session, ticket, locale, budget),
            "reel::game::record" => self.record_history(session, ticket, locale, budget),
            "reel::narrator" => self.dispatch_narrator(session, ticket, locale, budget),
            stage::add::SYMBOL
            | stage::remove::SYMBOL
            | stage::clear::SYMBOL
            | stage::move_child::SYMBOL => {
                let invoke = match symbol.as_str() {
                    stage::add::SYMBOL => stage::add::invoke,
                    stage::remove::SYMBOL => stage::remove::invoke,
                    stage::move_child::SYMBOL => stage::move_child::invoke,
                    _ => stage::clear::invoke,
                };
                invoke(&mut self.stage, &mut self.vm, ticket)?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            stage::input::binding::SYMBOL | stage::input::value::SYMBOL => {
                let invoke = if symbol == stage::input::binding::SYMBOL {
                    stage::input::binding::invoke
                } else {
                    stage::input::value::invoke
                };
                invoke(&mut self.stage, &mut self.vm, ticket)?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::choose" => self.dispatch_choose(session, ticket, work),
            "reel::text_input::select_bytes"
            | "reel::text_input::select_all"
            | "reel::text_input::undo"
            | "reel::text_input::redo"
            | stage::text_input::filter::SYMBOL => {
                let name = symbol.rsplit("::").next().unwrap();
                let (progress, change) = stage::text_input::dispatch(
                    &mut self.stage,
                    &mut self.vm,
                    ticket,
                    name,
                    budget,
                )?;
                self.issued = None;
                if let Some((node, target, closure, value)) = change {
                    self.start_text_callback(session, node, target, closure, value)?;
                }
                Ok(progress)
            }
            "reel::text"
            | "reel::text_input"
            | "reel::text_area"
            | "reel::slider"
            | "reel::toggle"
            | "reel::checkbox"
            | "reel::radio"
            | "reel::button"
            | "reel::input"
            | "reel::image"
            | "reel::video"
            | "reel::particles"
            | "reel::row"
            | "reel::column"
            | "reel::grid"
            | "reel::camera"
            | "reel::scroll"
            | "reel::popup"
            | "reel::stack"
            | "reel::cursor"
            | "reel::stage::replace" => {
                let arguments = self.vm.host_arguments(ticket)?.to_vec();
                let origin = self.stage_origin(ticket);
                let value = self.stage.dispatch(
                    &mut self.vm,
                    self.root_owner,
                    &symbol,
                    &arguments,
                    self.read_limit,
                    origin,
                )?;
                self.vm.respond(ticket, HostResponse::Ready(value))?;
                self.issued = None;
                Ok(NativeDispatch::Ready {
                    work: arguments.len() as u32,
                })
            }
            _ => {
                let schema = stage::configure::schema(&symbol)
                    .ok_or(AdapterError::Input("unregistered session native"))?;
                let progress = stage::configure::runtime::dispatch(
                    &self.stage,
                    &mut self.vm,
                    ticket,
                    schema,
                    budget,
                )?;
                self.issued = None;
                Ok(match progress {
                    stage::configure::runtime::Progress::Complete { work } => {
                        NativeDispatch::Ready { work }
                    }
                    stage::configure::runtime::Progress::Continue {
                        work,
                        required_work,
                    } => NativeDispatch::BudgetYield {
                        work,
                        required_work,
                    },
                })
            }
        }
    }

    fn stage_origin(&self, ticket: HostTicket) -> Option<String> {
        let p = self.vm.program().package();
        ticket.location.source.map(|span| {
            let source = &p.sources[span.file.index()];
            let before = &source.text[..span.start as usize];
            let line = before.bytes().filter(|b| *b == b'\n').count() + 1;
            let column = before.rsplit('\n').next().unwrap().chars().count() + 1;
            format!("{}:{line}:{column}", p.symbols[source.path.index()])
        })
    }

    fn dispatch_choose(
        &mut self,
        session: &mut Session,
        ticket: HostTicket,
        budget: u32,
    ) -> Result<NativeDispatch, AdapterError> {
        self.check_story_wait(session, ticket)?;
        let mut reader = if let Some(reader) = self.choice_reader.take() {
            reader
        } else {
            let args = self.vm.host_arguments(ticket)?;
            let [options] = args else {
                return Err(AdapterError::Input("choose arity"));
            };
            let options = *options; // end the VM argument borrow before root insertion
            choice::ChoiceReader::new(self.vm.heap_mut(), options, self.root_owner)?
        };
        match reader.step(self.vm.heap_mut(), budget) {
            Ok((false, work)) => {
                self.choice_reader = Some(reader);
                Ok(NativeDispatch::Decoding { work })
            }
            Ok((true, work)) => {
                let result = self.publish_choice(session, ticket, &reader.rows);
                reader.release(self.vm.heap_mut())?;
                result.map(|interaction| NativeDispatch::Published { interaction, work })
            }
            Err(error) => {
                reader.release(self.vm.heap_mut())?;
                Err(error)
            }
        }
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
        self.main_minimum = SliceMinimum::default();
        self.main_wait = None;
        Ok(task)
    }

    /// Preserve BudgetYield and ResourceBlocked verbatim; neither is terminal.
    /// Pending input is routed before the next slice, never to vm.task_id().
    pub fn step(
        &mut self,
        session: &mut Session,
        budget: SessionBudget,
        now: impl FnMut() -> u64,
    ) -> Result<SessionStep, AdapterError> {
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
            CompletionOutcome::Advanced if pending.keys.is_none() => Value::NULL,
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
