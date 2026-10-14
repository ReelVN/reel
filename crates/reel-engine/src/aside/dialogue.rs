//! Character values and grouped ADV dialogue use the existing session wait.
use super::{AdapterError, NativeDispatch, SessionVm, rich_text, text};
use crate::{DialogueLineInput, Session};
use aside_vm::bytecode::{FunctionId, FunctionKind};
use aside_vm::executor::{HostResponse, HostTicket};
use aside_vm::{Heap, HeapError, ObjectKind, RootKind, RootOwner, RootToken, Value, ValueTag};

fn immutable(_: &Heap, _: Value, _: u32, _: Value) -> Result<(), HeapError> {
    Err(HeapError::ReadOnly)
}

fn character_values(heap: &Heap, value: Value) -> Result<[Value; 2], AdapterError> {
    if heap.host_payload_len(value)? != 2 {
        return Err(AdapterError::Input("expected a character"));
    }
    Ok([heap.host_payload(value, 0)?, heap.host_payload(value, 1)?])
}

impl SessionVm {
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
        let heap = self.vm.heap_mut();
        let fields = heap.alloc_table()?;
        let value = heap.alloc_host(fields, &[id, name, text], Some(immutable))?;
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
        let content_id = self.dialogue_content_id(ticket);
        let interaction = self.publish_say(
            session,
            ticket,
            &[DialogueLineInput {
                speaker_id: Some(&id),
                speaker_name: &name,
                content_id: &content_id,
                text: (&text).into(),
            }],
            locale,
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
                        speaker_id: Some(&row.0),
                        speaker_name: &row.1,
                        text: (&row.2).into(),
                        content_id: id,
                    })
                    .collect();
                let result = self.publish_say(session, ticket, &inputs, locale);
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
    current: Option<[Value; 3]>,
    field: usize,
    bytes: Vec<u8>,
    row: [String; 3],
    rows: Vec<(String, String, reel_core::text::Text)>,
    rich: bool,
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
            rich: false,
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
                if heap.host_payload_len(line)? != 3 {
                    return Err(AdapterError::Input(
                        "say expects character.line(text) values",
                    ));
                }
                let mut values = [
                    heap.host_payload(line, 0)?,
                    heap.host_payload(line, 1)?,
                    heap.host_payload(line, 2)?,
                ];
                let (source, rich) = rich_text::source(heap, values[2])?;
                values[2] = source;
                self.rich = rich;
                for value in values {
                    if heap.info(value)?.kind != ObjectKind::String {
                        return Err(AdapterError::Input("invalid dialogue line"));
                    }
                }
                heap.set_field(self.roots, 1, line)?;
                self.current = Some(values);
                work += 1;
                continue;
            }
            let value = self.current.unwrap()[self.field];
            let len = heap.info(value)?.len as usize;
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
                if self.field == 3 {
                    self.rows
                        .try_reserve(1)
                        .map_err(|_| AdapterError::Allocation)?;
                    let [id, name, text] = std::mem::take(&mut self.row);
                    self.rows
                        .push((id, name, rich_text::decode(text, self.rich)?));
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
