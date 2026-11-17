//! Install portable reading data and reconnect its operation at a proved point.
#[cfg(test)]
mod tests;
use super::CheckpointError;
use crate::aside::{
    AdapterError, NativeBudget, NativeDispatch, PendingInteraction, PendingInteractionKind,
    RetainedKey, RetainedKeys, SessionVm,
    choice::ChoiceReader,
    narrative::{Narrative, ReadingProjection},
};
use crate::{
    CompletionOutcome, Interaction, InteractionId, Session,
    reading::checkpoint::{ReadingInstall, SavedReading},
};
use aside_vm::{
    RootKind, RootToken, Value,
    executor::{HostResponse, HostTicket},
    resume::{ContinuationDescriptor, ResumeLimits},
};

pub(in crate::aside) struct ReadingResume {
    expected: ContinuationDescriptor,
    install: Option<ReadingInstall>,
    projection: ReadingProjection,
    installed: bool,
    binding_required: bool,
    ticket: Option<HostTicket>,
    choice: Option<ChoiceKeys>,
}

impl SessionVm {
    /// Move detached reading contents into the candidate before current Root execution.
    pub(crate) fn prepare_reading_resume(
        &mut self,
        saved: SavedReading,
        expected: ContinuationDescriptor,
    ) -> Result<(), CheckpointError> {
        if self.reading_resume.is_some() || self.pending.is_some() {
            return Err(CheckpointError::Contract(
                "reading restoration is already prepared",
            ));
        }
        let binding_required = saved.data.completion.is_some()
            || match &saved.data.active {
                Some(Interaction::Dialogue(dialogue)) => dialogue.awaiting_advance(),
                Some(Interaction::Choice(_)) => true,
                None => false,
            };
        self.reading_resume = Some(ReadingResume {
            expected,
            install: Some(saved.install_begin(self.identity)),
            projection: ReadingProjection::default(),
            installed: false,
            binding_required,
            ticket: None,
            choice: None,
        });
        Ok(())
    }

    /// Whether a restored reading operation still needs its current native wait.
    pub(crate) fn reading_resume_pending(&self) -> bool {
        self.reading_resume
            .as_ref()
            .is_some_and(|resume| resume.binding_required)
    }

    /// Install history, display and reveal progress into the Reading built by current Root.
    /// Each unit rebases one stored item or projects one bounded string segment.
    pub(crate) fn step_prepared_reading(
        &mut self,
        session: &mut Session,
        work: u32,
        chunk: usize,
    ) -> Result<bool, AdapterError> {
        self.same_session(session)?;
        let Some(mut resume) = self.reading_resume.take() else {
            return Ok(true);
        };
        let result = (|| {
            for _ in 0..work {
                if resume.installed {
                    return Ok(true);
                }
                if let Some(install) = &mut resume.install {
                    if install.step(1)? {
                        let page = resume.install.take().unwrap().finish(session)?;
                        if self.narrative.is_none() {
                            self.narrative = Some(Narrative::new(
                                &mut self.vm,
                                self.root_owner,
                                &self.stage.types,
                            )?);
                        }
                        self.narrative.as_mut().unwrap().page = page;
                    }
                } else {
                    resume.installed = resume.projection.step(
                        self.narrative.as_mut().unwrap(),
                        self.vm.heap_mut(),
                        session,
                        1,
                        chunk,
                    )?;
                }
            }
            Ok(resume.installed)
        })();
        if !resume.installed || resume.binding_required {
            self.reading_resume = Some(resume);
        }
        result
    }

