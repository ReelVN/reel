//! Keyed reconciliation. Persistent and in-flight values live in the VM heap;
//! key indexing and reconciliation resume through the same native roots.
use super::keys::compare;

aside_host::native_documentation! {
    pub(super) fn documentation {
        /// Create a keyed list from `items()`, `key(item)` and `build(row)`.
        /// The builder receives observable row fields `value` and `index`. Keys must be
        /// unique: scalars preserve their type, strings compare by content, and other
        /// objects compare by identity. Stable keys retain mounted rows through reordering.
        /// With `virtual: true`, mount the nearest enclosing scroll's visible window and
        /// navigation targets through ordinary containers, accounting for their layout,
        /// transforms and clipping. An empty viewport retains only pinned targets.
        /// Retire rows outside that window. Keep persistent row state in the
        /// item model because remounting creates fresh row nodes. The returned
        /// VirtualListNode exposes scroll_to(key, align: "nearest"). Key indexing
        /// and duplicate validation use bounded native continuations, including
        /// long string comparisons. Source changes invalidate the private index;
        /// snapshots resume its cursors without repeating completed key callbacks.
        "reel::list";
        /// Locate a keyed virtual row, scroll it into view, then return its mounted node
        /// after presentation. Align accepts nearest, start, center or end. Removing the
        /// key, retiring the list, starting another request, or user scrolling/navigation
        /// that supersedes this target cancels the pending request. The result is owned by
        /// the list; cancelling one consumer's await does not cancel the producer.
        /// Positioning uses the owner's logical vertical axis, including container
        /// scaling. An unreachable target reports a source-located native error.
        "reel::list::scroll_to";
    }
}
use super::*;
use aside_vm::executor::{Continuation, FaultRetention};
use aside_vm::{Heap, ObjectKind, ValueTag};
use std::cmp::Ordering;
pub(in crate::aside) mod sort;
pub(super) mod virtualized;

pub(super) fn describe(api: &mut aside_host::source::Api) {
    use aside_compiler::semantic::TypeHint;
    use aside_host::source::{Member, NamedType, named};
    let type_name = "reel::types::VirtualListNode";
    api.refine(
        "reel::list::scroll_to",
        named("reel::types::Presentation"),
        &[("list", named(type_name)), ("align", TypeHint::String)],
    );
    let base = api
        .types
        .iter()
        .find(|ty| ty.name == "reel::types::Node")
        .unwrap();
    let descriptor = NamedType::new(
        type_name,
        "A keyed virtual list with mounted-row scrolling and the shared node properties.",
        vec![Member::method(
            api.native("reel::list::scroll_to")
                .clone()
                .bound("scroll_to"),
        )],
    )
    .extends(base);
    api.types.push(descriptor);
    api.native_mut("reel::list").result_cases(&[
        (
            "virtual",
            aside_vm::bytecode::Const::Bool(true),
            named(type_name),
        ),
        (
            "virtual",
            aside_vm::bytecode::Const::Bool(false),
            named("reel::types::Node"),
        ),
    ]);
}

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
    Sort,
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
            work.sorted = Value::NULL;
            self.list_continue(ticket, &work, Phase::Sort)
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

    pub(super) fn dispatch_list(
        &mut self,
        ticket: HostTicket,
        budget: NativeBudget,
    ) -> Result<NativeDispatch, AdapterError> {
        if matches!(budget, NativeBudget::Exact(0)) {
            return Ok(NativeDispatch::BudgetYield {
                work: 0,
                required_work: 1,
            });
        }
        if self.vm.host_arguments(ticket)?[3]
            .as_bool()
            .ok_or(AdapterError::Input("list virtual must be bool"))?
        {
            return self.dispatch_virtual_list(ticket, budget);
        }
        if ticket.input != NativeInput::Entry
            && ticket.continuation.unwrap().stage == Phase::Sort as u64
        {
            let mut work = Work::load(self.vm.host_continuation(ticket)?);
            work.check_source(self.vm.heap())?;
            let used = match sort::advance(
                self.vm.heap_mut(),
                work.entries,
                work.sorted,
                budget,
                self.read_limit,
            )? {
                sort::Step::Budget { required_work } => {
                    return Ok(NativeDispatch::BudgetYield {
                        work: 0,
                        required_work,
                    });
                }
                sort::Step::More { state, work: used } => {
                    work.sorted = state;
                    self.list_continue(ticket, &work, Phase::Sort)?;
                    used
                }
                sort::Step::Ready { result, work: used } => {
                    work.sorted = result;
                    let node = self
                        .vm
                        .heap()
                        .sequence_get(work.state, State::Node as u32)?;
                    self.vm.heap_mut().commit_derived(work.output, node)?;
                    self.vm.heap_mut().begin_untracked(work.output)?;
                    work.index = 0;
                    self.list_continue(ticket, &work, Phase::Build)?;
                    used
                }
            };
            self.issued = None;
            return Ok(NativeDispatch::Ready { work: used });
        }
        self.dispatch_list_work(ticket)?;
        Ok(NativeDispatch::Ready { work: 1 })
    }
    fn dispatch_list_work(&mut self, ticket: HostTicket) -> Result<(), AdapterError> {
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
                let justify = text::alloc_string(self.vm.heap_mut(), "start", self.read_limit)?;
                let mut layout = vec![Value::bool(false), justify, Value::NULL];
                layout.extend_from_slice(&args[4..]);
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
