use super::*;

#[derive(Clone, Copy)]
pub(super) enum Fact {
    Unknown,
    Receiver,
    Scalar(Value),
}

#[derive(Clone, Copy)]
pub(super) struct Receiver {
    class: Value,
    members: Value,
    version: u64,
    reactive: bool,
}

pub(super) struct ResolvedReceiver<'a> {
    pub(super) proof: Receiver,
    pub(super) instance_version: u64,
    pub(super) class_version: u64,
    pub(super) fields_value: Value,
    pub(super) fields: crate::heap::ObjectRead<'a>,
}

impl<'a> ResolvedReceiver<'a> {
    pub(super) fn read(heap: &'a Heap, value: Value) -> Option<Self> {
        let instance = heap.read_object(value).ok()?;
        if instance.kind() != ObjectKind::Instance
            || instance
                .field(crate::class::InstanceSlot::State as u32)
                .ok()?
                .as_int()
                != Some(crate::class::State::Ready as i64)
        {
            return None;
        }
        let class_value = instance
            .field(crate::class::InstanceSlot::Class as u32)
            .ok()?;
        let class = heap.read_object(class_value).ok()?;
        let members = class.field(ClassSlot::Members as u32).ok()?;
        let fields_value = instance
            .field(crate::class::InstanceSlot::Fields as u32)
            .ok()?;
        Some(Self {
            proof: Receiver {
                class: class_value,
                members,
                version: heap.read_object(members).ok()?.info().content_version,
                reactive: heap.reactive_evaluating(),
            },
            instance_version: instance.info().content_version,
            class_version: class.info().content_version,
            fields_value,
            fields: heap.read_object(fields_value).ok()?,
        })
    }
}

impl Receiver {
    fn matches(self, other: Self) -> bool {
        self.class.same_bits(other.class)
            && self.members.same_bits(other.members)
            && self.version == other.version
            && self.reactive == other.reactive
    }
}

#[derive(Clone, Copy)]
pub(super) struct Guard {
    pub(super) offset: u32,
    pub(super) contract: crate::CellContract,
    pub(super) expected: Value,
}

pub(super) struct ReadonlyCell {
    pub(super) contract: crate::CellContract,
    pub(super) value: Value,
    pub(super) version: u64,
}

pub(super) fn readonly_cell(heap: &Heap, cell: Value) -> Option<ReadonlyCell> {
    let object = heap.object(heap.key(cell).ok()?).ok()?;
    if object.model || object.observable_fields != 0 || object.property_owner.is_some() {
        return None;
    }
    let crate::payload::Payload::Cell(state) = &object.payload else {
        return None;
    };
    if state.contract.mutable {
        return None;
    }
    // Plain non-observable cells cannot be PropertyWrites endpoints. Their
    // initialized stored value is the ordinary cell_read result in this scope.
    Some(ReadonlyCell {
        contract: state.contract,
        value: state.value?,
        version: object.content_version,
    })
}

#[derive(Clone, Copy)]
pub(super) struct Input {
    pub(super) register: u32,
    pub(super) expected: Fact,
}

#[derive(Clone, Copy)]
pub(super) enum Effect {
    Write(u32, Value),
    Clear([u32; 3], u8),
    Branch,
}

#[derive(Clone, Copy)]
pub(super) struct Step {
    pub(super) pc: Pc,
    pub(super) end: Pc,
    pub(super) target: Pc,
    pub(super) opcode: Opcode,
    pub(super) work: u64,
    pub(super) inputs: [Option<Input>; 2],
    pub(super) effect: Effect,
}

impl Step {
    pub(super) fn destinations(self) -> impl Iterator<Item = (u32, Option<Value>)> {
        let entries = match self.effect {
            Effect::Write(register, value) => [Some((register, Some(value))), None, None],
            Effect::Clear(registers, count) => {
                std::array::from_fn(|i| (i < count as usize).then_some((registers[i], None)))
            }
            Effect::Branch => [None; 3],
        };
        entries.into_iter().flatten()
    }
}

pub(super) struct Plan {
    pub(super) function: Option<FunctionId>,
    receiver: Option<Receiver>,
    pub(super) guards: ChargedVec<Guard>,
    pub(super) steps: ChargedVec<Step>,
    pub(super) spans: ChargedVec<Span>,
    pub(super) inputs: ChargedVec<Input>,
    pub(super) writes: ChargedVec<Write>,
    #[cfg(feature = "execution-counts")]
    pub(super) opcodes: ChargedVec<(Opcode, u8)>,
    pub(super) scope_end: Pc,
    pub(super) generation: u64,
}

