//! Instruction admission and execution, before general continuation dispatch.
use super::*;

impl Vm {
    pub(super) fn script_body_step(
        &mut self,
        s: &mut ScriptFrame,
        m: &mut Meter,
    ) -> Result<bool, VmError> {
        let top = self.active.frames.data.len() - 1;
        if !m.admits(0, 1, 0) {
            return Ok(false);
        }
        let (instruction, next_pc) =
            if let Some(instruction) = s.instruction.filter(|_| s.instruction_pc == s.pc) {
                (instruction, s.instruction_end.index())
            } else {
                let Some(decoded) = step_cost!(
                    self,
                    Fetch,
                    self.program.package().functions[s.code.function.index()]
                        .code
                        .fetch_budgeted(
                            s.pc.index(),
                            &mut self.code_window,
                            &mut |work, allocation| m.take(0, work, allocation),
                        )
                )?
                else {
                    return Ok(false);
                };
                s.instruction = Some(decoded.0);
                s.instruction_pc = s.pc;
                decoded
            };
        s.instruction_end = Pc(next_pc as u32);
        s.member_symbol = matches!(instruction.opcode, value if value == Opcode::MemberGet as u16 || value == Opcode::MemberSet as u16).then_some(SymbolId(instruction.operands[2]));
        let mut op = instruction_spec(instruction.opcode)
            .expect("verified spec")
            .opcode;
        #[cfg(feature = "execution-counts")]
        let admitted_opcode = op;
        let [a, mut b, mut c] = instruction.operands;
        let mut index_work = 0;
        if matches!(op, Opcode::IndexGet | Opcode::IndexSet) {
            let object = self
                .slots
                .read(at(s.regs, if op == Opcode::IndexGet { b } else { a }))?;
            if object.tag() == ValueTag::Object
                && matches!(
                    self.heap.info(object)?.kind,
                    ObjectKind::Class | ObjectKind::Instance | ObjectKind::Super
                )
            {
                let key = self
                    .slots
                    .read(at(s.regs, if op == Opcode::IndexGet { c } else { b }))?;
                let bytes = if key.tag() == ValueTag::Object {
                    self.heap.info(key)?.len
                } else {
                    0
                };
                index_work = self.type_checks.symbol_work(bytes);
                // A dynamic name uses the existing immutable symbol index.
                // Check its work before comparing text; register reads do
                // not re-evaluate the author's object or key expressions.
                if !m.admits(1, index_work + 1, 0) {
                    self.publish_script_frame(top, s);
                    return Ok(false);
                }
                let name = self
                    .type_checks
                    .symbol(self.program.package(), &self.heap, key)
                    .map_err(heap_fault)
                    .and_then(|name| name.ok_or(FaultKind::MissingKey));
                let name = match name {
                    Ok(name) => name,
                    Err(kind) => {
                        assert!(m.take(1, index_work + 1, 0));
                        instruction_admitted!(self, admitted_opcode);
                        self.collection_fault(s, kind)?;
                        self.publish_script_frame(top, s);
                        return Ok(true);
                    }
                };
                // Execute the ordinary member operation in this same
                // frame and PC, including reactive refresh and writes.
                op = if op == Opcode::IndexGet {
                    Opcode::MemberGet
                } else {
                    b = c;
                    Opcode::MemberSet
                };
                c = name.0;
            }
        }
        let mut plan = InstructionPlan::None;
        let (work, allocation) =
            match step_cost!(self, Cost, self.instruction_cost(s, op, a, b, c, &mut plan)) {
                Ok((work, allocation)) => (work + index_work, allocation),
                Err(error) => {
                    if !m.take(1, 1 + index_work, 0) {
                        self.publish_script_frame(top, s);
                        return Ok(false);
                    }
                    instruction_admitted!(self, admitted_opcode);
                    self.put(s.env, 1, Value::NULL)?;
                    self.clear(s.env, 2)?;
                    s.phase = ScriptPhase::Exit {
                        origin: s.pc,
                        reason: ExitReason::Throw(FaultSeed {
                            fault: None,
                            explicit: None,
                            author_value: false,
                            awaited: None,

                            kind: vm_fault(error),
                            location: self.loc(*s, s.pc),
                        }),
                        skip: None,
                    };
                    self.publish_script_frame(top, s);
                    return Ok(true);
                }
            };
        let mut cost = (work, allocation);
        let refresh = if op == Opcode::MemberGet {
            let property = if matches!(plan, InstructionPlan::NamedRead(_)) {
                None
            } else if let InstructionPlan::ClassRead(target, property) = &plan {
                if self.heap.reactive_evaluating() {
                    cost.0 += target.work();
                    *property
                } else {
                    None
                }
            } else {
                let object = self.slots.read(at(s.regs, b))?;
                if self.heap.is_property_ref(object)
                    && self.program.package().symbols[c as usize] == "value"
                {
                    let (property, _) = self.heap.property_ref(object)?;
                    Some(property)
                } else if self.heap.reactive_evaluating()
                    && matches!(
                        self.heap.info(object)?.kind,
                        ObjectKind::Class | ObjectKind::Instance | ObjectKind::Super
                    )
                {
                    cost.0 += self.class_member_cost(object)?;
                    match self.class_read_plan(
                        object,
                        SymbolId(c),
                        s.member_symbol.map(|_| (s.code.function, s.pc)),
                    ) {
                        Ok((target, property)) => {
                            plan = InstructionPlan::ClassRead(target, property);
                            property
                        }
                        Err(kind) => {
                            self.collection_fault(s, kind)?;
                            self.publish_script_frame(top, s);
                            return Ok(true);
                        }
                    }
                } else {
                    None
                }
            };
            if let Some(property) = property {
                match self.reactive_read_plan(property, &mut cost) {
                    Ok(read) => read,
                    Err(error) => {
                        self.collection_fault(s, vm_fault(error))?;
                        self.publish_script_frame(top, s);
                        return Ok(true);
                    }
                }
            } else {
                None
            }
        } else {
            None
        };
        if !m.take(1, cost.0, cost.1) {
            // Dynamic class indexing resolved its symbol before the
            // final member operation's allocation admission.
            m.usage.work += index_work;
            self.publish_script_frame(top, s);
            return Ok(false);
        }
        if let Some(read) = refresh {
            instruction_admitted!(self, admitted_opcode);
            execution_count!(self, ReactiveSuspensions, 1);
            self.publish_script_frame(top, s);
            if let Err(error) = self.push_reactive_read(s, read) {
                self.collection_fault(s, vm_fault(error))?;
                self.publish_script_frame(top, s);
            }
            return Ok(true);
        }
        s.instruction = None;
        instruction_admitted!(self, admitted_opcode);
        execution_count!(self, BytecodeExecutions, 1);
        let step = step_cost!(
            self,
            Execute,
            match plan {
                InstructionPlan::ClassRead(target, _) =>
                    self.execute_class_read(s, target, Reg(a), Pc(next_pc as u32)),
                InstructionPlan::Callee(callee) =>
                    self.execute_call(s, op, a, b, Pc(next_pc as u32), Some(callee)),
                InstructionPlan::NamedRead(read) =>
                    self.execute_named_read(s, read, Reg(a), Reg(b), SymbolId(c)),
                InstructionPlan::None => self.execute(s, op, a, b, c, Pc(next_pc as u32)),
            }
        );
        match step {
            Ok(true) => {
                self.publish_script_frame(top, s);
                return Ok(true);
            }
            Ok(false) => {}
            Err(kind) => {
                self.abort_class(s)?;
                self.put(s.env, 1, Value::NULL)?;
                self.clear(s.env, 2)?;
                s.phase = ScriptPhase::Exit {
                    origin: s.pc,
                    reason: ExitReason::Throw(FaultSeed {
                        fault: None,
                        explicit: None,
                        author_value: false,
                        awaited: None,

                        kind,
                        location: self.loc(*s, s.pc),
                    }),
                    skip: None,
                };
            }
        }
        self.publish_script_frame(top, s);
        Ok(true)
    }
}
