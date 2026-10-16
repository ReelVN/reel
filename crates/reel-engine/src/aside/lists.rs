//! Keyed reconciliation. Persistent and in-flight values live in the VM heap;
//! only the sorting workspace is Rust-owned, with a scoped heap reservation.
use super::keys::compare;
use super::*;
use aside_vm::executor::{Continuation, FaultRetention};
use aside_vm::{Heap, ObjectKind, ValueTag};
use std::cmp::Ordering;

#[derive(Clone, Copy)]
#[repr(u32)]
enum Entry {
    Key,
    Item,
    Row,
    Node,
}
#[derive(Clone, Copy)]
#[repr(u32)]
enum State {
    Node,
    Entries,
}
#[derive(Clone, Copy)]
#[repr(u64)]
enum Phase {
    Source,
    Key,
    Build,
    Built,
    Publish,
}
#[derive(Clone, Copy)]
#[repr(u32)]
enum Row {
    Value = 1,
    Index,
}

struct Work {
    output: Value,
    state: Value,
    source: Value,
    entries: Value,
    sorted: Value,
    children: Value,
    index: u32,
    version: u64,
}
impl Work {
    fn load(values: &[Value]) -> Self {
        Self {
            output: values[0],
            state: values[1],
            source: values[2],
            entries: values[3],
            sorted: values[4],
            children: values[5],
            index: values[6].as_int().unwrap() as u32,
            version: values[7].as_int().unwrap() as u64,
        }
    }
    fn values(&self) -> [Value; 8] {
        [
            self.output,
            self.state,
            self.source,
            self.entries,
            self.sorted,
            self.children,
            Value::int(i64::from(self.index)),
            Value::int(self.version as i64),
        ]
    }
    fn check_source(&self, heap: &Heap) -> Result<(), AdapterError> {
        if heap.info(self.source)?.content_version != self.version {
            return Err(HeapError::ConcurrentModification.into());
        }
        Ok(())
    }
}

fn entry(heap: &Heap, value: Value, field: Entry) -> Result<Value, HeapError> {
    heap.sequence_get(value, field as u32)
}

fn sorted(heap: &mut Heap, entries: Value, chunk: usize) -> Result<Value, AdapterError> {
    let count = heap.sequence_len(entries)? as usize;
    let _reservation = heap
        .budget_usage()
        .reserve((count * size_of::<Value>()) as u64)?;
    let mut values = Vec::new();
    values
        .try_reserve_exact(count)
        .map_err(|_| AdapterError::Allocation)?;
    for i in 0..count {
        values.push(heap.sequence_get(entries, i as u32)?);
    }
    values.sort_unstable_by(|&a, &b| {
        compare(
            heap,
            entry(heap, a, Entry::Key).unwrap(),
            entry(heap, b, Entry::Key).unwrap(),
            chunk,
        )
        .expect("validated immutable list keys")
    });
    for pair in values.windows(2) {
        if compare(
            heap,
            entry(heap, pair[0], Entry::Key)?,
            entry(heap, pair[1], Entry::Key)?,
            chunk,
        )? == Ordering::Equal
        {
            return Err(AdapterError::Input("list keys must be unique"));
        }
    }
    let result = heap.alloc_array(&[])?;
    for &value in &values {
        heap.array_push(result, value)?;
    }
    Ok(result)
}

fn find(
    heap: &Heap,
    entries: Value,
    key: Value,
    chunk: usize,
) -> Result<Option<Value>, AdapterError> {
    let (mut low, mut high) = (0, heap.sequence_len(entries)?);
    while low < high {
        let mid = low + (high - low) / 2;
        let candidate = heap.sequence_get(entries, mid)?;
        match compare(heap, entry(heap, candidate, Entry::Key)?, key, chunk)? {
            Ordering::Less => low = mid + 1,
            Ordering::Greater => high = mid,
            Ordering::Equal => return Ok(Some(candidate)),
        }
    }
    Ok(None)
}

impl SessionVm {
    fn list_continue(
        &mut self,
        ticket: HostTicket,
        work: &Work,
        phase: Phase,
    ) -> Result<(), AdapterError> {
        self.vm.respond(
            ticket,
            HostResponse::Continue(Continuation {
                version: 1,
                stage: phase as u64,
                values: &work.values(),
                faults: FaultRetention::Preserve,
            }),
        )?;
        Ok(())
    }