    pub(in crate::aside) fn resume_reading_point(
        &mut self,
        session: &mut Session,
        ticket: HostTicket,
        symbol: &str,
        budget: NativeBudget,
    ) -> Result<Option<NativeDispatch>, AdapterError> {
        if !self.reading_resume_pending()
            || !matches!(
                symbol,
                "reel::narrator"
                    | "reel::character::speak"
                    | "reel::say"
                    | "reel::choose"
                    | "reel::speech::wait"
            )
        {
            return Ok(None);
        }
        let Some((task, boundary)) = self.flow_boundary() else {
            return Ok(None);
        };
        if ticket.task != task {
            return Ok(None);
        }
        self.check_story_wait(session, ticket)?;
        let mut work = 0;
        if self.reading_resume.as_ref().unwrap().ticket.is_none() {
            let cost = self
                .vm
                .resume_descriptor_bound(&self.resume, ticket)
                .map_err(|_| {
                    AdapterError::Input("restored reading requires a declared interaction point")
                })?;
            let required = u32::try_from(cost.work.saturating_add(cost.key_bytes))
                .map_err(|_| AdapterError::Input("reading point exceeds native work capacity"))?;
            if budget.with_minimum(required) < required {
                return Ok(Some(NativeDispatch::BudgetYield {
                    work: 0,
                    required_work: required,
                }));
            }
            let descriptor = self
                .vm
                .resume_descriptor(
                    &self.resume,
                    ticket,
                    boundary,
                    ResumeLimits {
                        work: cost.work,
                        ..Default::default()
                    },
                )
                .map_err(|_| {
                    AdapterError::Input("restored reading point has a different current call chain")
                })?;
            if !descriptor.matches(&self.reading_resume.as_ref().unwrap().expected) {
                return Err(AdapterError::Input(
                    "restored reading point differs from the saved continuation",
                ));
            }
            self.reading_resume.as_mut().unwrap().ticket = Some(ticket);
            work = required;
        } else if self.reading_resume.as_ref().unwrap().ticket != Some(ticket) {
            return Err(AdapterError::Input(
                "restored reading ticket changed during preparation",
            ));
        }
        let limit = budget.with_minimum(work.max(1));
        if limit == 0 {
            return Ok(Some(NativeDispatch::BudgetYield {
                work: 0,
                required_work: 1,
            }));
        }
        while work < limit {
            if !self.reading_resume.as_ref().unwrap().installed {
                self.step_prepared_reading(session, 1, self.read_limit)?;
                work += 1;
                continue;
            }
            let mut resume = self.reading_resume.take().unwrap();
            let result = self.bind_reading_point(session, ticket, symbol, &mut resume);
            work += 1;
            match result {
                Ok(Some(interaction)) => {
                    return Ok(Some(NativeDispatch::Published { interaction, work }));
                }
                Ok(None) => self.reading_resume = Some(resume),
                Err(error) => {
                    if let Some(choice) = resume.choice {
                        choice.cancel(self.vm.heap_mut())?;
                    }
                    return Err(error);
                }
            }
        }
        Ok(Some(NativeDispatch::Decoding { work }))
    }

    fn bind_reading_point(
        &mut self,
        session: &Session,
        ticket: HostTicket,
        symbol: &str,
        resume: &mut ReadingResume,
    ) -> Result<Option<InteractionId>, AdapterError> {
        let interaction = session
            .reading_data
            .active
            .as_ref()
            .map(Interaction::id)
            .or_else(|| {
                session
                    .reading_data
                    .completion
                    .as_ref()
                    .map(|completion| completion.interaction_id())
            })
            .ok_or(AdapterError::Input(
                "saved reading has no interaction to resume",
            ))?;
        let choice = matches!(session.reading_data.active, Some(Interaction::Choice(_)))
            || session
                .reading_data
                .completion
                .as_ref()
                .is_some_and(|completion| {
                    matches!(completion.outcome(), CompletionOutcome::Selected(_))
                });
        if choice != (symbol == "reel::choose") {
            return Err(AdapterError::Input(
                "current point operation differs from the saved reading interaction",
            ));
        }
        let keys = if choice {
            if resume.choice.is_none() {
                resume.choice = Some(ChoiceKeys::new(self, ticket)?);
                return Ok(None);
            }
            if !resume.choice.as_mut().unwrap().step(
                self.vm.heap_mut(),
                session,
                self.read_limit,
            )? {
                return Ok(None);
            }
            Some(resume.choice.take().unwrap().finish(self.vm.heap_mut())?)
        } else {
            None
        };
        let return_value = if symbol == "reel::speech::wait" {
            let receiver = self.vm.host_arguments(ticket)?[0];
            if crate::aside::dialogue::speech_resource_header(self.vm.heap(), receiver).is_none()
                || self
                    .vm
                    .heap()
                    .host_payload(receiver, 1)?
                    .as_int()
                    .map(|id| id as u64)
                    != Some(interaction.generation())
            {
                return Err(AdapterError::Input(
                    "restored Speech refers to a different interaction",
                ));
            }
            Some(receiver)
        } else {
            None
        };
        let wait = ticket.wait_id();
        if let Err(error) = self.vm.respond(ticket, HostResponse::Pending(wait)) {
            self.release_keys(keys)?;
            return Err(error.into());
        }
        self.pending = Some(PendingInteraction {
            kind: PendingInteractionKind::Advance,
            interaction,
            task: ticket.task,
            wait,
            keys,
            return_value,
        });
        self.feedback_only = false;
        self.issued = None;
        Ok(Some(interaction))
    }
}

