//! Resumable script preparation and instruction admission.
use super::*;

impl Vm {
    pub(super) fn script_step(
        &mut self,
        mut s: ScriptFrame,
        m: &mut Meter,
    ) -> Result<bool, VmError> {
        #[cfg(feature = "execution-counts")]
        self.execution_counts.step(&s.phase);
        let top = self.active.frames.data.len() - 1;
        let signature = self.program.package().functions[s.code.function.index()].signature;
        match s.phase {
            ScriptPhase::CompactBegin => {
                let output = self.value(s.env, 4)?;
                let allocate = self
                    .heap
                    .host_payload(output, crate::heap::DerivedSlot::Candidate as u32)?
                    .same_bits(Value::NULL);
                if !m.take(
                    0,
                    12,
                    if allocate {
                        crate::dependencies::BASE_BYTES
                    } else {
                        0
                    },
                ) {
                    return Ok(false);
                }
                if let Err(error) = self.heap.begin_derived(output) {
                    self.block_heap(error)?;
                    return Ok(true);
                }
                s.phase = ScriptPhase::Body;
            }
            ScriptPhase::GeneratorCreate => {
                if !self.generator_create_step(&mut s, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::GeneratorCapture {
                part,
                index,
                created,
            } => {
                if !self.generator_capture_step(&mut s, part, index, created, m)? {
                    return Ok(false);
                }
                if self.active.frames.data.len() <= top {
                    return Ok(true);
                }
            }
            ScriptPhase::GeneratorStart { action, call } => {
                self.active.frames.data[top] = Frame::Script(s);
                if !self.generator_start_step(&mut s, action, call, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::GeneratorRestore {
                part,
                index,
                closing,
            } => {
                if !self.generator_restore_step(&mut s, part, index, closing, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::Class(step) => {
                if !self.class_step(&mut s, step, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::Copy { .. }
            | ScriptPhase::Bind(_)
            | ScriptPhase::Defaults { .. }
            | ScriptPhase::Parameter { .. }
            | ScriptPhase::Install { .. } => return self.preparation_step(m),
            ScriptPhase::Rest => {
                if !m.take(0, 1, 0) {
                    return Ok(false);
                }
                let count = self.program.package().signatures[signature.index()]
                    .parameters
                    .len() as u32;
                match self.heap.alloc_array(&[]) {
                    Ok(rest) => {
                        self.put(s.bound, count, rest)?;
                        s.phase = ScriptPhase::Bind(ArgumentBinding::new(self.receiver_prefix(
                            s.env,
                            s.closure,
                            s.shared_parameters,
                        )?));
                    }
                    Err(error) => self.preparation_fault(&mut s, heap_fault(error))?,
                }
            }
            ScriptPhase::DefaultCall {
                parameter,
                function: default,
            } => {
                let (work, bytes) = self.script_push_cost(default, parameter);
                if !m.take(0, work + 1, bytes) {
                    return Ok(false);
                }
                let closure = if s.closure {
                    Some(self.value(s.env, 0)?)
                } else {
                    None
                };
                self.active.frames.data[top] = Frame::Script(s);
                let prepared = self.push_script(
                    default,
                    closure,
                    ArgSource::Prefix { frame: s.id },
                    parameter,
                    s.owner,
                    inherit(s.context, Context::NoWait),
                    s.effects,
                    ReturnTo::Default {
                        frame: s.id,
                        parameter,
                        call: s.pc,
                    },
                );
                match prepared {
                    Ok(()) => return Ok(true),
                    Err(e) => {
                        let kind = language_fault(e)?;
                        self.preparation_fault(&mut s, kind)?;
                    }
                }
            }
            ScriptPhase::Body => {
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
                            self.active.frames.data[top] = Frame::Script(s);
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
                                self.collection_fault(&mut s, kind)?;
                                self.active.frames.data[top] = Frame::Script(s);
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
                let (work, allocation) =
                    match step_cost!(self, Cost, self.instruction_cost(&s, op, a, b, c)) {
                        Ok((work, allocation)) => (work + index_work, allocation),
                        Err(error) => {
                            if !m.take(1, 1 + index_work, 0) {
                                self.active.frames.data[top] = Frame::Script(s);
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
                                    location: self.loc(s, s.pc),
                                }),
                                skip: None,
                            };
                            self.active.frames.data[top] = Frame::Script(s);
                            return Ok(true);
                        }
                    };
                let mut cost = (work, allocation);
                let mut class_read = None;
                let refresh = if op == Opcode::MemberGet {
                    let object = self.slots.read(at(s.regs, b))?;
                    let property = if self.heap.is_property_ref(object)
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
                        match self.class_read_plan(object, SymbolId(c)) {
                            Ok((target, property)) => {
                                class_read = Some(target);
                                property
                            }
                            Err(kind) => {
                                self.collection_fault(&mut s, kind)?;
                                self.active.frames.data[top] = Frame::Script(s);
                                return Ok(true);
                            }
                        }
                    } else {
                        None
                    };
                    if let Some(property) = property {
                        match self.reactive_read_plan(property, &mut cost) {
                            Ok(read) => read,
                            Err(error) => {
                                self.collection_fault(&mut s, vm_fault(error))?;
                                self.active.frames.data[top] = Frame::Script(s);
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
                    self.active.frames.data[top] = Frame::Script(s);
                    return Ok(false);
                }
                if let Some(read) = refresh {
                    instruction_admitted!(self, admitted_opcode);
                    execution_count!(self, ReactiveSuspensions, 1);
                    self.active.frames.data[top] = Frame::Script(s);
                    if let Err(error) = self.push_reactive_read(s, read) {
                        self.collection_fault(&mut s, vm_fault(error))?;
                        self.active.frames.data[top] = Frame::Script(s);
                    }
                    return Ok(true);
                }
                s.instruction = None;
                instruction_admitted!(self, admitted_opcode);
                execution_count!(self, BytecodeExecutions, 1);
                let step = step_cost!(
                    self,
                    Execute,
                    if let Some(target) = class_read {
                        self.execute_class_read(&mut s, target, Reg(a), Pc(next_pc as u32))
                    } else {
                        self.execute(&mut s, op, a, b, c, Pc(next_pc as u32))
                    }
                );
                match step {
                    Ok(true) => {
                        self.active.frames.data[top] = Frame::Script(s);
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
                                location: self.loc(s, s.pc),
                            }),
                            skip: None,
                        };
                    }
                }
            }
            ScriptPhase::Constant(mut work) | ScriptPhase::DefaultConstant { mut work, .. } => {
                let parameter = match s.phase {
                    ScriptPhase::DefaultConstant { parameter, .. } => Some(parameter),
                    _ => None,
                };
                match self.constant_step(s.env, [1, 2], &mut work, m)? {
                    constants::ConstantStep::Blocked => return Ok(false),
                    constants::ConstantStep::Pending => {
                        s.phase = if let Some(parameter) = parameter {
                            ScriptPhase::DefaultConstant { parameter, work }
                        } else {
                            ScriptPhase::Constant(work)
                        }
                    }
                    constants::ConstantStep::Ready(value) => {
                        if let Some(parameter) = parameter {
                            self.put(s.bound, parameter, value)?;
                            self.finish_constant(s.env, [1, 2])?;
                            s.phase = ScriptPhase::Defaults { parameter };
                        } else {
                            match self.write(s.regs, work.dest, value) {
                                Ok(()) => {
                                    self.finish_constant(s.env, [1, 2])?;
                                    s.pc = s.instruction_end;
                                    s.phase = ScriptPhase::Body;
                                }
                                Err(kind) => self.collection_fault(&mut s, kind)?,
                            }
                        }
                    }
                    constants::ConstantStep::Fault(kind) => {
                        if parameter.is_some() {
                            self.preparation_fault(&mut s, kind)?;
                        } else {
                            self.collection_fault(&mut s, kind)?;
                        }
                    }
                }
            }
            ScriptPhase::Spread(work) => {
                if !self.spread_step(&mut s, work, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::TableSpread(work) => {
                if !self.table_spread_step(&mut s, work, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::KeyPath { index } => {
                if !self.key_path_step(&mut s, index, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::TableValidate | ScriptPhase::TableScan => {
                if !self.table_step(&mut s, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::HostMethod { object, dest } => {
                if !self.host_method_step(&mut s, object, dest, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::Sequence {
                dest,
                window,
                index,
                tuple,
            } => {
                if !self.sequence_step(&mut s, dest, window, index, tuple, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::TextFinish { .. } => {
                if !self.text_finish_step(&mut s, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::TextPush {
                builder,
                input,
                offset,
            } => {
                if !self.text_push_step(&mut s, builder, input, offset, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::Builtin(_) => {
                if !self.builtin_step(&mut s, m)? {
                    self.active.frames.data[top] = Frame::Script(s);
                    return Ok(false);
                }
            }
            ScriptPhase::Search(work) => {
                if !self.search_step(&mut s, work, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::Sort(work) => {
                if !self.sort_step(&mut s, work, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::Bulk(work) => {
                if !self.bulk_step(&mut s, work, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::TextBuiltin(work) => {
                if !self.text_builtin_step(&mut s, work, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::Concat {
                dest,
                left,
                right,
                offset,
            } => {
                if !self.concat_step(&mut s, dest, left, right, offset, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::Equality { dest, negated } => {
                if !m.take(0, 1, 0) {
                    return Ok(false);
                }
                self.equality_step(&mut s, dest, negated)?;
            }
            ScriptPhase::EqualityCleanup {
                dest,
                negated,
                result,
            } => {
                if !m.take(0, 1, 0) {
                    return Ok(false);
                }
                if !self.active.equality_cursors.data.is_empty() {
                    let depth = self.active.equality_cursors.data.len() as u32 - 1;
                    self.clear(self.active.equality_roots.unwrap(), depth * 2)?;
                    self.clear(self.active.equality_roots.unwrap(), depth * 2 + 1)?;
                    self.active.equality_cursors.data.pop();
                } else {
                    if s.table_work.is_some() {
                        self.table_comparison_done(&mut s, result)?;
                    } else {
                        match result {
                            Ok(equal) => {
                                self.put(s.regs, dest.0, Value::bool(equal != negated))?;
                                s.pc = s.instruction_end;
                                s.phase = ScriptPhase::Body;
                            }
                            Err(kind) => {
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
                                        location: self.loc(s, s.pc),
                                    }),
                                    skip: None,
                                };
                            }
                        }
                    }
                }
            }
            ScriptPhase::Exit {
                origin,
                reason,
                skip,
            } => {
                let regions = self.program.package().functions[s.code.function.index()]
                    .regions
                    .len() as u64;
                // The shared selector visits every region. A consumed-region
                // check can compare up to three parent blocks with three child
                // blocks. Admit this bound before making any selector call.
                let work = regions * (if skip.is_some() { 32 } else { 8 }) + 1;
                if !m.take(0, work, 0) {
                    return Ok(false);
                }
                let handler = self.select_exit_handler(s, origin, reason, skip)?;
                s.phase = ScriptPhase::ExitApply {
                    origin,
                    reason,
                    handler,
                };
            }
            ScriptPhase::ExitApply {
                origin,
                reason,
                handler,
            } => {
                let superseded = self.superseded_unwind(s, origin, reason, handler);
                let replacing_fault =
                    superseded.is_some() && matches!(reason, ExitReason::Throw(_));
                let roots = if superseded.is_some() {
                    (self.release_cost(1), 0)
                } else if handler.is_some_and(|h| h.kind == HandlerKind::Finally) {
                    self.window_push_cost(1, 2)
                } else {
                    (0, 0)
                };
                let work = 2 * self.active.caught.data.len() as u64
                    + 12
                    + roots.0
                    + if replacing_fault {
                        self.program_lookup_work()
                    } else {
                        0
                    };
                let alloc = if replacing_fault {
                    crate::fault::FAULT_NODE_BYTES
                } else {
                    0
                };
                if !m.take(0, work, alloc + roots.1) {
                    return Ok(false);
                }
                if !self.exit_step(&mut s, origin, reason, handler, superseded)? {
                    return Ok(false);
                }
            }
            ScriptPhase::Release { reason } => {
                if s.derived_bridge.is_some() && matches!(reason, ExitReason::Return) {
                    return self.compact_derived_commit(s, m);
                }
                let error = match reason {
                    ExitReason::Throw(info) => Some(info),
                    _ => None,
                };
                let Some(growth) = self.commit_root_admission(s.ret, error)? else {
                    return Ok(true);
                };
                let caught = self.active.caught.data.len() as u64;
                let next =
                    (caught + 2).max(self.release_cost(4) + self.commit_work(s.ret) + growth.0);
                // The scan is not a saved stage. Charge it with its successor
                // so the reported retry grant can complete this bounded action.
                if !m.take(0, caught + next, growth.1) {
                    return Ok(false);
                }
                let index = self.active.caught.data.iter().position(|c| {
                    #[cfg(test)]
                    self.work_probe
                        .release_catch_checks
                        .set(self.work_probe.release_catch_checks.get() + 1);
                    c.frame == s.id
                });
                if let Some(index) = index {
                    let record = self.active.caught.data.remove(index);
                    self.clear(s.env, 4 + record.region.0 * 2)?;
                    self.clear(s.env, 5 + record.region.0 * 2)?;
                    self.active.frames.data[top] = Frame::Script(s);
                    return Ok(true);
                }
                let value = self.value(s.env, 1)?;
                let cause = match reason {
                    ExitReason::Throw(info) if info.fault.is_some() => Some(self.value(s.env, 2)?),
                    _ => None,
                };
                let committed = if s.generator {
                    self.generator_release(s, value, cause, error)?
                } else {
                    self.commit(s.ret, value, cause, error)?
                };
                if !committed {
                    self.active.frames.data[top] = Frame::Script(s);
                    return Ok(true);
                }
                self.release_frame(Frame::Script(s))?;
                self.active.frames.data.remove(top);
                return Ok(true);
            }
        }
        self.active.frames.data[top] = Frame::Script(s);
        Ok(true)
    }
    pub(super) fn instruction_cost(
        &self,
        s: &ScriptFrame,
        op: Opcode,
        _a: u32,
        b: u32,
        c: u32,
    ) -> Result<(u64, u64), VmError> {
        let f = &self.program.package().functions[s.code.function.index()];
        Ok(match op {
            Opcode::LoadConst => (1, self.constant_allocation_bytes(ConstId(b))),
            Opcode::Clear2 => (2, 0),
            Opcode::Clear3 => (3, 0),
            Opcode::BindInit | Opcode::BindWrite => {
                (1 + self.type_checks.extra(f.bindings[_a as usize].ty), 0)
            }
            Opcode::CellWrite => {
                let contract = self
                    .heap
                    .cell_info(self.slots.read(at(s.regs, _a))?)?
                    .contract;
                (
                    1 + contract.type_id.map_or(0, |ty| self.type_checks.extra(ty)),
                    0,
                )
            }
            Opcode::Is => (1 + self.type_checks.extra(TypeId(c)), 0),
            Opcode::CheckType => (1 + self.type_checks.extra(TypeId(b)), 0),
            Opcode::Return => (
                1 + self
                    .type_checks
                    .extra(self.program.package().signatures[f.signature.index()].return_type),
                0,
            ),
            Opcode::Call | Opcode::CallNoWait => {
                let Descriptor::Call(d) = &f.descriptors[b as usize] else {
                    unreachable!()
                };
                let count = match d.arguments {
                    Arguments::Window(w) => w.len,
                    Arguments::Packed(r) => {
                        self.heap
                            .packed_array_len(self.slots.read(at(s.regs, r.0))?)?
                            / 2
                    }
                };
                let mut allocation = 0;
                let planned = self
                    .program
                    .native_call_plan(s.code.function, DescriptorId(b))
                    .is_some();
                let native_count = if planned { 0 } else { count };
                let cost = match d.target {
                    CallTarget::Function(id) => self.script_push_cost(id, native_count),
                    CallTarget::Native(id) => self.native_push_cost(id, native_count),
                    CallTarget::Register(r) => {
                        let target = self.slots.read(at(s.regs, r.0))?;
                        if self.is_text(target, ObjectKind::Class) {
                            allocation = Values::allocation_bytes(4);
                            (24, 0)
                        } else {
                            let cost = self
                                .closure_function(target)
                                .map(|id| self.script_push_cost(id, count))
                                .unwrap_or((1, 0));
                            if self.is_text(target, ObjectKind::BoundScript) {
                                let (work, bytes) = self.initializer_cost(target)?;
                                allocation = bytes;
                                (cost.0 + work, cost.1)
                            } else {
                                cost
                            }
                        }
                    }
                };
                (
                    cost.0
                        + self.active.unwinds.data.len() as u64
                        + 1
                        + 2 * self.program.native_call_lookup_work(),
                    allocation + cost.1,
                )
            }
            Opcode::Await => {
                let cost = self
                    .heap
                    .host_await(self.slots.read(at(s.regs, b))?)
                    .map(|id| self.native_push_cost(id, 1))
                    .unwrap_or((1, 0));
                (
                    cost.0
                        + self.active.unwinds.data.len() as u64
                        + 1
                        + 2 * self.program.native_call_lookup_work(),
                    cost.1,
                )
            }
            Opcode::MakeClosure => {
                let Descriptor::Closure { function, captures } = &f.descriptors[b as usize] else {
                    unreachable!()
                };
                if matches!(
                    self.program.package().functions[function.index()].kind,
                    FunctionKind::Native(_)
                ) {
                    (self.native_values.data.len().ilog2() as u64 + 2, 0)
                } else {
                    (
                        captures.len() as u64 + 1,
                        Values::allocation_bytes(captures.len() as u64 + 3),
                    )
                }
            }
            Opcode::MakeTuple | Opcode::MakeArray => (1, 0),
            Opcode::Throw | Opcode::Rethrow => (
                f.regions.len() as u64 + self.active.caught.data.len() as u64 + 3,
                0,
            ),
            Opcode::EndFinally => (self.release_cost(1), 0),
            Opcode::GlobalRead | Opcode::GlobalInit | Opcode::GlobalWrite => {
                let Descriptor::Global { module, global } = f.descriptors[b as usize] else {
                    unreachable!()
                };
                let binding = self.program.global_binding(module, global)?;
                let extra = if op == Opcode::GlobalRead || binding.imported {
                    0
                } else {
                    self.type_checks.extra(binding.ty)
                };
                (1 + extra, 0)
            }
            Opcode::MakeClass => (24, Values::allocation_bytes(9)),
            Opcode::SuperRef => (4, Values::allocation_bytes(2)),
            Opcode::PropertyRef => {
                let object = self.slots.read(at(s.regs, b))?;
                if object.tag() == ValueTag::Object
                    && matches!(
                        self.heap.info(object)?.kind,
                        ObjectKind::Instance | ObjectKind::Super
                    )
                {
                    let Descriptor::Property { key, .. } = f.descriptors[c as usize] else {
                        unreachable!()
                    };
                    let key = self.slots.read(at(s.regs, key.0))?;
                    let bytes = if key.tag() == ValueTag::Object {
                        self.heap.info(key)?.len
                    } else {
                        0
                    };
                    (
                        self.type_checks.symbol_work(bytes) + self.class_member_cost(object)?,
                        Values::allocation_bytes(4),
                    )
                } else {
                    (1, Values::allocation_bytes(4))
                }
            }
            Opcode::MemberGet | Opcode::MemberSet => {
                let object = self
                    .slots
                    .read(at(s.regs, if op == Opcode::MemberGet { b } else { _a }))?;
                let kind = self.heap.info(object)?.kind;
                if matches!(
                    kind,
                    ObjectKind::FaultEnvelope
                        | ObjectKind::FaultStack
                        | ObjectKind::FaultCause
                        | ObjectKind::CoreError
                ) {
                    let name = &self.program.package().symbols[c as usize];
                    return if op == Opcode::MemberSet {
                        Ok((1, 0))
                    } else {
                        self.heap
                            .fault_member_cost(object, name)
                            .map_err(Into::into)
                    };
                }
                if matches!(
                    kind,
                    ObjectKind::Class | ObjectKind::Instance | ObjectKind::Super
                ) {
                    let extra = if op == Opcode::MemberSet {
                        self.class_write_cost(object, SymbolId(c))?
                    } else {
                        0
                    };
                    return Ok((
                        self.class_member_cost(object)? + extra,
                        Values::allocation_bytes(3),
                    ));
                }
                (
                    1,
                    if matches!(kind, ObjectKind::Array | ObjectKind::Tuple) {
                        Values::allocation_bytes(2)
                    } else {
                        0
                    },
                )
            }
            Opcode::IterAdvance => (5, 0),
            Opcode::ArgsNew => (1, 0),
            Opcode::IterInit
            | Opcode::IterNext
            | Opcode::MakeRange
            | Opcode::MakeRangeInclusive => (4, Values::allocation_bytes(3)),
            Opcode::ChildrenPush | Opcode::ArrayPush => {
                let array = self.slots.read(at(s.regs, _a))?;
                let value = self.slots.read(at(s.regs, b))?;
                let len = self.heap.info(array)?.len;
                (
                    1,
                    if op == Opcode::ArrayPush || !value.same_bits(Value::NULL) {
                        Values::growth_bytes(len.into(), 1)
                    } else {
                        0
                    },
                )
            }
            Opcode::ArgsPush | Opcode::ArgsNamed => {
                let packed = self.slots.read(at(s.regs, _a))?;
                let len = self.heap.packed_array_len(packed)?;
                (2, Values::growth_bytes(len.into(), 2))
            }
            _ => (1, 0),
        })
    }
}
