//! Character values and grouped ADV dialogue use the existing session wait.
use super::{
    AdapterError, NativeBudget, NativeDispatch, PendingInteractionKind, SessionVm, rich_text, text,
};
use crate::{DialogueLineInput, HistoryInput, Session};
use aside_vm::bytecode::{FunctionId, FunctionKind};
use aside_vm::executor::{
    Completion as VmCompletion, Continuation, FaultRetention, HostResponse, HostTicket, NativeInput,
};
use aside_vm::{Heap, HeapError, ObjectKind, RootKind, RootOwner, RootToken, Value, ValueTag};
mod resources;
mod voice;
pub(in crate::aside) use resources::{
    allocate_character_resource, character_resource_field, character_resource_header,
    connect_character_resource, finish_character_resource,
};
pub(super) use voice::voice_input;

pub(super) struct SayOptions {
    pub reveal_skippable: bool,
    pub wait: bool,
    pub voice: Option<crate::audio::AudioRequest>,
}

pub(super) fn describe(api: &mut aside_host::source::Api) {
    use aside_compiler::semantic::TypeHint;
    use aside_host::source::{Member, NamedType, named, nullable};
    use aside_vm::bytecode::Const;
    let speech = named("reel::types::Speech");
    let character = named("reel::types::Character");
    let line = named("reel::types::CharacterLine");
    let text = TypeHint::Union(vec![TypeHint::String, named("reel::types::RichText")]);
    let history = TypeHint::Union(vec![TypeHint::Bool, text.clone()]);
    api.refine(
        "reel::character",
        character.clone(),
        &[("id", nullable(TypeHint::String))],
    );
    api.refine(
        "reel::character::line",
        line,
        &[
            ("character", character.clone()),
            ("text", text.clone()),
            ("history", history.clone()),
        ],
    );
    for symbol in ["reel::narrator", "reel::character::speak", "reel::say"] {
        let parameters = if symbol == "reel::say" {
            vec![("lines", TypeHint::Array)]
        } else {
            vec![
                ("text", text.clone()),
                ("history", history.clone()),
                ("voice", nullable(TypeHint::String)),
                ("voice_volume", aside_host::source::number()),
            ]
        };
        api.refine(symbol, nullable(speech.clone()), &parameters);
        api.native_mut(symbol).result_cases(&[
            ("wait", Const::Bool(false), speech.clone()),
            ("wait", Const::Bool(true), TypeHint::Null),
        ]);
    }
    api.refine(
        "reel::character::speak",
        nullable(speech.clone()),
        &[("character", character)],
    );
    api.refine(
        "reel::speech::append",
        speech.clone(),
        &[
            ("speech", speech.clone()),
            ("text", text),
            ("history", history),
        ],
    );
    for method in ["wait", "revealed"] {
        api.refine(
            &format!("reel::speech::{method}"),
            speech.clone(),
            &[("speech", speech.clone())],
        );
    }
    api.types.push(NamedType::new(
        "reel::types::Speech",
        "A submitted dialogue interaction. Chain text, playback waits and advancement on the same speech.",
        ["append", "wait", "revealed"]
            .into_iter()
            .map(|method| {
                Member::method(
                    api.native(&format!("reel::speech::{method}"))
                        .clone()
                        .bound(method),
                )
            })
            .collect(),
    ));
    let members = vec![
        Member::field(
            "reel::types::Character",
            "id",
            TypeHint::String,
            None,
            "Stable character identity.",
            None,
        ),
        Member::field(
            "reel::types::Character",
            "name",
            TypeHint::String,
            None,
            "Displayed speaker name.",
            None,
        ),
        Member::method(api.native("reel::character::line").clone().bound("line")),
    ];
    let mut character = NamedType::new(
        "reel::types::Character",
        "A named dialogue speaker. Call it to submit speech, or compose a line for say.",
        members,
    );
    character.call = Some(api.native("reel::character::speak").clone().bound("speak"));
    api.types.push(character);
    api.types.push(NamedType::new(
        "reel::types::CharacterLine",
        "One immutable character line composed for say.",
        Vec::new(),
    ));
}

