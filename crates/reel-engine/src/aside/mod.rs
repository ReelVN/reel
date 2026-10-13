//! Bounded session/VM coordination with trusted native admission.
//!
//! A player drives one bounded slice per turn and dispatches the returned native
//! ticket through its trusted registry. The adapter owns the main TaskId, each
//! wait association and all independently retained result/key roots. It never
//! interprets bytecode or assumes a table's private storage layout.
mod choice;
mod publication;
mod registry;
pub mod source;
mod stage;
#[cfg(test)]
mod tests;
mod text;

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
            Self::Input(_) => Some(FaultKind::Type),
            Self::Stage(_) => Some(FaultKind::Type),
            Self::Allocation
            | Self::Session(SessionError::Capacity(_) | SessionError::AllocationFailed) => {
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
            Self::Heap(HeapError::RetiredHost) => Some(FaultKind::InvalidState),
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
    root: RootToken,
}
struct PendingInteraction {
    interaction: InteractionId,
    task: TaskId,
    wait: WaitId,
    keys: Vec<RetainedKey>,
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

/// Owns a single session's main task. UI owners have no VM tasks in this adapter.
/// Native dispatch remains at the host boundary; tickets cannot select another
/// task or owner. No mutable VM escape hatch can bypass its revocation gates.
pub struct SessionVm {
    vm: Vm,
    identity: SessionIdentity,
    owner: OwnerHandle,
    root_owner: RootOwner,
    task: Option<TaskId>,
    issued: Option<HostTicket>,
    pending: Option<PendingInteraction>,
    choice_reader: Option<choice::ChoiceReader>,
    claimed: Option<Completion>,
    delivery: Option<DeliveryId>,
    result_root: Option<RootToken>,
    read_limit: usize,
    revoked: bool,
    root_cursor: u32,
    root_end: u32,
    stage: stage::StageBinding,
}

impl SessionVm {
    pub fn load(
        session: &Session,
        program: VerifiedProgram,
        limits: VmLimits,
    ) -> Result<Self, AdapterError> {
        let support = registry::runtime_support(
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
        Ok(Self {
            vm,
            identity,
            owner,
            root_owner: RootOwner(owner.owner_id().0),
            task: None,
            issued: None,
            pending: None,
            choice_reader: None,
            claimed: None,
            delivery: None,
            result_root: None,
            read_limit: limits.heap.max_mutation_bytes.min(4096) as usize,
            revoked: false,
            root_cursor: 0,
            root_end: 0,
            stage: stage::StageBinding::default(),
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
                    text::read_string(heap, value, info.len as usize, self.read_limit)?
                } else {
                    format!("<{:?}>", info.kind)
                }
            }
        })
    }
    pub fn task(&self) -> Option<TaskId> {
        self.task
    }
    pub fn stage(&self) -> &reel_ui::stage::Stage {
        &self.stage.scene
    }

