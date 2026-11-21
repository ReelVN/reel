//! Bound methods share argument validation and resumable execution.
use super::*;
mod bulk;
pub(super) use bulk::BulkWork;
mod sort;
pub(super) use sort::SortWork;
mod search;
pub(super) use search::SearchWork;
mod predicate;
pub(super) use predicate::PredicateWork;
mod strings;
mod table_entry;
pub(super) use strings::TextWork;

pub(super) use crate::BuiltinMethod as Builtin;

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub(super) struct BuiltinCall {
    descriptor: DescriptorId,
    dest: Reg,
    method: Builtin,
    count: u32,
    index: u32,
    binding: ArgumentBinding,
    // Fixed by these declarations, not a limit on script function arity.
    arguments: [Option<u32>; 2],
    prepared: bool,
}

impl Vm {
    fn push_builtin_callback(
        &mut self,
        s: &mut ScriptFrame,
        function: FunctionId,
        callback: Value,
        argc: u32,
        phase: ScriptPhase,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        let (initializer_work, bytes) = if self.is_text(callback, ObjectKind::BoundScript) {
            self.initializer_cost(callback)?
        } else {
            (0, 0)
        };
        let (call_work, call_bytes) = self.script_push_cost(function, argc);
        if !meter.take(0, call_work + initializer_work + 6, bytes + call_bytes) {
            return Ok(false);
        }
        s.phase = phase;
        self.publish_script_frame(self.index(s.id)?, s);
        if let Err(error) = self.push_script(
            function,
            Some(callback),
            ArgSource::BuiltinCallback { frame: s.id },
            argc,
            s.owner,
            inherit(s.context, Context::NoWait),
            s.effects,
            ReturnTo::BuiltinCallback {
                frame: s.id,
                call: s.pc,
            },
        ) {
            self.collection_fault(s, vm_fault(error))?;
        }
        Ok(true)
    }

    pub(super) fn builtin_callback_argument(
        &self,
        frame: FrameId,
        index: u32,
    ) -> Result<(Value, Value), VmError> {
        let Frame::Script(s) = self.active.frames.data[self.index(frame)?] else {
            return Err(VmError::Contract("builtin callback frame"));
        };
        match s.phase {
            ScriptPhase::Sort(work) => self.sort_argument(&s, work, index),
            ScriptPhase::Predicate(_) => Ok((Value::int(-1), self.value(s.env, 2)?)),
            _ => Err(VmError::Contract("builtin callback continuation")),
        }
    }

    pub(super) fn bound_builtin(
        &mut self,
        receiver: Value,
        method: Builtin,
        effects: Effects,
    ) -> Result<Value, FaultKind> {
        if effects.0 & Effects::ALLOCATE == 0 {
            return Err(FaultKind::Effect);
        }
        self.heap
            .alloc_record(
                ObjectKind::BoundBuiltin,
                &[receiver, Value::int(method as i64)],
            )
            .map_err(heap_fault)
    }

    pub(super) fn start_builtin(
        &mut self,
        s: &mut ScriptFrame,
        callee: Value,
        descriptor: DescriptorId,
        dest: Reg,
        count: u32,
    ) -> Result<(), FaultKind> {
        let receiver = self.heap.get_field(callee, 0).map_err(heap_fault)?;
        let method = Builtin::decode(
            self.heap
                .get_field(callee, 1)
                .map_err(heap_fault)?
                .as_int()
                .ok_or(FaultKind::Type)?,
        )?;
        if method.mutates()
            && self
                .heap
                .collection_is_readonly(receiver)
                .map_err(heap_fault)?
        {
            return Err(FaultKind::ReadOnly);
        }
        if method.mutates()
            && (if s.context.derived {
                !self.heap.derived_write_allowed(receiver)
            } else {
                s.effects.0 & Effects::WRITE == 0
            })
        {
            return Err(FaultKind::Effect);
        }
        if method.mutates() {
            self.heap
                .observe_write(receiver, None)
                .map_err(heap_fault)?;
        }
        // Point queries subscribe at lookup commit, after the key is known.
        if !method.mutates() && !matches!(method, Builtin::HasOwn | Builtin::Get) {
            self.heap.observe_read(receiver, None).map_err(heap_fault)?;
        }
        self.put(s.env, 1, receiver).map_err(vm_fault)?;
        s.phase = ScriptPhase::Builtin(BuiltinCall {
            descriptor,
            dest,
            method,
            count,
            index: 0,
            binding: ArgumentBinding::new(0),
            arguments: [None; 2],
            prepared: false,
        });
        Ok(())
    }