#[derive(Clone, Copy)]
pub(super) struct Span {
    pub(super) pc: Pc,
    pub(super) last_pc: Pc,
    pub(super) instruction_end: Pc,
    pub(super) resident_end: Pc,
    pub(super) target: Pc,
    pub(super) instructions: u8,
    #[cfg(feature = "execution-counts")]
    pub(super) fields: u8,
    #[cfg(feature = "execution-counts")]
    pub(super) script_steps: u8,
    pub(super) work: u64,
    pub(super) bulk_last: bool,
    pub(super) input_start: u16,
    pub(super) input_end: u16,
    pub(super) write_start: u16,
    pub(super) write_end: u16,
    #[cfg(feature = "execution-counts")]
    pub(super) opcode_start: u16,
    #[cfg(feature = "execution-counts")]
    pub(super) opcode_end: u16,
}

#[derive(Clone, Copy)]
pub(super) struct Write {
    pub(super) register: u32,
    pub(super) value: Option<Value>,
}

impl Plan {
    pub(super) fn new(usage: &HeapBudgetUsage) -> Result<Self, HeapError> {
        Ok(Self {
            function: None,
            receiver: None,
            guards: ChargedVec::new(usage, FIELDS)?,
            steps: ChargedVec::new(usage, STEPS)?,
            spans: ChargedVec::new(usage, STEPS)?,
            inputs: ChargedVec::new(usage, STEPS * 2)?,
            writes: ChargedVec::new(usage, STEPS * 3)?,
            #[cfg(feature = "execution-counts")]
            opcodes: ChargedVec::new(usage, STEPS)?,
            scope_end: Pc(0),
            generation: 0,
        })
    }

    pub(super) fn reset(&mut self, function: FunctionId, receiver: Receiver) {
        self.function = Some(function);
        self.receiver = Some(receiver);
        self.guards.data.clear();
        self.steps.data.clear();
        self.spans.data.clear();
        self.inputs.data.clear();
        self.writes.data.clear();
        #[cfg(feature = "execution-counts")]
        self.opcodes.data.clear();
        self.scope_end = Pc(0);
    }

    pub(super) fn covers(&self, pc: Pc) -> bool {
        !self.spans.data.is_empty()
            && (pc == Pc(0) || self.spans.data.iter().any(|span| span.pc == pc))
    }

    pub(super) fn memo(
        &self,
        heap: &Heap,
        receiver: Value,
        resolved: &ResolvedReceiver<'_>,
        index: usize,
    ) -> Option<Memo> {
        if !self
            .receiver
            .is_some_and(|expected| expected.matches(resolved.proof))
        {
            return None;
        }
        let mut cells = [(Value::NULL, 0); FIELDS];
        for (index, guard) in self.guards.data.iter().enumerate() {
            let cell = resolved.fields.field(guard.offset).ok()?;
            let current = readonly_cell(heap, cell)?;
            if current.contract != guard.contract || !current.value.same_bits(guard.expected) {
                return None;
            }
            cells[index] = (cell, current.version);
        }
        Some(Memo {
            function: self.function?,
            receiver,
            index,
            generation: self.generation,
            instance_version: resolved.instance_version,
            class: resolved.proof.class,
            class_version: resolved.class_version,
            members: resolved.proof.members,
            members_version: resolved.proof.version,
            fields: resolved.fields_value,
            fields_version: resolved.fields.info().content_version,
            reactive: resolved.proof.reactive,
            cells,
            cell_count: self.guards.data.len() as u8,
        })
    }
}

#[derive(Clone, Copy)]
pub(super) struct Memo {
    pub(super) function: FunctionId,
    pub(super) receiver: Value,
    pub(super) index: usize,
    pub(super) generation: u64,
    instance_version: u64,
    class: Value,
    class_version: u64,
    members: Value,
    members_version: u64,
    fields: Value,
    fields_version: u64,
    reactive: bool,
    cells: [(Value, u64); FIELDS],
    cell_count: u8,
}

impl Memo {
    pub(super) fn observation_cell(&self) -> Value {
        self.cells[0].0
    }

    pub(super) fn matches(&self, heap: &Heap) -> bool {
        if self.reactive != heap.reactive_evaluating() {
            return false;
        }
        for (value, version) in [
            (self.receiver, self.instance_version),
            (self.class, self.class_version),
            (self.members, self.members_version),
            (self.fields, self.fields_version),
        ] {
            let Ok(object) = heap.read_object(value) else {
                return false;
            };
            if object.info().content_version != version {
                return false;
            }
        }
        self.cells[..self.cell_count as usize]
            .iter()
            .all(|&(cell, version)| {
                let Ok(object) = heap.key(cell).and_then(|key| heap.object(key)) else {
                    return false;
                };
                object.content_version == version
                    && !object.model
                    && object.observable_fields == 0
                    && object.property_owner.is_none()
            })
    }
}
