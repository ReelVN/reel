//! Bounded session/VM coordination with trusted native admission.
//!
//! A player drives one bounded slice per turn and dispatches the returned native
//! ticket through its trusted registry. The adapter owns the main TaskId, each
//! wait association and all independently retained result/key roots. It never
//! interprets bytecode or assumes a table's private storage layout.
mod activity;
mod audio;
pub use activity::ActivityProgress;
mod callbacks;
mod game;
pub use game::GameSettings;
mod choice;
mod dialogue;
mod lists;
mod localization;
mod narrative;
mod observable;
mod publication;
mod registry;
mod results;
mod rich_text;
pub mod source;
mod stage;
#[cfg(test)]
mod tests;
mod text;
mod views;

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
    Markup(reel_core::text::MarkupError),
    Allocation,
    Stage(reel_ui::stage::StageError),
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
        write!(f, "{self:?}")
    }
}
impl std::error::Error for AdapterError {}
impl AdapterError {
    /// Language failures at the pending native's source location. Identity or
    /// internal ownership failures remain host errors and are not disguised.
    pub fn native_fault_kind(&self) -> Option<FaultKind> {
        match self {
            Self::Vm(VmError::Effect) => Some(FaultKind::Effect),
            Self::Input(_) | Self::Markup(_) => Some(FaultKind::Type),
            Self::Stage(_) => Some(FaultKind::Type),
            Self::Allocation | Self::Session(SessionError::AllocationFailed) => {
                Some(FaultKind::Resource)
            }
            Self::Session(
                SessionError::EmptyChoiceKey
                | SessionError::EmptyChoices
                | SessionError::DuplicateChoiceKey,
            ) => Some(FaultKind::Type),
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

struct RetainedKey {
    key: String,
    value: Value,
}
struct RetainedKeys {
    root: RootToken,
    entries: Vec<RetainedKey>,
}
struct PendingInteraction {
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
    main_wait: Option<WaitId>,
    callbacks: Vec<callbacks::Callback>,
    schedule_cursor: usize,
    derived_task: Option<TaskId>,
    reactive_ready: bool,
    feedback_only: bool,
    issued: Option<HostTicket>,
    pending: Option<PendingInteraction>,
    choice_reader: Option<choice::ChoiceReader>,
    dialogue_reader: Option<dialogue::DialogueReader>,
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
    audio: audio::Audio,
    translations: localization::Translations,
    game: std::sync::Arc<GameSettings>,
    narrative: Option<narrative::Narrative>,
    skip_input: Option<reel_ui::stage::NodeId>,
}

impl SessionVm {
    pub fn load(
        session: &Session,
        program: VerifiedProgram,
        limits: VmLimits,
    ) -> Result<Self, AdapterError> {
        let support = aside_host::runtime_support(
            program.package(),
            &registry::contracts(),
            &registry::capabilities(),
        )?;
        let identity = session.identity();
        let owner = session.root_owner();
        if !session.is_owner_live(owner) {
            return Err(AdapterError::Input("owner revoked"));
        }
        let vm = Vm::load(program, support, limits, SessionEpoch(identity.epoch.0))?;
        let stage = stage::StageBinding::new(vm.program())?;
        Ok(Self {
            vm,
            identity,
            owner,
            root_owner: RootOwner(owner.owner_id().0),
            task: None,
            main_wait: None,
            callbacks: Vec::new(),
            schedule_cursor: 0,
            derived_task: None,
            reactive_ready: true,
            feedback_only: false,
            issued: None,
            pending: None,
            choice_reader: None,
            dialogue_reader: None,
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
            audio: audio::Audio::default(),
            translations: localization::Translations::default(),
            game: std::sync::Arc::new(GameSettings::default()),
            narrative: None,
            skip_input: None,
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
                } else {
                    format!("<{:?}>", info.kind)
                }
            }
        })
    }
    pub fn task(&self) -> Option<TaskId> {
        self.task
    }
    pub fn stage_is_input_feedback(&self) -> bool {
        self.feedback_only
    }
    pub fn stage(&self) -> &reel_ui::stage::Stage {
        &self.stage.scene
    }

    /// Publish the properties of the same mounted objects after script work.
    pub fn sync_stage(&mut self, session: &Session) -> Result<(), AdapterError> {
        self.same_session(session)?;
        if self.reactive_ready && self.derived_task.is_none() {
            self.sync_narrative(session)?;
        }
        if self.vm.heap().derived_pending() || self.derived_task.is_some() {
            return Ok(());
        }
        self.stage.sync(self.vm.heap_mut(), self.read_limit)
    }
    /// Reactive factories must settle before a host acknowledges their narrative frame.
    pub fn stage_ready(&self) -> bool {
        !self.activities.pending()
            && self.derived_task.is_none()
            && !self.vm.heap().derived_pending()
    }
    pub fn cleanup_pending(&self) -> bool {
        self.revoked
            && (self.root_cursor < self.root_end
                || self.task.is_some()
                || !self.callbacks.is_empty()
                || self.derived_task.is_some()
                || self.audio.cleanup_pending())
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
        self.activities.set_skipping(session.reading().skipping());
        let mut progress = self.activities.advance(&mut self.vm, seconds, work)?;
        if self.reactive_ready && self.derived_task.is_none() {
            self.sync_narrative(session)?;
        }
        progress.woke |= self.vm.heap().derived_pending() || self.derived_task.is_some();
        Ok(progress)
    }

    pub fn activities_pending(&self) -> bool {
        !self.revoked && self.activities.pending()
    }

    pub fn activities_need_time(&self) -> bool {
        !self.revoked && self.activities.needs_time()
    }

    pub fn activities_poll_after(&self) -> Option<std::time::Duration> {
        (!self.revoked)
            .then(|| self.activities.poll_after())
            .flatten()
    }

    pub fn dispatch(
        &mut self,
        session: &mut Session,
        ticket: HostTicket,
        locale: &str,
        work: u32,
    ) -> Result<NativeDispatch, AdapterError> {
        self.check_ticket(session, ticket)?;
        let p = self.vm.program().package();
        let symbol = p.symbols[p.natives[ticket.native.index()].symbol.index()].clone();
        match symbol.as_str() {
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
                if symbol == "reel::localization::load" {
                    self.check_wait(session, ticket)?;
                }
                self.dispatch_localization(ticket, &symbol)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::audio::play"
            | "reel::audio::volume"
            | "reel::audio::await"
            | "reel::audio::group_volume"
            | "reel::audio::pan"
            | "reel::audio::stop" => {
                if symbol == "reel::audio::await" {
                    self.check_wait(session, ticket)?;
                }
                self.dispatch_audio(ticket, &symbol)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::game::dialogue"
            | "reel::game::choice"
            | "reel::game::history"
            | "reel::game::reading"
            | "reel::game::set_auto"
            | "reel::game::set_skip"
            | "reel::game::pause_reading"
            | "reel::dialogue::clear"
            | "reel::choice::select" => {
                self.dispatch_narrative(session, ticket, &symbol)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::view" => {
                self.dispatch_view(ticket)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::completion"
            | "reel::completion::resolve"
            | "reel::completion::reject"
            | "reel::completion::cancel"
            | "reel::completion::await" => {
                if symbol == "reel::completion::await" {
                    self.check_wait(session, ticket)?;
                }
                self.dispatch_result(ticket, &symbol)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::list" => {
                self.dispatch_list(ticket)?;
                Ok(NativeDispatch::Ready { work: 1 })
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
            "reel::sequence" | "reel::parallel" => {
                let kind = if symbol == "reel::sequence" {
                    activity::action::Kind::Sequence
                } else {
                    activity::action::Kind::Parallel
                };
                let work = activity::action::compose(&mut self.vm, ticket, kind, work)?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work })
            }
            "reel::run" => {
                let args = self.vm.host_arguments(ticket)?;
                let [action, owner] = [args[0], args[1]];
                let owner = if owner.tag() == aside_vm::ValueTag::Null {
                    self.default_activity_owner(ticket.task)?
                } else {
                    owner
                };
                let (done, work) = self.activities.run(
                    &mut self.vm,
                    &self.stage,
                    self.root_owner,
                    ticket,
                    [action, owner],
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
                self.vm.respond(ticket, HostResponse::Ready(value))?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::pause"
            | "reel::resume"
            | "reel::stop"
            | "reel::finish"
            | "reel::activity::await" => {
                if audio::is_audio(self.vm.heap(), self.vm.host_arguments(ticket)?[0]) {
                    self.dispatch_audio(ticket, &symbol)?;
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
            "reel::character::speak" => self.character_speak(session, ticket, locale),
            "reel::unbind" => {
                let reference = self.vm.host_arguments(ticket)?[0];
                let value = self.vm.heap_mut().unbind_property(reference)?;
                self.vm.respond(ticket, HostResponse::Ready(value))?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::say" => self.dispatch_say(session, ticket, locale, work),
            "reel::narrator" => {
                self.dispatch_narrator(session, ticket, locale)
                    .map(|interaction| NativeDispatch::Published {
                        interaction,
                        work: 0,
                    })
            }
            stage::add::SYMBOL | stage::remove::SYMBOL | stage::clear::SYMBOL => {
                let invoke = match symbol.as_str() {
                    stage::add::SYMBOL => stage::add::invoke,
                    stage::remove::SYMBOL => stage::remove::invoke,
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
            "reel::text"
            | "reel::text_input"
            | "reel::slider"
            | "reel::toggle"
            | "reel::button"
            | "reel::input"
            | "reel::image"
            | "reel::row"
            | "reel::column"
            | "reel::scroll"
            | "reel::stack"
            | "reel::stage::replace" => {
                let arguments = self.vm.host_arguments(ticket)?.to_vec();
                let origin = if symbol == "reel::image" {
                    ticket.location.source.map(|span| {
                        let source = &p.sources[span.file.index()];
                        let before = &source.text[..span.start as usize];
                        let line = before.bytes().filter(|b| *b == b'\n').count() + 1;
                        let column = before.rsplit('\n').next().unwrap().chars().count() + 1;
                        format!("{}:{line}:{column}", p.symbols[source.path.index()])
                    })
                } else {
                    None
                };
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
            _ => Err(AdapterError::Input("unregistered session native")),
        }
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
        self.main_wait = None;
        Ok(task)
    }

    /// Preserve BudgetYield and ResourceBlocked verbatim; neither is terminal.
    /// Pending input is routed before the next slice, never to vm.task_id().
    pub fn step(
        &mut self,
        session: &mut Session,
        budget: SliceBudget,
        now: impl FnMut() -> u64,
    ) -> Result<SessionStep, AdapterError> {
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
            || !matches!(
                ticket.input,
                NativeInput::Entry
                    | NativeInput::Continue { .. }
                    | NativeInput::CallbackReturned
                    | NativeInput::CallbackFault
            )
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
        kind: aside_vm::executor::FaultKind,
    ) -> Result<(), AdapterError> {
        self.check_ticket(session, ticket)?;
        self.vm.respond_language_error(ticket, kind)?;
        self.activities
            .cancel_native(self.vm.heap_mut(), ticket.task)?;
        if let Some(reader) = self.choice_reader.take() {
            reader.release(self.vm.heap_mut())?;
        }
        if let Some(reader) = self.dialogue_reader.take() {
            reader.release(self.vm.heap_mut())?;
        }
        self.issued = None;
        Ok(())
    }

    /// ABI v1 narrator. Content identity is tied to module/code version and
    /// source byte span (or PC without a source map), never a localization key.
    pub fn dispatch_narrator(
        &mut self,
        session: &mut Session,
        ticket: HostTicket,
        locale: &str,
    ) -> Result<InteractionId, AdapterError> {
        self.check_story_wait(session, ticket)?;
        let p = self.vm.program().package();
        let native = &p.natives[ticket.native.index()];
        if p.symbols[native.symbol.index()] != "reel::narrator" {
            return Err(AdapterError::Input("not narrator"));
        }
        let arguments = self.vm.host_arguments(ticket)?;
        let [text] = arguments else {
            return Err(AdapterError::Input("narrator arity"));
        };
        let text = rich_text::read(self.vm.heap(), *text, self.read_limit)?;
        let content_id = self.dialogue_content_id(ticket);
        self.publish_say(
            session,
            ticket,
            &[DialogueLineInput {
                speaker_id: None,
                speaker_name: "",
                content_id: &content_id,
                text: (&text).into(),
            }],
            locale,
        )
    }

    fn dialogue_content_id(&self, ticket: HostTicket) -> String {
        let p = self.vm.program().package();
        let function = &p.functions[ticket.location.code.function.index()];
        let module = &p.symbols[p.modules[function.module.index()].path.index()];
        match ticket.location.source {
            Some(span) => format!(
                "source:{}:{}:v{}:file{}:{}..{}",
                module.len(),
                module,
                ticket.location.code.version,
                span.file.0,
                span.start,
                span.end
            ),
            None => format!(
                "source:{}:{}:v{}:fn{}:pc{}",
                module.len(),
                module,
                ticket.location.code.version,
                ticket.location.code.function.0,
                ticket.location.pc.0
            ),
        }
    }

    /// Publish already decoded ADV lines from a trusted native implementation.
    /// The host decoder must use the real author schema; this is not a script API.
    fn publish_say(
        &mut self,
        session: &mut Session,
        ticket: HostTicket,
        lines: &[DialogueLineInput<'_>],
        locale: &str,
    ) -> Result<InteractionId, AdapterError> {
        self.check_story_wait(session, ticket)?;
        let selected_locale = self.locale();
        let locale = selected_locale.as_deref().unwrap_or(locale);
        let publication =
            publication::Publication::stage(session, |s| s.begin_say(self.owner, lines, locale))?;
        let interaction = publication.id();
        self.accept_wait(session, ticket, publication, None)?;
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
            .callbacks
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
            || !self.callbacks.is_empty()
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
        for index in 0..self.callbacks.len() {
            self.cancel_callback(session, index)?;
        }
        for index in 0..session.owners.len() {
            let peer = session.owners[index].handle;
            if peer != self.owner {
                session
                    .revoke_owner_with_retirement(peer, OwnerRetirementRequirements::default())?;
            }
        }
        self.root_end = self.vm.heap().root_slots();
        self.audio.close();
        self.translations.close();
        self.results.clear();
        self.activities.revoke(self.vm.heap_mut())?;
        self.stage.revoke(self.vm.heap_mut())?;
        session.revoke_owner_with_retirement(
            self.owner,
            OwnerRetirementRequirements {
                root_slots_end: self.root_end as usize,
                tasks_pending: self.task.is_some() || self.derived_task.is_some(),
                external_resources_pending: self.audio.cleanup_pending(),
            },
        )?;
        self.revoked = true;
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
        budget: SliceBudget,
        now: impl FnMut() -> u64,
    ) -> Result<u32, AdapterError> {
        self.same_session(session)?;
        if !self.revoked {
            return Err(AdapterError::Input("owner not revoked"));
        }
        self.maintain_audio()?;
        if !self.audio.cleanup_pending() {
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
        if let Some(callback) = self.callbacks.first() {
            match self.vm.run_slice(callback.task, budget, now) {
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
            match self.vm.run_slice(task, budget, now) {
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
            match self.vm.run_slice(task, budget, now) {
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
        Ok(self.vm.gc_step(budget)?)
    }
}