pub(super) enum SpeechCommand {
    Append,
    Wait,
    Revealed,
}

const SPEECH_TAG: i64 = -1103;
const CHARACTER_TAG: i64 = -1104;

pub(in crate::aside) fn speech_resource_header(
    heap: &Heap,
    value: Value,
) -> Option<super::data::LogicalReference> {
    (heap.host_payload_len(value).ok() == Some(2)
        && heap.host_payload(value, 0).ok()?.as_int() == Some(SPEECH_TAG))
    .then(|| super::data::LogicalReference {
        payload: Vec::new(),
        kind: "reel.speech".into(),
        version: 1,
        fields: 1,
    })
}

pub(in crate::aside) fn speech_resource_field(
    heap: &Heap,
    value: Value,
    index: u32,
) -> Result<(std::borrow::Cow<'static, str>, Value), super::data::TypedDataError> {
    if index != 0 {
        return Err(super::data::TypedDataError::Contract(
            "invalid Speech field",
        ));
    }
    Ok(("generation".into(), heap.host_payload(value, 1)?))
}

pub(in crate::aside) fn allocate_speech_resource(
    heap: &mut Heap,
) -> Result<Value, super::data::TypedDataError> {
    let fields = heap.alloc_table()?;
    let value = heap.alloc_host(
        fields,
        &[Value::int(SPEECH_TAG), Value::NULL],
        Some(&IMMUTABLE),
    )?;
    let kind = heap.host_type("reel::types::Speech")?;
    heap.set_host_type(value, kind)?;
    Ok(value)
}

pub(in crate::aside) fn connect_speech_resource(
    heap: &mut Heap,
    value: Value,
    name: &str,
    field: Value,
) -> Result<(), super::data::TypedDataError> {
    if name != "generation" {
        return Err(super::data::TypedDataError::Contract(
            "unknown Speech field",
        ));
    }
    heap.set_host_payload(value, 1, field)?;
    Ok(())
}

pub(in crate::aside) fn finish_speech_resource(
    heap: &Heap,
    value: Value,
) -> Result<(), super::data::TypedDataError> {
    if heap.host_payload(value, 1)?.as_int().is_none() {
        return Err(super::data::TypedDataError::Contract(
            "Speech generation must be an integer",
        ));
    }
    Ok(())
}

pub(super) static IMMUTABLE: aside_vm::HostPropertyValidator =
    aside_vm::HostPropertyValidator::new("reel.character", 1, immutable);

pub(super) fn immutable(
    _: &aside_vm::PropertyRead<'_>,
    _: Value,
    _: u32,
    _: Value,
) -> Result<(), HeapError> {
    Err(HeapError::ReadOnly)
}

fn character_values(heap: &Heap, value: Value) -> Result<[Value; 2], AdapterError> {
    if character_resource_header(heap, value).is_none() {
        return Err(AdapterError::Input("expected a character"));
    }
    Ok([heap.host_payload(value, 1)?, heap.host_payload(value, 2)?])
}

pub(super) enum HistoryText {
    Display,
    Omit,
    Text(reel_core::text::Text),
}
impl HistoryText {
    pub(super) fn read(heap: &Heap, value: Value, chunk: usize) -> Result<Self, AdapterError> {
        Ok(match value.as_bool() {
            Some(true) => Self::Display,
            Some(false) => Self::Omit,
            None => Self::Text(rich_text::read(heap, value, chunk)?),
        })
    }
    pub(super) fn input(&self) -> HistoryInput<'_> {
        match self {
            Self::Display => HistoryInput::Display,
            Self::Omit => HistoryInput::Omit,
            Self::Text(text) => HistoryInput::Text(text.into()),
        }
    }
}

