//! Character values and grouped ADV dialogue use the existing session wait.
use super::{AdapterError, NativeDispatch, SessionVm, rich_text, text};
use crate::{DialogueLineInput, HistoryInput, Session};
use aside_vm::bytecode::{FunctionId, FunctionKind};
use aside_vm::executor::{HostResponse, HostTicket};
use aside_vm::{Heap, HeapError, ObjectKind, RootKind, RootOwner, RootToken, Value, ValueTag};

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
        line.clone(),
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
            vec![("text", text.clone()), ("history", history.clone())]
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
        &[("character", character.clone())],
    );
    api.refine(
        "reel::speech::append",
        TypeHint::Null,
        &[
            ("speech", speech.clone()),
            ("text", text),
            ("history", history),
        ],
    );
    api.refine("reel::speech::wait", TypeHint::Null, &[("speech", speech)]);
    api.types.push(NamedType::new(
        "reel::types::Speech",
        "A submitted dialogue interaction. Append text or wait for its shared advancement.",
        ["append", "wait"]
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

pub(super) fn immutable(_: &Heap, _: Value, _: u32, _: Value) -> Result<(), HeapError> {
    Err(HeapError::ReadOnly)
}

fn character_values(heap: &Heap, value: Value) -> Result<[Value; 2], AdapterError> {
    if heap.host_payload_len(value)? != 2 {
        return Err(AdapterError::Input("expected a character"));
    }
    Ok([heap.host_payload(value, 0)?, heap.host_payload(value, 1)?])
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
        let methods = if let Some(methods) = self.speech_methods {
            methods
        } else {
            let p = self.vm.program().package();
            let find = |symbol: &str| {
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
                    .ok_or(AdapterError::Input("speech method missing"))
            };
            let methods = [find("reel::speech::append")?, find("reel::speech::wait")?];
            self.speech_methods = Some(methods);
            #[cfg(test)]
            {
                self.speech_costs[0] += 1;
            }
            methods
        };
        let fields = self.vm.heap_mut().alloc_table()?;
        let value = self.vm.heap_mut().alloc_host(
            fields,
            &[Value::int(id.generation() as i64)],
            Some(immutable),
        )?;
        self.stage
            .types
            .mark(self.vm.heap_mut(), value, "reel::types::Speech")?;
        for (name, function) in ["append", "wait"].into_iter().zip(methods) {
            let method = self.vm.bind_native(value, function)?;
            let heap = self.vm.heap_mut();
            let key = heap.alloc_string(name)?;
            heap.table_store(fields, heap.table_len(fields)?, key, method)?;
        }
        #[cfg(test)]
        {
            self.speech_costs[1] += 1;
        }
        Ok(value)
    }

    pub(super) fn dispatch_speech(
        &mut self,
        session: &mut Session,
        ticket: HostTicket,
        append: bool,
    ) -> Result<NativeDispatch, AdapterError> {
        self.check_story_wait(session, ticket)?;
        let args = self.vm.host_arguments(ticket)?;
        let heap = self.vm.heap();
        if heap.host_payload_len(args[0])? != 1 {
            return Err(AdapterError::Input("expected a speech handle"));
        }
        let generation = heap
            .host_payload(args[0], 0)?
            .as_int()
            .ok_or(AdapterError::Input("invalid speech handle"))? as u64;
        let id = crate::InteractionId {
            identity: session.identity(),
            generation,
        };
        session.open_dialogue(id)?;
        let wait = !append
            || args[3]
                .as_bool()
                .ok_or(AdapterError::Input("wait must be bool"))?;
        if wait && session.completion.is_some() {
            return Err(crate::SessionError::CompletionPending.into());
        }
        if append {
            let text = rich_text::read(heap, args[1], self.read_limit)?;
            let history = HistoryText::read(heap, args[2], self.read_limit)?;
            let content_id = self.dialogue_content_id(ticket);
            session.append_dialogue(self.owner, id, &text, &content_id, history.input())?;
            self.sync_narrative(session)?;
        }
        if !wait {
            self.vm.respond(ticket, HostResponse::Ready(Value::NULL))?;
            self.issued = None;
            Ok(NativeDispatch::Ready { work: 1 })
        } else {
            // Validate admission before publishing the VM wait; no scripts run
            // between this response and arming the existing interaction.
            let wait = ticket.wait_id();
            self.vm.respond(ticket, HostResponse::Pending(wait))?;
            session.wait_dialogue(self.owner, id)?;
            self.pending = Some(super::PendingInteraction {
                interaction: id,
                task: ticket.task,
                wait,
                keys: None,
            });
            self.feedback_only = false;
            self.issued = None;
            Ok(NativeDispatch::Published {
                interaction: id,
                work: 1,
            })
        }
    }

    pub(super) fn record_history(
        &mut self,
        session: &mut Session,
        ticket: HostTicket,
        locale: &str,
    ) -> Result<(), AdapterError> {
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
        let content_id = self.dialogue_content_id(ticket);
        let record = session.prepare_history_record(
            &[DialogueLineInput {
                speaker_id: speaker.as_ref().map(|s| s.0.as_str()),
                speaker_name: speaker.as_ref().map_or("", |s| s.1.as_str()),
                content_id: &content_id,
                text: (&text).into(),
                history: HistoryInput::Display,
            }],
            &locale,
        )?;
        session.commit_history_record(record);
        // A script can already hold game.history() and read it immediately
        // after record returns, before yielding to a frame or another service.
        self.sync_narrative(session)?;
        self.vm.respond(ticket, HostResponse::Ready(Value::NULL))?;
        self.issued = None;
        Ok(())
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
        let line = method("reel::character::line")?;
        let heap = self.vm.heap_mut();
        let fields = heap.alloc_table()?;
        for (i, (key, value)) in [("id", id), ("name", name)].into_iter().enumerate() {
            let key = heap.alloc_string(key)?;
            heap.table_store(fields, i as u32, key, value)?;
        }
        let value = heap.alloc_host(fields, &[id, name], Some(immutable))?;
        self.stage
            .types
            .mark(heap, value, "reel::types::Character")?;
        self.vm.bind_callable(value, call)?;
        let method = self.vm.bind_native(value, line)?;
        let key = self.vm.heap_mut().alloc_string("line")?;
        self.vm.heap_mut().table_store(fields, 2, key, method)?;
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
        let value = heap.alloc_host(fields, &[id, name, text, history], Some(immutable))?;
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
    ) -> Result<NativeDispatch, AdapterError> {
        self.check_story_wait(session, ticket)?;
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
        let content_id = self.dialogue_content_id(ticket);
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
            reveal_skippable,
            wait,
        )?;
        Ok(NativeDispatch::Published {
            interaction,
            work: 1,
        })
    }

    pub(super) fn dispatch_say(
        &mut self,
        session: &mut Session,
        ticket: HostTicket,
        locale: &str,
        budget: u32,
    ) -> Result<NativeDispatch, AdapterError> {
        self.check_story_wait(session, ticket)?;
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
        match reader.step(self.vm.heap_mut(), budget, self.read_limit) {
            Ok((false, work)) => {
                self.dialogue_reader = Some(reader);
                Ok(NativeDispatch::Decoding { work })
            }
            Ok((true, work)) => {
                let base = self.dialogue_content_id(ticket);
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
                let result =
                    self.publish_say(session, ticket, &inputs, locale, reveal_skippable, wait);
                reader.release(self.vm.heap_mut())?;
                result.map(|interaction| NativeDispatch::Published { interaction, work })
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
