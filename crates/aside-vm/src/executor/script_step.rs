//! Resumable script preparation and instruction admission.
use super::*;

impl Vm {
    pub(super) fn script_step(
        &mut self,
        s: &mut ScriptFrame,
        m: &mut Meter,
    ) -> Result<bool, VmError> {
        #[cfg(feature = "execution-counts")]
        self.execution_counts.step(&s.phase);
        if matches!(s.phase, ScriptPhase::Body) {
            return self.script_body_step(s, m);
        }
        self.script_continuation_step(s, m)
    }

    fn script_continuation_step(
        &mut self,
        s: &mut ScriptFrame,
        m: &mut Meter,
    ) -> Result<bool, VmError> {
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
                if !self.generator_create_step(s, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::GeneratorCapture {
                part,
                index,
                created,
            } => {
                if !self.generator_capture_step(s, part, index, created, m)? {
                    return Ok(false);
                }
                if self.active.frames.data.len() <= top {
                    return Ok(true);
                }
            }
            ScriptPhase::GeneratorStart { action, call } => {
                self.publish_script_frame(top, s);
                if !self.generator_start_step(s, action, call, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::GeneratorRestore {
                part,
                index,
                closing,
            } => {
                if !self.generator_restore_step(s, part, index, closing, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::Class(step) => {
                if !self.class_step(s, step, m)? {
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
                    Err(error) => self.preparation_fault(s, heap_fault(error))?,
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
                self.publish_script_frame(top, s);
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
                        self.preparation_fault(s, kind)?;
                    }
                }
            }
            ScriptPhase::Body => unreachable!("Body dispatches before continuation work"),
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
                                Err(kind) => self.collection_fault(s, kind)?,
                            }
                        }
                    }
                    constants::ConstantStep::Fault(kind) => {
                        if parameter.is_some() {
                            self.preparation_fault(s, kind)?;
                        } else {
                            self.collection_fault(s, kind)?;
                        }
                    }
                }
            }
            ScriptPhase::Spread(work) => {
                if !self.spread_step(s, work, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::TableSpread(work) => {
                if !self.table_spread_step(s, work, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::KeyPath { index } => {
                if !self.key_path_step(s, index, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::TableValidate | ScriptPhase::TableScan => {
                if !self.table_step(s, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::HostMethod { object, dest } => {
                if !self.host_method_step(s, object, dest, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::Sequence {
                dest,
                window,
                index,
                tuple,
            } => {
                if !self.sequence_step(s, dest, window, index, tuple, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::TextFinish { .. } => {
                if !self.text_finish_step(s, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::TextPush {
                builder,
                input,
                offset,
            } => {
                if !self.text_push_step(s, builder, input, offset, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::Builtin(_) => {
                if !self.builtin_step(s, m)? {
                    self.publish_script_frame(top, s);
                    return Ok(false);
                }
            }
            ScriptPhase::Search(work) => {
                if !self.search_step(s, work, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::Sort(work) => {
                if !self.sort_step(s, work, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::Bulk(work) => {
                if !self.bulk_step(s, work, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::TextBuiltin(work) => {
                if !self.text_builtin_step(s, work, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::Concat {
                dest,
                left,
                right,
                offset,
            } => {
                if !self.concat_step(s, dest, left, right, offset, m)? {
                    return Ok(false);
                }
            }
            ScriptPhase::Equality { .. } | ScriptPhase::EqualityCleanup { .. } => {
                if !self.equality_phase_step(s, m)? {
                    return Ok(false);
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
                let handler = self.select_exit_handler(*s, origin, reason, skip)?;
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
                let superseded = self.superseded_unwind(*s, origin, reason, handler);
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
                if !self.exit_step(s, origin, reason, handler, superseded)? {
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
                    self.publish_script_frame(top, s);
                    return Ok(true);
                }
                let value = self.value(s.env, 1)?;
                let cause = match reason {
                    ExitReason::Throw(info) if info.fault.is_some() => Some(self.value(s.env, 2)?),
                    _ => None,
                };
                let committed = if s.generator {
                    self.generator_release(*s, value, cause, error)?
                } else {
                    self.commit(s.ret, value, cause, error)?
                };
                if !committed {
                    self.publish_script_frame(top, s);
                    return Ok(true);
                }
                self.release_frame(Frame::Script(*s))?;
                self.active.frames.data.remove(top);
                return Ok(true);
            }
        }
        self.publish_script_frame(top, s);
        Ok(true)
    }
    pub(super) fn instruction_cost(
        &self,
        s: &ScriptFrame,
        op: Opcode,
        _a: u32,
        b: u32,
        c: u32,
        plan: &mut InstructionPlan,
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
                            let function = self.closure_function(target);
                            let cost = function
                                .as_ref()
                                .map(|id| self.script_push_cost(*id, count))
                                .unwrap_or((1, 0));
                            if let Ok(function) = function
                                && let Some(callee) = self.prepare_callee(target, function)
                            {
                                *plan = InstructionPlan::Callee(callee);
                                cost
                            } else if self.is_text(target, ObjectKind::BoundScript) {
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
                let info = self.heap.info(object)?;
                let kind = info.kind;
                #[cfg(feature = "execution-counts")]
                if op == Opcode::MemberGet && s.member_symbol.is_some() {
                    let counter = match kind {
                        ObjectKind::Table => execution_counts::Counter::MemberGetTableCosts,
                        ObjectKind::Host => execution_counts::Counter::MemberGetHostCosts,
                        ObjectKind::Class | ObjectKind::Instance | ObjectKind::Super => {
                            execution_counts::Counter::MemberGetClassCosts
                        }
                        ObjectKind::PropertyRef => {
                            execution_counts::Counter::MemberGetPropertyCosts
                        }
                        _ => execution_counts::Counter::MemberGetOtherCosts,
                    };
                    self.execution_counts.add(counter, 1);
                }
                if op == Opcode::MemberGet
                    && s.member_symbol.is_some()
                    && let Some(read) = self.prepare_named_read(object, info)
                {
                    *plan = InstructionPlan::NamedRead(read);
                    return Ok((1, 0));
                }
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
                    let prepare = op == Opcode::MemberGet;
                    #[cfg(test)]
                    let prepare = prepare && !self.work_probe.general_class_reads;
                    if prepare
                        && let Ok((target, property)) = self.class_read_plan(
                            object,
                            SymbolId(c),
                            s.member_symbol.map(|_| (s.code.function, s.pc)),
                        )
                    {
                        let work = target.work();
                        *plan = InstructionPlan::ClassRead(target, property);
                        // Three-slot bound methods currently use inline Values;
                        // this preserves their existing payload admission.
                        return Ok((work, Values::allocation_bytes(3)));
                    }
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