impl SessionVm {
    pub(super) fn speech_handle(
        &mut self,
        id: crate::InteractionId,
    ) -> Result<Value, AdapterError> {
        #[cfg(test)]
        let objects = self.vm.heap().stats().live_objects;
        let fields = self.vm.heap_mut().alloc_table()?;
        let value = self.vm.heap_mut().alloc_host(
            fields,
            &[Value::int(SPEECH_TAG), Value::int(id.generation() as i64)],
            Some(&IMMUTABLE),
        )?;
        self.stage
            .types
            .mark(self.vm.heap_mut(), value, "reel::types::Speech")?;
        #[cfg(test)]
        {
            self.speech_costs[0] += 1;
            self.speech_costs[1] += (self.vm.heap().stats().live_objects - objects) as usize;
        }
        Ok(value)
    }

    pub(super) fn dispatch_speech(
        &mut self,
        session: &mut Session,
        ticket: HostTicket,
        command: SpeechCommand,
        budget: NativeBudget,
    ) -> Result<NativeDispatch, AdapterError> {
        if matches!(ticket.input, NativeInput::WaitingFault) {
            let fault = self.vm.host_callback_fault(ticket)?.unwrap();
            self.vm
                .respond(ticket, HostResponse::ForwardFault(fault.id))?;
            self.issued = None;
            return Ok(NativeDispatch::Ready { work: 1 });
        }
        self.check_story_wait(session, ticket)?;
        let identity = if matches!(ticket.input, NativeInput::Entry)
            && matches!(command, SpeechCommand::Append)
        {
            match self.prepare_content_id(ticket, budget)? {
                super::content_identity::Preparation::Ready { id, work } => Some((id, work)),
                super::content_identity::Preparation::BudgetYield { required_work } => {
                    return Ok(NativeDispatch::BudgetYield {
                        work: 0,
                        required_work,
                    });
                }
            }
        } else {
            None
        };
        let args = self.vm.host_arguments(ticket)?;
        let heap = self.vm.heap();
        if speech_resource_header(heap, args[0]).is_none() {
            return Err(AdapterError::Input("expected a speech handle"));
        }
        let generation = heap
            .host_payload(args[0], 1)?
            .as_int()
            .ok_or(AdapterError::Input("invalid speech handle"))? as u64;
        let id = crate::InteractionId {
            identity: session.identity(),
            generation,
        };
        let receiver = args[0];
        if matches!(ticket.input, NativeInput::WaitingReturned) {
            // The original argument remains rooted by this native frame while the
            // interaction waits. Returning it preserves identity without a host root.
            self.vm.respond(ticket, HostResponse::Ready(receiver))?;
            self.issued = None;
            return Ok(NativeDispatch::Ready { work: 1 });
        }
        session.open_dialogue(id)?;
        if matches!(ticket.input, NativeInput::Entry) && matches!(command, SpeechCommand::Wait) {
            if session.reading_data.completion.is_some() {
                return Err(crate::SessionError::CompletionPending.into());
            }
            let wait = ticket.wait_id();
            self.vm.respond(ticket, HostResponse::Pending(wait))?;
            session.wait_dialogue(self.owner, id)?;
            self.pending = Some(super::PendingInteraction {
                interaction: id,
                task: ticket.task,
                wait,
                keys: None,
                kind: PendingInteractionKind::Advance,
                return_value: Some(receiver),
            });
            self.feedback_only = false;
            self.issued = None;
            return Ok(NativeDispatch::Published {
                interaction: id,
                work: 1,
            });
        }
        if matches!(ticket.input, NativeInput::Entry) {
            let wait = !matches!(command, SpeechCommand::Append)
                || args[3]
                    .as_bool()
                    .ok_or(AdapterError::Input("wait must be bool"))?;
            if wait && session.reading_data.completion.is_some() {
                return Err(crate::SessionError::CompletionPending.into());
            }
            if matches!(command, SpeechCommand::Append) {
                let text = rich_text::read(heap, args[1], self.read_limit)?;
                let history = HistoryText::read(heap, args[2], self.read_limit)?;
                let content_id = &identity.as_ref().expect("append identity admitted").0;
                session.append_dialogue(self.owner, id, &text, content_id, history.input())?;
                self.sync_narrative(session)?;
            }
            if !wait {
                self.vm.respond(ticket, HostResponse::Ready(receiver))?;
            } else {
                // Continue once before waiting so the same frame can return its
                // receiver after completion, including across cancellation/save.
                self.vm.respond(
                    ticket,
                    HostResponse::Continue(Continuation {
                        version: 1,
                        stage: 0,
                        values: &[],
                        faults: FaultRetention::Preserve,
                    }),
                )?;
            }
            self.issued = None;
            return Ok(NativeDispatch::Ready {
                work: identity.map_or(1, |(_, work)| work),
            });
        }
        if !matches!(ticket.input, NativeInput::Continue { .. }) {
            return Err(AdapterError::Input("invalid speech continuation"));
        }
        let kind = if matches!(command, SpeechCommand::Revealed) {
            if session
                .open_dialogue(id)?
                .lines()
                .iter()
                .all(crate::DialogueLine::playback_complete)
            {
                self.vm.respond(ticket, HostResponse::Ready(receiver))?;
                self.issued = None;
                return Ok(NativeDispatch::Ready { work: 1 });
            }
            PendingInteractionKind::Reveal
        } else {
            PendingInteractionKind::Advance
        };
        let wait = ticket.wait_id();
        self.vm.respond(ticket, HostResponse::Pending(wait))?;
        if matches!(kind, PendingInteractionKind::Advance) {
            // The admission checks precede the VM wait. No scripts run between
            // publishing it and arming the already validated interaction.
            session.wait_dialogue(self.owner, id)?;
        }
        self.pending = Some(super::PendingInteraction {
            interaction: id,
            task: ticket.task,
            wait,
            keys: None,
            kind,
            return_value: None,
        });
        self.feedback_only = false;
        self.issued = None;
        Ok(NativeDispatch::Published {
            interaction: id,
            work: 1,
        })
    }