    fn list_call(
        &mut self,
        ticket: HostTicket,
        work: &Work,
        phase: Phase,
        closure: Value,
        argument: Value,
    ) -> Result<(), AdapterError> {
        self.vm.respond(
            ticket,
            HostResponse::CallScript {
                closure,
                arguments: &[argument],
                continuation: Continuation {
                    version: 1,
                    stage: phase as u64,
                    values: &work.values(),
                    faults: FaultRetention::Preserve,
                },
            },
        )?;
        Ok(())
    }

    fn list_key(&mut self, ticket: HostTicket, mut work: Work) -> Result<(), AdapterError> {
        work.check_source(self.vm.heap())?;
        if work.index < self.vm.heap().sequence_len(work.source)? {
            let item = self.vm.heap().sequence_get(work.source, work.index)?;
            let key = self.vm.host_arguments(ticket)?[1];
            self.list_call(ticket, &work, Phase::Key, key, item)
        } else {
            work.sorted = sorted(self.vm.heap_mut(), work.entries, self.read_limit)?;
            let node = self
                .vm
                .heap()
                .sequence_get(work.state, State::Node as u32)?;
            self.vm.heap_mut().commit_derived(work.output, node)?;
            self.vm.heap_mut().begin_untracked(work.output)?;
            work.index = 0;
            self.list_continue(ticket, &work, Phase::Build)
        }
    }

    fn list_built(
        &mut self,
        ticket: HostTicket,
        mut work: Work,
        child: Value,
    ) -> Result<(), AdapterError> {
        if child.tag() != ValueTag::Null {
            self.stage.id(self.vm.heap(), child)?;
            self.vm.heap_mut().array_push(work.children, child)?;
        }
        let candidate = self.vm.heap().sequence_get(work.entries, work.index)?;
        self.vm
            .heap_mut()
            .set_field(candidate, Entry::Node as u32, child)?;
        work.index += 1;
        self.list_continue(ticket, &work, Phase::Build)
    }

