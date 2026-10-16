//! Dependency-driven image slots built from ordinary stage nodes. All retained
//! state belongs to the VM heap, including work suspended between native slices.
use super::*;
use aside_host::PropertySchema;
use aside_vm::bytecode::Const;
use aside_vm::executor::{Continuation, FaultRetention};
use aside_vm::{Heap, ObjectKind, PropertyId, ValueTag};
use stage::NodeProperty;

#[derive(Clone, Copy)]
#[repr(u32)]
enum State {
    Node,
    Defaults,
    Slots,
}

#[derive(Clone, Copy)]
#[repr(u64)]
enum Phase {
    Source,
    Read,
    Apply,
}

struct Work {
    output: Value,
    state: Value,
    source: Value,
    descriptions: Value,
    slots: Value,
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
            descriptions: values[3],
            slots: values[4],
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
            self.descriptions,
            self.slots,
            self.children,
            Value::int(i64::from(self.index)),
            Value::int(self.version as i64),
        ]
    }
    fn state(&self, heap: &Heap, slot: State) -> Result<Value, HeapError> {
        heap.sequence_get(self.state, slot as u32)
    }
}

fn defaults(heap: &mut Heap) -> Result<Value, HeapError> {
    let fit = heap.alloc_string("contain")?;
    let values = heap.alloc_array(&[Value::NULL, fit])?;
    for property in NodeProperty::PROPERTIES {
        let value = match (property.default)() {
            Const::Null => Value::NULL,
            Const::Bool(v) => Value::bool(v),
            Const::Int(v) => Value::int(v),
            Const::FloatBits(v) => Value::float(f64::from_bits(v)),
            Const::Utf8(v) => heap.alloc_string(&v)?,
            _ => unreachable!("scalar stage property default"),
        };
        heap.array_push(values, value)?;
    }
    Ok(values)
}

/// Resolve one descriptor while dependency capture is active, before touching
/// any visible node. The common image schema is the only validation boundary.
fn description(
    heap: &mut Heap,
    part: Value,
    defaults: Value,
) -> Result<(Value, u32), AdapterError> {
    if part.tag() == ValueTag::Null {
        return Ok((Value::NULL, 1));
    }
    let count = heap.table_len(part)?;
    heap.observe_read(part, None)?;
    let mut values = Vec::with_capacity(2 + NodeProperty::PROPERTIES.len());
    let mut work = 2 * heap.sequence_len(defaults)?;
    for i in 0..heap.sequence_len(defaults)? {
        values.push(heap.sequence_get(defaults, i)?);
    }
    for i in 0..count {
        let (key, mut value) = heap.table_entry(part, i)?;
        // Property names are from a fixed schema, so no arbitrary string copy.
        let (field, compared) = image_field(heap, key)?;
        work += compared + 1;
        if heap.is_property_ref(value) {
            let (property, _) = heap.property_ref(value)?;
            heap.observe_read(property.host, Some(property.field))?;
            value = heap.host_property(property)?;
        }
        values[field] = value;
    }
    for (field, &value) in values.iter().enumerate() {
        stage::validate_image_property(heap, field as u32, value)?;
    }
    Ok((heap.alloc_array(&values)?, work))
}

fn image_field(heap: &Heap, key: Value) -> Result<(usize, u32), AdapterError> {
    let info = heap.info(key)?;
    let mut work = 1;
    if info.kind == ObjectKind::String {
        for (index, name) in ["source", "fit"]
            .into_iter()
            .chain(NodeProperty::PROPERTIES.iter().map(|p| p.name))
            .enumerate()
        {
            work += 1;
            if name.len() != info.len as usize {
                continue;
            }
            let mut same = true;
            for (offset, expected) in name.bytes().enumerate() {
                work += 1;
                let mut byte = [0];
                heap.read_bytes(key, offset as u32, &mut byte)?;
                if byte[0] != expected {
                    same = false;
                    break;
                }
            }
            if same {
                return Ok((index, work));
            }
        }
    }
    Err(AdapterError::Input("unknown image layer property"))
}