    /// Whether story interaction playback or input has a result for the next VM slice.
    /// A completed reveal observation is ready before its VM wait is settled.
    pub fn completion_pending(&self, session: &Session) -> bool {
        session.completion_pending() || self.speech_reveal_ready(session)
    }

    fn speech_reveal_ready(&self, session: &Session) -> bool {
        self.pending.as_ref().is_some_and(|pending| {
            matches!(pending.kind, PendingInteractionKind::Reveal)
                && session
                    .open_dialogue(pending.interaction)
                    .is_ok_and(|dialogue| {
                        dialogue
                            .lines()
                            .iter()
                            .all(crate::DialogueLine::playback_complete)
                    })
        })
    }

    pub(super) fn deliver_speech_reveal(&mut self, session: &Session) -> Result<(), AdapterError> {
        let pending = self.pending.as_ref().expect("pending speech reveal");
        self.live(session)?;
        if self.task != Some(pending.task) {
            return Err(AdapterError::Input("speech reveal task mismatch"));
        }
        session.open_dialogue(pending.interaction)?;
        if self.speech_reveal_ready(session) {
            // Reveal is a playback observation. It does not arm advancement,
            // consume the session completion or record a read checkpoint.
            self.vm
                .complete(pending.wait, VmCompletion::Ready(Value::NULL))?;
            self.pending = None;
        }
        Ok(())
    }