    pub(super) fn dispatch_list(&mut self, ticket: HostTicket) -> Result<(), AdapterError> {
        if ticket.input == NativeInput::Entry {
            let args = self.vm.host_arguments(ticket)?.to_vec();
            let (output, state) = if let Some(output) = self.vm.reactive_output(ticket)? {
                let state = self.vm.heap().derived_state(output)?;
                let node = self.vm.heap().sequence_get(state, State::Node as u32)?;
                if self.stage.id(self.vm.heap(), node).is_err() {
                    self.vm.heap_mut().disconnect_derived(output)?;
                    self.vm.respond(ticket, HostResponse::Ready(Value::NULL))?;
                    self.issued = None;
                    return Ok(());
                }
                (output, state)
            } else {
                let mut layout = args[3..].to_vec();
                layout.push(Value::NULL);
                let origin = self.stage_origin(ticket);
                let node = self.stage.dispatch(
                    &mut self.vm,
                    self.root_owner,
                    "reel::column",
                    &layout,
                    self.read_limit,
                    origin,
                )?;
                let output = self.vm.heap_mut().alloc_derived(args[0])?;
                let entries = self.vm.heap_mut().alloc_array(&[])?;
                let state = self.vm.heap_mut().alloc_array(&[node, entries])?;
                self.vm.heap_mut().set_derived_state(output, state)?;
                self.vm.heap_mut().set_host_payload(node, 3, output)?;
                (output, state)
            };
            let work = Work {
                output,
                state,
                source: Value::NULL,
                entries: self.vm.heap_mut().alloc_array(&[])?,
                sorted: Value::NULL,
                children: self.vm.heap_mut().alloc_array(&[])?,
                index: 0,
                version: 0,
            };
            self.vm
                .call_reactive(ticket, output, args[0], false, &work.values())?;
        } else {
            let mut work = Work::load(self.vm.host_continuation(ticket)?);
            if ticket.input == NativeInput::CallbackFault {
                self.vm.heap_mut().end_derived(work.output)?;
                self.vm.respond(
                    ticket,
                    HostResponse::ForwardFault(self.vm.host_callback_fault(ticket)?.unwrap().id),
                )?;
                self.issued = None;
                return Ok(());
            }
            match ticket.continuation.unwrap().stage {
                stage if stage == Phase::Source as u64 => {
                    work.source = self.vm.host_callback_value(ticket)?;
                    if self.vm.heap().info(work.source)?.kind != ObjectKind::Array {
                        return Err(AdapterError::Input("list source must return an array"));
                    }
                    work.version = self.vm.heap().info(work.source)?.content_version;
                    self.vm.heap_mut().observe_read(work.source, None)?;
                    self.list_key(ticket, work)?;
                }
                stage if stage == Phase::Key as u64 => {
                    work.check_source(self.vm.heap())?;
                    let key = self.vm.host_callback_value(ticket)?;
                    let item = self.vm.heap().sequence_get(work.source, work.index)?;
                    let candidate =
                        self.vm
                            .heap_mut()
                            .alloc_array(&[key, item, Value::NULL, Value::NULL])?;
                    self.vm.heap_mut().array_push(work.entries, candidate)?;
                    work.index += 1;
                    self.list_key(ticket, work)?;
                }
                stage if stage == Phase::Build as u64 => {
                    work.check_source(self.vm.heap())?;
                    if work.index == self.vm.heap().sequence_len(work.entries)? {
                        self.vm.heap_mut().end_derived(work.output)?;
                        work.index = 0;
                        self.list_continue(ticket, &work, Phase::Publish)?;
                    } else {
                        let heap = self.vm.heap();
                        let candidate = heap.sequence_get(work.entries, work.index)?;
                        let key = entry(heap, candidate, Entry::Key)?;
                        let old = heap.sequence_get(work.state, State::Entries as u32)?;
                        if let Some(old) = find(heap, old, key, self.read_limit)? {
                            let row = entry(heap, old, Entry::Row)?;
                            let child = entry(heap, old, Entry::Node)?;
                            self.vm
                                .heap_mut()
                                .set_field(candidate, Entry::Row as u32, row)?;
                            self.list_built(ticket, work, child)?;
                        } else {
                            let item = entry(heap, candidate, Entry::Item)?;
                            let row = observable::record(
                                self.vm.heap_mut(),
                                &[
                                    ("key", key),
                                    ("value", item),
                                    ("index", Value::int(i64::from(work.index))),
                                ],
                                &[],
                            )?;
                            self.vm
                                .heap_mut()
                                .set_field(candidate, Entry::Row as u32, row)?;
                            let build = self.vm.host_arguments(ticket)?[2];
                            self.list_call(ticket, &work, Phase::Built, build, row)?;
                        }
                    }
                }
                stage if stage == Phase::Built as u64 => {
                    let child = self.vm.host_callback_value(ticket)?;
                    self.list_built(ticket, work, child)?;
                }
                stage if stage == Phase::Publish as u64 => {
                    if work.index < self.vm.heap().sequence_len(work.entries)? {
                        let candidate = self.vm.heap().sequence_get(work.entries, work.index)?;
                        let row = entry(self.vm.heap(), candidate, Entry::Row)?;
                        let item = entry(self.vm.heap(), candidate, Entry::Item)?;
                        observable::publish(self.vm.heap_mut(), row, Row::Value as u32, item)?;
                        observable::publish(
                            self.vm.heap_mut(),
                            row,
                            Row::Index as u32,
                            Value::int(i64::from(work.index)),
                        )?;
                        work.index += 1;
                        self.list_continue(ticket, &work, Phase::Publish)?;
                    } else {
                        let node = self
                            .vm
                            .heap()
                            .sequence_get(work.state, State::Node as u32)?;
                        self.stage.reconcile_list(
                            self.vm.heap_mut(),
                            node,
                            work.children,
                            self.read_limit,
                        )?;
                        self.vm.heap_mut().set_field(
                            work.state,
                            State::Entries as u32,
                            work.sorted,
                        )?;
                        self.vm.respond(ticket, HostResponse::Ready(node))?;
                    }
                }
                _ => return Err(AdapterError::Input("list continuation")),
            }
        }
        self.issued = None;
        Ok(())
    }
}