impl SessionVm {
    fn layered_continue(
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

    fn image_origin(&self, ticket: HostTicket) -> Option<String> {
        let p = self.vm.program().package();
        ticket.location.source.map(|span| {
            let source = &p.sources[span.file.index()];
            let before = &source.text[..span.start as usize];
            let line = before.bytes().filter(|b| *b == b'\n').count() + 1;
            let column = before.rsplit('\n').next().unwrap().chars().count() + 1;
            format!("{}:{line}:{column}", p.symbols[source.path.index()])
        })
    }

    pub(super) fn dispatch_layered_image(
        &mut self,
        ticket: HostTicket,
    ) -> Result<u32, AdapterError> {
        let result = self.layered_step(ticket);
        if result.is_err() && ticket.input != NativeInput::Entry {
            let output = self.vm.host_continuation(ticket)?[0];
            self.vm.heap_mut().end_derived(output)?;
        }
        if result.is_ok() {
            self.issued = None;
        }
        result
    }

    fn layered_step(&mut self, ticket: HostTicket) -> Result<u32, AdapterError> {
        if ticket.input == NativeInput::Entry {
            let args = self.vm.host_arguments(ticket)?.to_vec();
            let (output, state) = if let Some(output) = self.vm.reactive_output(ticket)? {
                let state = self.vm.heap().derived_state(output)?;
                let node = self.vm.heap().sequence_get(state, State::Node as u32)?;
                if self.stage.id(self.vm.heap(), node).is_err() {
                    self.vm.heap_mut().disconnect_derived(output)?;
                    self.vm.respond(ticket, HostResponse::Ready(Value::NULL))?;
                    return Ok(1);
                }
                (output, state)
            } else {
                let mut layout = args[1..].to_vec();
                layout.push(Value::NULL);
                let node = self.stage.dispatch(
                    &mut self.vm,
                    self.root_owner,
                    "reel::stack",
                    &layout,
                    self.read_limit,
                    None,
                )?;
                let output = self.vm.heap_mut().alloc_derived(args[0])?;
                let defaults = defaults(self.vm.heap_mut())?;
                let slots = self.vm.heap_mut().alloc_array(&[])?;
                let state = self.vm.heap_mut().alloc_array(&[node, defaults, slots])?;
                self.vm.heap_mut().set_derived_state(output, state)?;
                self.vm.heap_mut().set_host_payload(node, 3, output)?;
                (output, state)
            };
            let work = Work {
                output,
                state,
                source: Value::NULL,
                descriptions: self.vm.heap_mut().alloc_array(&[])?,
                slots: self.vm.heap_mut().alloc_array(&[])?,
                children: self.vm.heap_mut().alloc_array(&[])?,
                index: 0,
                version: 0,
            };
            self.vm
                .call_reactive(ticket, output, args[0], true, &work.values())?;
            return Ok(NodeProperty::PROPERTIES.len() as u32);
        }

        let mut work = Work::load(self.vm.host_continuation(ticket)?);
        if ticket.input == NativeInput::CallbackFault {
            self.vm.heap_mut().end_derived(work.output)?;
            self.vm.respond(
                ticket,
                HostResponse::ForwardFault(self.vm.host_callback_fault(ticket)?.unwrap().id),
            )?;
            return Ok(1);
        }
        let mut cost = 1;
        match ticket.continuation.unwrap().stage {
            phase if phase == Phase::Source as u64 => {
                work.source = self.vm.host_callback_value(ticket)?;
                if self.vm.heap().info(work.source)?.kind != ObjectKind::Array {
                    return Err(AdapterError::Input(
                        "layered_image layers must return an array",
                    ));
                }
                work.version = self.vm.heap().info(work.source)?.content_version;
                self.vm.heap_mut().observe_read(work.source, None)?;
                self.layered_continue(ticket, &work, Phase::Read)?;
            }
            phase if phase == Phase::Read as u64 => {
                if self.vm.heap().info(work.source)?.content_version != work.version {
                    return Err(HeapError::ConcurrentModification.into());
                }
                if work.index < self.vm.heap().sequence_len(work.source)? {
                    let part = self.vm.heap().sequence_get(work.source, work.index)?;
                    let defaults = work.state(self.vm.heap(), State::Defaults)?;
                    let (args, work_cost) = description(self.vm.heap_mut(), part, defaults)?;
                    cost += work_cost;
                    self.vm.heap_mut().array_push(work.descriptions, args)?;
                    work.index += 1;
                    self.layered_continue(ticket, &work, Phase::Read)?;
                } else {
                    let node = work.state(self.vm.heap(), State::Node)?;
                    self.vm.heap_mut().commit_derived(work.output, node)?;
                    self.vm.heap_mut().begin_untracked(work.output)?;
                    work.index = 0;
                    self.layered_continue(ticket, &work, Phase::Apply)?;
                }
            }
            phase if phase == Phase::Apply as u64 => {
                if work.index < self.vm.heap().sequence_len(work.descriptions)? {
                    self.apply_image_slot(ticket, &work)?;
                    cost += (NodeProperty::PROPERTIES.len() + 2) as u32;
                    work.index += 1;
                    self.layered_continue(ticket, &work, Phase::Apply)?;
                } else {
                    let node = work.state(self.vm.heap(), State::Node)?;
                    cost += self.vm.heap().sequence_len(work.children)?;
                    self.stage.reconcile_list(
                        self.vm.heap_mut(),
                        node,
                        work.children,
                        self.read_limit,
                    )?;
                    self.vm
                        .heap_mut()
                        .set_field(work.state, State::Slots as u32, work.slots)?;
                    self.vm.heap_mut().end_derived(work.output)?;
                    self.vm.respond(ticket, HostResponse::Ready(node))?;
                }
            }
            _ => return Err(AdapterError::Input("layered image continuation")),
        }
        Ok(cost)
    }

    fn apply_image_slot(&mut self, ticket: HostTicket, work: &Work) -> Result<(), AdapterError> {
        let old = work.state(self.vm.heap(), State::Slots)?;
        let mut node = if work.index < self.vm.heap().sequence_len(old)? {
            self.vm.heap().sequence_get(old, work.index)?
        } else {
            Value::NULL
        };
        let args = self.vm.heap().sequence_get(work.descriptions, work.index)?;
        if args.tag() == ValueTag::Null {
            if node.tag() != ValueTag::Null {
                let property = PropertyId {
                    host: node,
                    field: 2 + NodeProperty::Visible.index() as u32,
                };
                if self.vm.heap().host_property(property)?.as_bool() != Some(false) {
                    self.vm
                        .heap_mut()
                        .write_host_property(property, Value::bool(false), true)?;
                }
            }
        } else if node.tag() == ValueTag::Null {
            let values = (0..self.vm.heap().sequence_len(args)?)
                .map(|i| self.vm.heap().sequence_get(args, i))
                .collect::<Result<Vec<_>, _>>()?;
            let origin = self.image_origin(ticket);
            node = self.stage.dispatch(
                &mut self.vm,
                self.root_owner,
                "reel::image",
                &values,
                self.read_limit,
                origin,
            )?;
        } else {
            for field in 0..self.vm.heap().sequence_len(args)? {
                let value = self.vm.heap().sequence_get(args, field)?;
                let property = PropertyId { host: node, field };
                if !self.vm.heap().host_property(property)?.same_bits(value) {
                    self.vm
                        .heap_mut()
                        .write_host_property(property, value, true)?;
                }
            }
        }
        self.vm.heap_mut().array_push(work.slots, node)?;
        if node.tag() != ValueTag::Null {
            self.vm.heap_mut().array_push(work.children, node)?;
        }
        Ok(())
    }
}