    pub(super) fn record_history(
        &mut self,
        session: &mut Session,
        ticket: HostTicket,
        locale: &str,
        budget: NativeBudget,
    ) -> Result<NativeDispatch, AdapterError> {
        let (content_id, work) = match self.prepare_content_id(ticket, budget)? {
            super::content_identity::Preparation::Ready { id, work } => (id, work),
            super::content_identity::Preparation::BudgetYield { required_work } => {
                return Ok(NativeDispatch::BudgetYield {
                    work: 0,
                    required_work,
                });
            }
        };
        let args = self.vm.host_arguments(ticket)?;
        let text = rich_text::read(self.vm.heap(), args[0], self.read_limit)?;
        let speaker = if args[1].tag() == ValueTag::Null {
            None
        } else {
            let [id, name] = character_values(self.vm.heap(), args[1])?;
            Some((
                text::read_string(self.vm.heap(), id, self.read_limit)?,
                text::read_string(self.vm.heap(), name, self.read_limit)?,
            ))
        };
        let locale = self.locale().unwrap_or_else(|| locale.into());
        session.record_history(
            &[DialogueLineInput {
                speaker_id: speaker.as_ref().map(|s| s.0.as_str()),
                speaker_name: speaker.as_ref().map_or("", |s| s.1.as_str()),
                content_id: &content_id,
                text: (&text).into(),
                history: HistoryInput::Display,
            }],
            &locale,
        )?;
        // A script can already hold game.history() and read it immediately
        // after record returns, before yielding to a frame or another service.
        self.sync_narrative(session)?;
        self.vm.respond(ticket, HostResponse::Ready(Value::NULL))?;
        self.issued = None;
        Ok(NativeDispatch::Ready { work })
    }

    pub(super) fn character(&mut self, ticket: HostTicket) -> Result<(), AdapterError> {
        let args = self.vm.host_arguments(ticket)?;
        let name = args[0];
        let id = if args[1].tag() == ValueTag::Null {
            name
        } else {
            args[1]
        };
        let info = self.vm.heap().info(id)?;
        if info.kind != ObjectKind::String || info.len == 0 {
            return Err(AdapterError::Input(
                "character id must be a nonempty string",
            ));
        }
        let method = |symbol: &str| -> Result<FunctionId, AdapterError> {
            let p = self.vm.program().package();
            p.functions
                .iter()
                .enumerate()
                .find_map(|(i, f)| match f.kind {
                    FunctionKind::Native(n)
                        if p.symbols[p.natives[n.index()].symbol.index()] == symbol =>
                    {
                        Some(FunctionId(i as u32))
                    }
                    _ => None,
                })
                .ok_or(AdapterError::Input("character callable missing"))
        };
        let call = method("reel::character::speak")?;
        let heap = self.vm.heap_mut();
        let value = allocate_character_resource(heap)?;
        connect_character_resource(heap, value, "id", id)?;
        connect_character_resource(heap, value, "name", name)?;
        self.vm.bind_callable(value, call)?;
        self.vm.respond(ticket, HostResponse::Ready(value))?;
        self.issued = None;
        Ok(())
    }

    pub(super) fn character_line(&mut self, ticket: HostTicket) -> Result<(), AdapterError> {
        let args = self.vm.host_arguments(ticket)?;
        let [id, name] = character_values(self.vm.heap(), args[0])?;
        let text = args[1];
        rich_text::source(self.vm.heap(), text)?;
        let history = args[2];
        if history.as_bool().is_none() {
            rich_text::source(self.vm.heap(), history)?;
        }
        let heap = self.vm.heap_mut();
        let fields = heap.alloc_table()?;
        let value = heap.alloc_host(fields, &[id, name, text, history], Some(&IMMUTABLE))?;
        self.stage
            .types
            .mark(heap, value, "reel::types::CharacterLine")?;
        self.vm.respond(ticket, HostResponse::Ready(value))?;
        self.issued = None;
        Ok(())
    }

    pub(super) fn character_speak(
        &mut self,
        session: &mut Session,
        ticket: HostTicket,
        locale: &str,
        budget: NativeBudget,
    ) -> Result<NativeDispatch, AdapterError> {
        self.check_story_wait(session, ticket)?;
        let (content_id, work) = match self.prepare_content_id(ticket, budget)? {
            super::content_identity::Preparation::Ready { id, work } => (id, work),
            super::content_identity::Preparation::BudgetYield { required_work } => {
                return Ok(NativeDispatch::BudgetYield {
                    work: 0,
                    required_work,
                });
            }
        };
        let args = self.vm.host_arguments(ticket)?;
        let [id, name] = character_values(self.vm.heap(), args[0])?;
        let read = |value| text::read_string(self.vm.heap(), value, self.read_limit);
        let (id, name) = (read(id)?, read(name)?);
        let text = rich_text::read(self.vm.heap(), args[1], self.read_limit)?;
        let history = HistoryText::read(self.vm.heap(), args[2], self.read_limit)?;
        let reveal_skippable = args[3]
            .as_bool()
            .ok_or(AdapterError::Input("reveal_skippable must be bool"))?;
        let wait = args[4]
            .as_bool()
            .ok_or(AdapterError::Input("wait must be bool"))?;
        let voice = voice_input(self.vm.heap(), args[5], args[6], self.read_limit)?;
        let interaction = self.publish_say(
            session,
            ticket,
            &[DialogueLineInput {
                history: history.input(),
                speaker_id: Some(&id),
                speaker_name: &name,
                content_id: &content_id,
                text: (&text).into(),
            }],
            locale,
            SayOptions {
                reveal_skippable,
                wait,
                voice,
            },
        )?;
        Ok(NativeDispatch::Published { interaction, work })
    }

