//! Synchronous general instructions retain the scalar block's roots and cursor.
use super::*;

struct Borrowed<'a, 'b> {
    registers: &'a mut Registers<'b>,
    environment: &'a EnvironmentWindow<'b>,
}
impl body_operations::Operands for Borrowed<'_, '_> {
    fn read(&self, register: u32) -> Result<Value, HeapError> {
        self.registers.read(register)
    }
    fn write(&mut self, register: u32, value: Value) -> Result<(), HeapError> {
        self.registers.write(register, value)
    }
    fn environment(&self, index: u32) -> Result<Value, HeapError> {
        self.registers.environment(self.environment, index)
    }
    fn heap_read(&self) -> &Heap {
        self.registers.heap_read()
    }
    fn cell_initialize(&mut self, cell: Value, value: Value) -> Result<(), HeapError> {
        self.registers.cell_initialize(cell, value)
    }
    fn cell_write(&mut self, cell: Value, value: Value) -> Result<(), HeapError> {
        self.registers.cell_write(cell, value)
    }
}

impl Block<'_> {
    // Match general dispatch's attempted phase, then its exact opcode admission.
    // Failed admission retains the fetched instruction at the original PC.
    fn admit_general(
        &mut self,
        meter: &mut Meter,
        _opcode: Opcode,
        work: u64,
        allocation: u64,
    ) -> bool {
        #[cfg(feature = "execution-counts")]
        self.execution_counts.step(&ScriptPhase::Body);
        if !meter.take(1, work, allocation) {
            return false;
        }
        self.cursor.instruction = None;
        instruction_admitted!(self, _opcode);
        execution_count!(self, BytecodeExecutions, 1);
        true
    }

    pub(super) fn synchronous_step(
        &mut self,
        meter: &mut Meter,
        decoded: BodyInstruction,
    ) -> Result<Exit, VmError> {
        let BodyInstruction {
            instruction,
            opcode,
            next,
        } = decoded;
        let [a, b, c] = instruction.operands;
        let result = match opcode {
            Opcode::BindInit | Opcode::BindWrite => {
                let binding = &self.program.package().functions[self.cursor.function.index()]
                    .bindings[a as usize];
                let work = 1 + self.types.extra(binding.ty);
                if !self.admit_general(meter, opcode, work, 0) {
                    return Ok(Exit::Budget);
                }
                #[cfg(test)]
                self.work_probe
                    .body_bindings
                    .set(self.work_probe.body_bindings.get() + 1);
                body_operations::bind(
                    &mut Borrowed {
                        registers: &mut self.registers,
                        environment: self.environment.as_ref().unwrap(),
                    },
                    self.program,
                    self.types,
                    binding,
                    (self.header.context, self.header.effects),
                    opcode,
                    b,
                )
            }
            Opcode::CaptureRead => {
                if !self.admit_general(meter, opcode, 1, 0) {
                    return Ok(Exit::Budget);
                }
                #[cfg(test)]
                self.work_probe
                    .body_captures
                    .set(self.work_probe.body_captures.get() + 1);
                body_operations::capture(
                    &mut Borrowed {
                        registers: &mut self.registers,
                        environment: self.environment.as_ref().unwrap(),
                    },
                    self.header.closure,
                    a,
                    b,
                )
            }
            Opcode::Eq | Opcode::Ne => {
                // Structural object comparisons retain their original cursors,
                // roots and per-step budgets in general continuation dispatch.
                let Ok(left) = self.registers.peek(b) else {
                    return Ok(Exit::General(opcode));
                };
                let Ok(right) = self.registers.peek(c) else {
                    return Ok(Exit::General(opcode));
                };
                if left.tag() == ValueTag::Object || right.tag() == ValueTag::Object {
                    return Ok(Exit::General(opcode));
                }
                if !self.admit_general(meter, opcode, 1, 0) {
                    return Ok(Exit::Budget);
                }
                self.registers.count_peek_pair();
                #[cfg(test)]
                self.work_probe
                    .body_equalities
                    .set(self.work_probe.body_equalities.get() + 1);
                binary(opcode, left, right)
                    .and_then(|value| self.registers.write(a, value).map_err(heap_fault))
            }
            Opcode::MemberGet => return self.synchronous_field(meter, decoded),
            _ => unreachable!("only synchronous Body candidates enter this block"),
        };
        match result {
            Ok(()) => {
                self.cursor.advance(next);
                Ok(Exit::Done)
            }
            Err(kind) => Ok(Exit::Fault(kind)),
        }
    }

    fn synchronous_field(
        &mut self,
        meter: &mut Meter,
        decoded: BodyInstruction,
    ) -> Result<Exit, VmError> {
        let [destination, source, name] = decoded.instruction.operands;
        let opcode = decoded.opcode;
        // Speculation is read-only and uncounted. The checked general path owns
        // errors, methods, tables and refreshes that require another script frame.
        let Ok(object) = self.registers.peek(source) else {
            return Ok(Exit::General(opcode));
        };
        let (target, work) = {
            let heap = self.registers.heap_read();
            let Ok(read) = heap.read_object(object) else {
                return Ok(Exit::General(opcode));
            };
            if !matches!(
                read.kind(),
                ObjectKind::Class | ObjectKind::Instance | ObjectKind::Super
            ) {
                return Ok(Exit::General(opcode));
            }
            let context = classes::ClassReadContext {
                heap,
                class_member_hints: self.class_hints,
                #[cfg(feature = "profiling")]
                profile: self.profile,
                #[cfg(test)]
                member_hint_hits: &self.work_probe.member_hint_hits,
            };
            let Ok((target, property)) = context.class_read_from(
                object,
                read,
                SymbolId(name),
                Some((self.cursor.function, self.cursor.pc)),
                #[cfg(feature = "profiling")]
                true,
            ) else {
                return Ok(Exit::General(opcode));
            };
            if target.cell().is_none() {
                return Ok(Exit::General(opcode));
            }
            if let Some(property) = property {
                match heap.reactive_derived_source(property) {
                    Ok(None) => {}
                    _ => return Ok(Exit::General(opcode)),
                }
            }
            let work = target.work() * if heap.reactive_evaluating() { 2 } else { 1 };
            (target, work)
        };
        self.cursor.member_symbol = Some(SymbolId(name));
        self.registers.count_peek_read();
        execution_count!(self, MemberGetClassCosts, 1);
        if !self.admit_general(meter, opcode, work, Values::allocation_bytes(3)) {
            return Ok(Exit::Budget);
        }
        #[cfg(test)]
        self.work_probe
            .body_fields
            .set(self.work_probe.body_fields.get() + 1);
        let result = part_cost!(
            self.profile,
            ClassFieldValue,
            self.registers.observed_cell_value(target.cell().unwrap())
        )
        .map_err(heap_fault)
        .and_then(|value| value.ok_or(FaultKind::UninitializedField))
        .and_then(|value| self.registers.write(destination, value).map_err(heap_fault));
        match result {
            Ok(()) => {
                self.cursor.advance(decoded.next);
                Ok(Exit::Done)
            }
            Err(kind) => Ok(Exit::Fault(kind)),
        }
    }
}