    fn builtin_argument(
        &self,
        s: &ScriptFrame,
        call: BuiltinCall,
        parameter: usize,
    ) -> Result<Value, VmError> {
        match call.arguments[parameter] {
            Some(index) => self
                .argument(
                    ArgSource::Caller {
                        frame: s.id,
                        descriptor: call.descriptor,
                    },
                    index,
                )
                .map(|(_, value)| value),
            None => Ok(Value::NULL),
        }
    }

    fn builtin_finish(
        &mut self,
        s: &mut ScriptFrame,
        call: BuiltinCall,
        result: Result<Value, FaultKind>,
    ) -> Result<(), VmError> {
        match result.and_then(|value| self.write(s.regs, call.dest, value)) {
            Ok(()) => {
                self.clear(s.env, 1)?;
                self.clear(s.env, 2)?;
                s.pc = s.instruction_end;
                s.phase = ScriptPhase::Body;
            }
            Err(kind) => self.collection_fault(s, kind)?,
        }
        Ok(())
    }

    pub(super) fn builtin_step(
        &mut self,
        s: &mut ScriptFrame,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        let ScriptPhase::Builtin(mut call) = s.phase else {
            unreachable!()
        };
        let receiver = self.value(s.env, 1)?;
        let Descriptor::Call(desc) = &self.program.package().functions[s.code.function.index()]
            .descriptors[call.descriptor.index()]
        else {
            unreachable!()
        };
        let source = if let Some(contract) = desc.contract {
            ArgSource::CheckedCaller {
                frame: s.id,
                descriptor: call.descriptor,
                contract,
            }
        } else {
            ArgSource::Caller {
                frame: s.id,
                descriptor: call.descriptor,
            }
        };
        if !call.prepared {
            let (parameters, required) = call.method.parameters();
            if call.binding.index < call.count {
                let index = call.binding.index;
                let (name, _) = self.argument(source, index)?;
                let result = self.argument_position(
                    name,
                    ParameterNames::Builtin(call.method),
                    &mut call.binding,
                    meter,
                )?;
                if matches!(result, ArgumentStep::Ready(Ok(_)))
                    && matches!(source, ArgSource::CheckedCaller { .. })
                {
                    if !meter.take(0, self.argument_work(source, index), 0) {
                        s.phase = ScriptPhase::Builtin(call);
                        return Ok(false);
                    }
                    if let Err(error) = self.checked_argument(source, index) {
                        self.collection_fault(s, language_fault(error)?)?;
                        return Ok(true);
                    }
                }
                let binding = match result {
                    ArgumentStep::More => {
                        s.phase = ScriptPhase::Builtin(call);
                        return Ok(true);
                    }
                    ArgumentStep::Blocked => {
                        s.phase = ScriptPhase::Builtin(call);
                        return Ok(false);
                    }
                    ArgumentStep::Ready(result) => result.and_then(|position| {
                        if call.method.declaration().rest.is_some() && name.as_int() == Some(-1) {
                            return Ok(());
                        }
                        match Some(position as usize).filter(|p| *p < parameters.len()) {
                            None => Err(FaultKind::UnknownArgument),
                            Some(p) if call.arguments[p].is_some() => {
                                Err(FaultKind::DuplicateArgument)
                            }
                            Some(p) => {
                                call.arguments[p] = Some(index);
                                Ok(())
                            }
                        }
                    }),
                };
                if let Err(kind) = binding {
                    self.collection_fault(s, kind)?;
                } else {
                    call.binding.advance(name);
                    s.phase = ScriptPhase::Builtin(call);
                }
                return Ok(true);
            }
            if !meter.take(0, 3, 0) {
                return Ok(false);
            }
            if call.arguments[..required].iter().any(Option::is_none) {
                self.collection_fault(s, FaultKind::MissingArgument)?;
                return Ok(true);
            }
            call.prepared = true;
            call.index = 0;
            s.phase = ScriptPhase::Builtin(call);
            return Ok(true);
        }
        let extra = if matches!(call.method, Builtin::Split | Builtin::Replace) {
            Values::allocation_bytes(4)
        } else if matches!(call.method, Builtin::Append) && call.index < call.count {
            Values::growth_bytes(self.heap.info(receiver)?.len.into(), 1)
        } else {
            0
        };
        if !meter.take(0, 3, extra) {
            return Ok(false);
        }
        let result = match call.method {
            Builtin::GeneratorNext | Builtin::GeneratorClose => {
                let action = if call.method == Builtin::GeneratorNext {
                    GeneratorAction::Pair(call.dest)
                } else {
                    GeneratorAction::Close(Some(call.dest))
                };
                if let Err(kind) = self.generator_begin(s, receiver, action) {
                    self.collection_fault(s, kind)?;
                }
                return Ok(true);
            }
            Builtin::Find | Builtin::Contains => {
                self.start_search(s, call)?;
                return Ok(true);
            }
            Builtin::FirstWhere | Builtin::Any => {
                if let Err(kind) = self.start_predicate(s, call) {
                    self.collection_fault(s, kind)?;
                }
                return Ok(true);
            }
            Builtin::Sort => {
                if let Err(kind) = self.start_sort(s, call) {
                    self.collection_fault(s, kind)?;
                }
                return Ok(true);
            }
            Builtin::Len | Builtin::ByteLen => {
                Ok(Value::int(i64::from(self.heap.info(receiver)?.len)))
            }
            Builtin::Pop => self
                .heap
                .array_pop(receiver)
                .map_err(heap_fault)
                .and_then(|v| v.ok_or(FaultKind::OutOfBounds)),
            Builtin::Top => self
                .heap
                .sequence_len(receiver)?
                .checked_sub(1)
                .ok_or(FaultKind::OutOfBounds)
                .and_then(|i| self.heap.sequence_get(receiver, i).map_err(heap_fault)),
            Builtin::Swap => {
                let a = self.builtin_argument(s, call, 0)?;
                let b = self.builtin_argument(s, call, 1)?;
                index(a)
                    .and_then(|a| {
                        index(b)
                            .and_then(|b| self.heap.array_swap(receiver, a, b).map_err(heap_fault))
                    })
                    .map(|()| receiver)
            }
            Builtin::HasOwn | Builtin::Get | Builtin::TableRemove | Builtin::GetOrInsertWith => {
                let key = self.builtin_argument(s, call, 0)?;
                let action = if matches!(call.method, Builtin::Get) {
                    TableAction::GetDefault {
                        dest: call.dest,
                        descriptor: call.descriptor,
                        fallback: call.arguments[1],
                    }
                } else if matches!(call.method, Builtin::TableRemove) {
                    TableAction::Remove(call)
                } else if matches!(call.method, Builtin::GetOrInsertWith) {
                    TableAction::EntryOrInsert {
                        call,
                        filled: false,
                    }
                } else {
                    TableAction::HasOwn(call.dest)
                };
                if let Err(kind) = self.start_table(s, receiver, key, action) {
                    self.collection_fault(s, kind)?;
                }
                return Ok(true);
            }
            Builtin::Append if call.index < call.count => {
                if s.effects.0 & Effects::ALLOCATE == 0 {
                    self.collection_fault(s, FaultKind::Effect)?;
                    return Ok(true);
                }
                let (_, value) = self.argument(source, call.index)?;
                match self.heap.array_push(receiver, value) {
                    Ok(()) => {
                        call.index += 1;
                        s.phase = ScriptPhase::Builtin(call);
                    }
                    Err(error) => self.collection_fault(s, heap_fault(error))?,
                }
                return Ok(true);
            }
            Builtin::Append => Ok(receiver),
            Builtin::ScalarCount
            | Builtin::SliceBytes
            | Builtin::SliceScalars
            | Builtin::StartsWith
            | Builtin::EndsWith
            | Builtin::StripPrefix
            | Builtin::StripSuffix
            | Builtin::Join
            | Builtin::Split
            | Builtin::Replace
            | Builtin::AsciiLowercase
            | Builtin::AsciiUppercase
            | Builtin::Trim
            | Builtin::TrimStart
            | Builtin::TrimEnd => {
                if let Err(kind) = self.start_text_builtin(s, call) {
                    self.collection_fault(s, kind)?;
                }
                return Ok(true);
            }
            _ => {
                if let Err(kind) = self.start_bulk(s, call) {
                    self.collection_fault(s, kind)?;
                }
                return Ok(true);
            }
        };
        self.builtin_finish(s, call, result)?;
        Ok(true)
    }

    fn range_bounds(&self, range: Value) -> Result<(u32, u32), FaultKind> {
        if !self.is_text(range, ObjectKind::Range) {
            return Err(FaultKind::Type);
        }
        let begin = index(self.heap.get_field(range, 0).map_err(heap_fault)?)?;
        let mut end = index(self.heap.get_field(range, 1).map_err(heap_fault)?)?;
        if self
            .heap
            .get_field(range, 2)
            .map_err(heap_fault)?
            .as_bool()
            .ok_or(FaultKind::Type)?
        {
            end = end.checked_add(1).ok_or(FaultKind::OutOfBounds)?;
        }
        if begin > end {
            return Err(FaultKind::OutOfBounds);
        }
        Ok((begin, end))
    }
}

fn index(value: Value) -> Result<u32, FaultKind> {
    let value = value.as_int().ok_or(FaultKind::Type)?;
    u32::try_from(value).map_err(|_| FaultKind::OutOfBounds)
}