    pub(super) fn dispatch_say(
        &mut self,
        session: &mut Session,
        ticket: HostTicket,
        locale: &str,
        budget: NativeBudget,
    ) -> Result<NativeDispatch, AdapterError> {
        self.check_story_wait(session, ticket)?;
        // Reserve the path hash before decoding. Even an already complete
        // reader is retained unchanged when the caller supplies too little work.
        let identity_work = self.content_id_required_work(ticket)?;
        let required_work = identity_work.saturating_add(1);
        let budget = budget.with_minimum(required_work);
        if budget < required_work {
            return Ok(NativeDispatch::BudgetYield {
                work: 0,
                required_work,
            });
        }
        let reveal_skippable = self.vm.host_arguments(ticket)?[1]
            .as_bool()
            .ok_or(AdapterError::Input("reveal_skippable must be bool"))?;
        let wait = self.vm.host_arguments(ticket)?[2]
            .as_bool()
            .ok_or(AdapterError::Input("wait must be bool"))?;
        let mut reader = if let Some(reader) = self.dialogue_reader.take() {
            reader
        } else {
            let lines = self.vm.host_arguments(ticket)?[0];
            DialogueReader::new(self.vm.heap_mut(), lines, self.root_owner)?
        };
        match reader.step(self.vm.heap_mut(), budget - identity_work, self.read_limit) {
            Ok((false, work)) => {
                self.dialogue_reader = Some(reader);
                Ok(NativeDispatch::Decoding { work })
            }
            Ok((true, work)) => {
                let super::content_identity::Preparation::Ready {
                    id: base,
                    work: id_work,
                } = self.prepare_content_id(ticket, NativeBudget::Exact(identity_work))?
                else {
                    unreachable!("the unchanged caller path was admitted before decoding");
                };
                let ids: Vec<_> = (0..reader.rows.len())
                    .map(|i| format!("{base}:line{i}"))
                    .collect();
                let inputs: Vec<_> = reader
                    .rows
                    .iter()
                    .zip(&ids)
                    .map(|(row, id)| DialogueLineInput {
                        history: row.3.input(),
                        speaker_id: Some(&row.0),
                        speaker_name: &row.1,
                        text: (&row.2).into(),
                        content_id: id,
                    })
                    .collect();
                let result = self.publish_say(
                    session,
                    ticket,
                    &inputs,
                    locale,
                    SayOptions {
                        reveal_skippable,
                        wait,
                        voice: None,
                    },
                );
                reader.release(self.vm.heap_mut())?;
                result.map(|interaction| NativeDispatch::Published {
                    interaction,
                    work: work + id_work,
                })
            }
            Err(error) => {
                reader.release(self.vm.heap_mut())?;
                Err(error)
            }
        }
    }
}