/// Resolve saved business keys against the current choose arguments without publishing.
struct ChoiceKeys {
    reader: ChoiceReader,
    decoded: bool,
    root: Option<RootToken>,
    references: Value,
    entries: Vec<RetainedKey>,
    row: usize,
    candidate: usize,
    offset: usize,
    matched: Option<Value>,
    key: String,
    complete: bool,
}
impl ChoiceKeys {
    fn new(vm: &mut SessionVm, ticket: HostTicket) -> Result<Self, AdapterError> {
        let array = vm.vm.host_arguments(ticket)?[0];
        let reader = ChoiceReader::new(vm.vm.heap_mut(), array, vm.root_owner)?;
        let references = vm.vm.heap_mut().alloc_array(&[])?;
        let root = vm
            .vm
            .heap_mut()
            .add_root(vm.root_owner, RootKind::Host, references)?;
        Ok(Self {
            reader,
            decoded: false,
            root: Some(root),
            references,
            entries: Vec::new(),
            row: 0,
            candidate: 0,
            offset: 0,
            matched: None,
            key: String::new(),
            complete: false,
        })
    }
    fn step(
        &mut self,
        heap: &mut aside_vm::Heap,
        session: &Session,
        chunk: usize,
    ) -> Result<bool, AdapterError> {
        if !self.decoded {
            self.decoded = self.reader.step(heap, 1)?.0;
            return Ok(false);
        }
        let source = match &session.reading_data.active {
            Some(Interaction::Choice(choice)) => {
                choice.options().get(self.row).map(|option| option.key())
            }
            _ if self.row == 0 => session
                .reading_data
                .completion
                .as_ref()
                .and_then(|completion| match completion.outcome() {
                    CompletionOutcome::Selected(key) => Some(key.as_str()),
                    _ => None,
                }),
            _ => None,
        };
        let Some(source) = source else {
            self.complete = true;
            return Ok(true);
        };
        if let Some(value) = self.matched {
            let mut end = source.len().min(self.offset.saturating_add(chunk.max(4)));
            while !source.is_char_boundary(end) {
                end -= 1;
            }
            self.key
                .try_reserve(end - self.offset)
                .map_err(|_| AdapterError::Allocation)?;
            self.key.push_str(&source[self.offset..end]);
            self.offset = end;
            if end == source.len() {
                self.entries
                    .try_reserve(1)
                    .map_err(|_| AdapterError::Allocation)?;
                heap.array_push(self.references, value)?;
                self.entries.push(RetainedKey {
                    key: std::mem::take(&mut self.key),
                    value,
                });
                self.row += 1;
                self.candidate = 0;
                self.offset = 0;
                self.matched = None;
            }
            return Ok(false);
        }
        let value = self
            .reader
            .rows
            .get(self.candidate)
            .map(|row| row.0)
            .ok_or(AdapterError::Input(
                "saved choice key is absent from current choose options",
            ))?;
        if heap.info(value)?.len as usize != source.len() {
            self.candidate += 1;
            self.offset = 0;
            return Ok(false);
        }
        let mut bytes = [0; 256];
        let count = (source.len() - self.offset)
            .min(bytes.len())
            .min(chunk.max(1));
        let read = heap.read_bytes(value, self.offset as u32, &mut bytes[..count])?;
        if bytes[..read] != source.as_bytes()[self.offset..self.offset + read] {
            self.candidate += 1;
            self.offset = 0;
        } else {
            self.offset += read;
            if self.offset == source.len() {
                self.matched = Some(value);
                self.offset = 0;
            }
        }
        Ok(false)
    }
    fn finish(mut self, heap: &mut aside_vm::Heap) -> Result<RetainedKeys, AdapterError> {
        debug_assert!(self.complete);
        self.reader.release(heap)?;
        Ok(RetainedKeys {
            root: self.root.take().unwrap(),
            entries: self.entries,
        })
    }
    fn cancel(mut self, heap: &mut aside_vm::Heap) -> Result<(), AdapterError> {
        self.reader.release(heap)?;
        if let Some(root) = self.root.take() {
            heap.release_root(root)?;
        }
        Ok(())
    }
}