    /// Publish the properties of the same mounted objects after script work.
    pub fn sync_stage(&mut self, session: &Session) -> Result<(), AdapterError> {
        self.same_session(session)?;
        self.stage.sync(self.vm.heap_mut(), self.read_limit)
    }
    pub fn cleanup_pending(&self) -> bool {
        self.revoked && (self.root_cursor < self.root_end || self.task.is_some())
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
            "reel::narrator" => {
                self.dispatch_narrator(session, ticket, locale)
                    .map(|interaction| NativeDispatch::Published {
                        interaction,
                        work: 0,
                    })
            }
            "reel::choose" => self.dispatch_choose(session, ticket, work),
            "reel::text"
            | "reel::row"
            | "reel::column"
            | "reel::stack"
            | "reel::stage::replace" => {
                let arguments = self.vm.host_arguments(ticket)?.to_vec();
                let value = self.stage.dispatch(
                    self.vm.heap_mut(),
                    self.root_owner,
                    &symbol,
                    &arguments,
                    self.read_limit,
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
        self.check_wait(session, ticket)?;
        let mut reader = if let Some(reader) = self.choice_reader.take() {
            reader
        } else {
            let args = self.vm.host_arguments(ticket)?;
            let [options] = args else {
                return Err(AdapterError::Input("choose arity"));
            };
            let options = *options; // end the VM argument borrow before root insertion
            choice::ChoiceReader::new(
                self.vm.heap_mut(),
                options,
                self.root_owner,
                session.limits().max_choices,
            )?
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
        Ok(task)
    }

    /// Preserve BudgetYield and ResourceBlocked verbatim; neither is terminal.
    /// Pending input is routed before the next slice, never to vm.task_id().
    pub fn step(
        &mut self,
        session: &mut Session,
        budget: SliceBudget,
        now: impl FnMut() -> u64,
    ) -> Result<RunOutcome, AdapterError> {
        self.live(session)?;
        self.deliver_completion(session)?;
        if let Some(ticket) = self.issued {
            return Ok(RunOutcome::HostCall(ticket));
        }
        if self.delivery.is_some() {
            return Err(AdapterError::Input("delivery not retired"));
        }
        let task = self.task.ok_or(AdapterError::Input("no main task"))?;
        let outcome = self.vm.run_slice(task, budget, now)?;
        match outcome {
            RunOutcome::HostCall(ticket) => self.issued = Some(ticket),
            RunOutcome::Returned(id) | RunOutcome::Fault(id) => self.delivery = Some(id),
            _ => {}
        }
        Ok(outcome)
    }

    fn check_ticket(&self, session: &Session, ticket: HostTicket) -> Result<(), AdapterError> {
        self.live(session)?;
        if self.issued != Some(ticket)
            || self.task != Some(ticket.task)
            || ticket.owner != self.root_owner
            || ticket.input != NativeInput::Entry
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
        if let Some(reader) = self.choice_reader.take() {
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
        self.check_wait(session, ticket)?;
        let p = self.vm.program().package();
        let native = &p.natives[ticket.native.index()];
        if p.symbols[native.symbol.index()] != "reel::narrator" {
            return Err(AdapterError::Input("not narrator"));
        }
        let arguments = self.vm.host_arguments(ticket)?;
        let [text] = arguments else {
            return Err(AdapterError::Input("narrator arity"));
        };
        let text = text::read_string(
            self.vm.heap(),
            *text,
            session.limits().max_interaction_text_bytes,
            self.read_limit,
        )?;
        let function = &p.functions[ticket.location.code.function.index()];
        let module = &p.symbols[p.modules[function.module.index()].path.index()];
        let content_id = match ticket.location.source {
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
        };
        self.publish_say(
            session,
            ticket,
            &[DialogueLineInput {
                speaker_id: None,
                speaker_name: "",
                content_id: &content_id,
                text: &text,
            }],
            locale,
        )
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
        self.check_wait(session, ticket)?;
        let publication =
            publication::Publication::stage(session, |s| s.begin_say(self.owner, lines, locale))?;
        let interaction = publication.id();
        self.accept_wait(session, ticket, publication, Vec::new())?;
        Ok(interaction)
    }

    /// Decoder output uses the original immutable key Values, not reconstructed
    /// strings. Each key is independently rooted before releasing native entry.
    fn publish_choice(
        &mut self,
        session: &mut Session,
        ticket: HostTicket,
        rows: &[(Value, Value)],
    ) -> Result<InteractionId, AdapterError> {
        self.check_wait(session, ticket)?;
        let limits = session.limits();
        if rows.len() > limits.max_choices {
            return Err(AdapterError::Input("session choice count limit"));
        }
        let mut decoded = Vec::new();
        decoded
            .try_reserve_exact(rows.len())
            .map_err(|_| AdapterError::Allocation)?;
        let mut remaining = limits.max_interaction_text_bytes;
        for &(key, label) in rows {
            let key = text::read_string(self.vm.heap(), key, remaining, self.read_limit)?;
            remaining -= key.len();
            let label = text::read_string(self.vm.heap(), label, remaining, self.read_limit)?;
            remaining -= label.len();
            decoded.push((key, label));
        }
        let mut keys = Vec::new();
        keys.try_reserve_exact(rows.len())
            .map_err(|_| AdapterError::Allocation)?;
        for (&(value, _), (key, _)) in rows.iter().zip(&decoded) {
            match self
                .vm
                .heap_mut()
                .add_root(self.root_owner, RootKind::Host, value)
            {
                Ok(root) => keys.push(RetainedKey {
                    key: key.clone(),
                    root,
                }),
                Err(error) => {
                    self.release_keys(keys)?;
                    return Err(error.into());
                }
            }
        }
        let inputs = decoded
            .iter()
            .map(|(key, label)| ChoiceInput { key, label })
            .collect::<Vec<_>>();
        let publication =
            match publication::Publication::stage(session, |s| s.begin_choice(self.owner, &inputs))
            {
                Ok(publication) => publication,
                Err(error) => {
                    self.release_keys(keys)?;
                    return Err(error.into());
                }
            };
        let interaction = publication.id();
        self.accept_wait(session, ticket, publication, keys)?;
        Ok(interaction)
    }

    fn accept_wait(
        &mut self,
        session: &mut Session,
        ticket: HostTicket,
        publication: publication::Publication,
        keys: Vec<RetainedKey>,
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
        publication.commit(session);
        self.issued = None;
        Ok(())
    }

    fn release_keys(&mut self, keys: Vec<RetainedKey>) -> Result<(), AdapterError> {
        for key in keys {
            self.vm.heap_mut().release_root(key.root)?;
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
            CompletionOutcome::Advanced if pending.keys.is_empty() => Value::NULL,
            CompletionOutcome::Selected(selected) => {
                let key = pending
                    .keys
                    .iter()
                    .find(|key| &key.key == selected)
                    .ok_or(AdapterError::Input("unknown choice key"))?;
                self.vm.heap().root_value(key.root)?
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
        self.task = None;
        Ok(self.vm.heap().root_value(root)?)
    }

    pub fn discard_delivery(&mut self, session: &Session) -> Result<(), AdapterError> {
        self.live(session)?;
        let delivery = self.delivery.ok_or(AdapterError::Input("no delivery"))?;
        self.vm.discard_delivery(delivery)?;
        self.delivery = None;
        self.task = None;
        Ok(())
    }

    /// End through the same retirement path as cancellation, retaining all
    /// declared cleanup obligations. A successful result must be claimed first.
    pub fn finish(&mut self, session: &mut Session) -> Result<(), AdapterError> {
        self.live(session)?;
        if self.task.is_some() || self.pending.is_some() || self.delivery.is_some() {
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
        // This adapter creates VM work only for the main owner. UI peer owners
        // belong to the pure Rust projection and have no VM/external resources.
        for index in 0..session.owners.len() {
            let peer = session.owners[index].handle;
            if peer != self.owner {
                session
                    .revoke_owner_with_retirement(peer, OwnerRetirementRequirements::default())?;
            }
        }
        self.root_end = self.vm.heap().root_slots();
        self.stage.revoke(self.vm.heap_mut())?;
        session.revoke_owner_with_retirement(
            self.owner,
            OwnerRetirementRequirements {
                root_slots_end: self.root_end as usize,
                tasks_pending: self.task.is_some(),
                external_resources_pending: false, // no host-owned RootRanges/resources
            },
        )?;
        self.revoked = true;
        self.issued = None;
        // Tokens are invalidated by the owner scan; never release them twice.
        self.pending = None;
        self.choice_reader = None;
        self.claimed = None;
        self.result_root = None;
        self.delivery = None;
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