pub(super) struct DialogueReader {
    array: Value,
    version: u64,
    count: u32,
    root: RootToken,
    roots: Value,
    current: Option<[Value; 4]>,
    field: usize,
    bytes: Vec<u8>,
    row: [String; 4],
    rows: Vec<(String, String, reel_core::text::Text, HistoryText)>,
    rich: [bool; 2],
}
impl DialogueReader {
    fn new(heap: &mut Heap, array: Value, owner: RootOwner) -> Result<Self, AdapterError> {
        let info = heap.info(array)?;
        if info.kind != ObjectKind::Array || heap.sequence_len(array)? == 0 {
            return Err(AdapterError::Input(
                "say requires a nonempty array of character lines",
            ));
        }
        let count = heap.sequence_len(array)?;
        let roots = heap.alloc_array(&[array, Value::NULL])?;
        let root = heap.add_root(owner, RootKind::Host, roots)?;
        Ok(Self {
            array,
            version: info.content_version,
            count,
            root,
            roots,
            current: None,
            field: 0,
            bytes: Vec::new(),
            row: Default::default(),
            rows: Vec::new(),
            rich: [false; 2],
        })
    }
    fn step(
        &mut self,
        heap: &mut Heap,
        budget: u32,
        chunk: usize,
    ) -> Result<(bool, u32), AdapterError> {
        if chunk == 0 {
            return Err(AdapterError::Input("string read chunk must be nonzero"));
        }
        if heap.info(self.array)?.content_version != self.version {
            return Err(HeapError::ConcurrentModification.into());
        }
        let mut work = 0;
        while work < budget && self.rows.len() < self.count as usize {
            if self.current.is_none() {
                let line = heap.sequence_get(self.array, self.rows.len() as u32)?;
                if heap.host_payload_len(line)? != 4 {
                    return Err(AdapterError::Input(
                        "say expects character.line(text) values",
                    ));
                }
                let mut values = [
                    heap.host_payload(line, 0)?,
                    heap.host_payload(line, 1)?,
                    heap.host_payload(line, 2)?,
                    heap.host_payload(line, 3)?,
                ];
                for (index, value) in values.iter_mut().enumerate().skip(2) {
                    if index == 3 && value.as_bool().is_some() {
                        continue;
                    }
                    let (source, rich) = rich_text::source(heap, *value)?;
                    *value = source;
                    self.rich[index - 2] = rich;
                }
                for value in &values[..2] {
                    if heap.info(*value)?.kind != ObjectKind::String {
                        return Err(AdapterError::Input("invalid dialogue line"));
                    }
                }
                heap.set_field(self.roots, 1, line)?;
                self.current = Some(values);
                work += 1;
                continue;
            }
            let value = self.current.unwrap()[self.field];
            let len = if value.as_bool().is_some() {
                0
            } else {
                heap.info(value)?.len as usize
            };
            if self.bytes.len() < len {
                let mut bytes = [0; 256];
                let count = (len - self.bytes.len())
                    .min(bytes.len())
                    .min(chunk)
                    .min((budget - work) as usize);
                let count = heap.read_bytes(value, self.bytes.len() as u32, &mut bytes[..count])?;
                self.bytes
                    .try_reserve(count)
                    .map_err(|_| AdapterError::Allocation)?;
                self.bytes.extend_from_slice(&bytes[..count]);
                work += count as u32;
            } else {
                self.row[self.field] = String::from_utf8(std::mem::take(&mut self.bytes))
                    .map_err(|_| AdapterError::Input("invalid UTF-8 string"))?;
                self.field += 1;
                work += 1;
                if self.field == 4 {
                    self.rows
                        .try_reserve(1)
                        .map_err(|_| AdapterError::Allocation)?;
                    let [id, name, text, history] = std::mem::take(&mut self.row);
                    let history = match value.as_bool() {
                        Some(true) => HistoryText::Display,
                        Some(false) => HistoryText::Omit,
                        None => HistoryText::Text(rich_text::decode(history, self.rich[1])?),
                    };
                    self.rows
                        .push((id, name, rich_text::decode(text, self.rich[0])?, history));
                    self.current = None;
                    self.field = 0;
                }
            }
        }
        Ok((self.rows.len() == self.count as usize, work))
    }
    pub fn release(self, heap: &mut Heap) -> Result<(), AdapterError> {
        heap.release_root(self.root)?;
        Ok(())
    }
}
