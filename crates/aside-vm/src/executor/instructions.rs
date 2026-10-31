//! Bytecode instruction execution.
use super::*;

impl Vm {
    /// All opcode identities and operand positions come from bytecode::instruction_spec.
    pub(super) fn execute(
        &mut self,
        s: &mut ScriptFrame,
        op: Opcode,
        a: u32,
        b: u32,
        c: u32,
        next_pc: Pc,
    ) -> Result<bool, FaultKind> {
        let r = |n| Reg(n);
        let f = &self.program.package().functions[s.code.function.index()];
        if op == Opcode::MakeClosure {
            let Descriptor::Closure { function, .. } = &f.descriptors[b as usize] else {
                unreachable!()
            };
            if matches!(
                self.program.package().functions[function.index()].kind,
                FunctionKind::Native(_)
            ) {
                let index = self
                    .native_values
                    .data
                    .binary_search_by_key(function, |(id, _)| *id)
                    .unwrap();
                let value = self
                    .slots
                    .read(at(self.constants, self.native_values.data[index].1))
                    .map_err(heap_fault)?;
                self.write(s.regs, r(a), value)?;
                s.pc = next_pc;
                return Ok(true);
            }
        }
        if matches!(
            op,
            Opcode::CellWrite
                | Opcode::BindWrite
                | Opcode::GlobalWrite
                | Opcode::GlobalInit
                | Opcode::IndexSet
                | Opcode::MemberSet
        ) {
            let allowed = if s.context.derived {
                match op {
                    Opcode::BindWrite => match f.bindings[a as usize].storage {
                        BindingStorage::Value(_) => true,
                        BindingStorage::Cell(reg) => {
                            self.heap.derived_write_allowed(self.read(s.regs, reg)?)
                        }
                    },
                    Opcode::CellWrite | Opcode::IndexSet | Opcode::MemberSet => {
                        self.heap.derived_write_allowed(self.read(s.regs, r(a))?)
                    }
                    _ => false,
                }
            } else {
                s.effects.0 & Effects::WRITE != 0
            };
            if !allowed {
                return Err(FaultKind::Effect);
            }
        }
        if matches!(
            op,
            Opcode::PropertyRef
                | Opcode::MakeClass
                | Opcode::SuperRef
                | Opcode::CellNew
                | Opcode::MakeClosure
                | Opcode::MakeTuple
                | Opcode::MakeArray
                | Opcode::MakeTable
                | Opcode::ArgsNew
                | Opcode::ArgsPush
                | Opcode::ArgsNamed
                | Opcode::ArgsSpread
                | Opcode::ArraySpread
                | Opcode::TableSpread
                | Opcode::ArrayPush
                | Opcode::ChildrenPush
                | Opcode::TextNew
                | Opcode::TextPush
                | Opcode::MakeRange
                | Opcode::MakeRangeInclusive
                | Opcode::IterInit
                | Opcode::IterNext
        ) && s.effects.0 & Effects::ALLOCATE == 0
        {
            return Err(FaultKind::Effect);
        }
        match op {
            Opcode::MakeClass => {
                self.start_class(s, r(a), DescriptorId(b))?;
                return Ok(false);
            }
            Opcode::SuperRef => {
                let value = self.super_ref(*s)?;
                self.write(s.regs, r(a), value)?;
            }
            Opcode::TextNew => {
                let value = self.heap.alloc_byte_buffer(&[]).map_err(heap_fault)?;
                self.write(s.regs, r(a), value)?;
            }
            Opcode::TextPush => {
                let builder = self.read(s.regs, r(a))?;
                if self.heap.info(builder).map_err(heap_fault)?.kind != ObjectKind::ByteBuffer {
                    return Err(FaultKind::Type);
                }
                s.phase = ScriptPhase::TextPush {
                    builder: r(a),
                    input: r(b),
                    offset: 0,
                };
                return Ok(false);
            }
            Opcode::TextFinish => {
                let value = self.read(s.regs, r(b))?;
                let info = self.heap.info(value).map_err(heap_fault)?;
                if info.kind != ObjectKind::ByteBuffer {
                    return Err(FaultKind::Type);
                }
                s.phase = ScriptPhase::TextFinish {
                    dest: r(a),
                    input: r(b),
                    offset: 0,
                    version: info.content_version,
                    utf8: Default::default(),
                };
                return Ok(false);
            }
            Opcode::ChildrenPush => {
                let (array, child) = self.read_pair(s.regs, a, b)?;
                if !child.same_bits(Value::NULL) {
                    if !self.heap.is_composable_host(child).map_err(heap_fault)? {
                        return Err(FaultKind::Type);
                    }
                    self.heap.array_push(array, child).map_err(heap_fault)?;
                }
            }
            Opcode::ArrayPush => {
                let (array, value) = self.read_pair(s.regs, a, b)?;
                self.heap.array_push(array, value).map_err(heap_fault)?;
            }
            Opcode::TableSpread => {
                let table = self.read(s.regs, r(a))?;
                self.start_table_spread(s, table, r(b), collections::TableFinish::Instruction)?;
                return Ok(false);
            }
            Opcode::MakeRange | Opcode::MakeRangeInclusive => {
                let start = self.read(s.regs, r(b))?.as_int().ok_or(FaultKind::Type)?;
                let end = self.read(s.regs, r(c))?.as_int().ok_or(FaultKind::Type)?;
                let value = self
                    .heap
                    .alloc_record(
                        ObjectKind::Range,
                        &[
                            Value::int(start),
                            Value::int(end),
                            Value::bool(op == Opcode::MakeRangeInclusive),
                        ],
                    )
                    .map_err(heap_fault)?;
                self.write(s.regs, r(a), value)?;
            }
            Opcode::IterInit => {
                let source = self.read(s.regs, r(b))?;
                let iterator = self.iterator_new(source).map_err(heap_fault)?;
                self.write(s.regs, r(a), iterator)?;
            }
            Opcode::IterNext => {
                let iterator = self.read(s.regs, r(b))?;
                if self.is_text(iterator, ObjectKind::Generator) {
                    self.generator_begin(s, iterator, GeneratorAction::Pair(r(a)))?;
                    return Ok(false);
                }
                let value = self.iterator_next(iterator).map_err(heap_fault)?;
                self.write(s.regs, r(a), value)?;
            }
            Opcode::IterAdvance => {
                let iterator = self.read(s.regs, r(b))?;
                if self.is_text(iterator, ObjectKind::Generator) {
                    self.generator_begin(
                        s,
                        iterator,
                        GeneratorAction::Advance {
                            done: r(a),
                            value: r(c),
                        },
                    )?;
                    return Ok(false);
                }
                let step = self.iterator_peek(iterator).map_err(heap_fault)?;
                self.write(s.regs, r(a), Value::bool(step.done))?;
                self.write(s.regs, r(c), step.value)?;
                self.iterator_commit(iterator, step).map_err(heap_fault)?;
            }
            Opcode::IterClose => {
                let iterator = self.read(s.regs, r(a))?;
                if self.is_text(iterator, ObjectKind::Generator) {
                    self.generator_begin(s, iterator, GeneratorAction::Close(None))?;
                    return Ok(false);
                }
                if self.heap.info(iterator).map_err(heap_fault)?.kind != ObjectKind::Iterator {
                    return Err(FaultKind::Type);
                }
                self.heap
                    .set_field(iterator, 0, Value::NULL)
                    .map_err(heap_fault)?;
            }
            Opcode::MakeTuple | Opcode::MakeArray => {
                let Descriptor::Sequence(window) = f.descriptors[b as usize] else {
                    unreachable!()
                };
                let builder = self.heap.alloc_array(&[]).map_err(heap_fault)?;
                self.put(s.env, 1, builder).map_err(vm_fault)?;
                s.phase = ScriptPhase::Sequence {
                    dest: r(a),
                    window,
                    index: 0,
                    tuple: op == Opcode::MakeTuple,
                };
                return Ok(false);
            }
            Opcode::MakeTable => {
                let table = self.heap.alloc_table().map_err(heap_fault)?;
                self.put(s.env, 1, table).map_err(vm_fault)?;
                self.next_table_field(s, r(a), DescriptorId(b), 0)
                    .map_err(vm_fault)?;
                return Ok(false);
            }
            Opcode::PropertyRef => {
                let host = self.read(s.regs, r(b))?;
                let Descriptor::Property { key, writable } = self.program.package().functions
                    [s.code.function.index()]
                .descriptors[c as usize] else {
                    unreachable!()
                };
                let key = self.read(s.regs, key)?;
                if matches!(
                    self.heap.info(host).map_err(heap_fault)?.kind,
                    ObjectKind::Instance | ObjectKind::Super
                ) {
                    let reference = self.class_property_ref(host, key, writable)?;
                    self.write(s.regs, r(a), reference)?;
                    s.pc = next_pc;
                    return Ok(true);
                }
                let fields = self.heap.property_fields(host).map_err(heap_fault)?;
                self.start_table(
                    s,
                    fields,
                    key,
                    TableAction::PropertyRef {
                        dest: r(a),
                        object: r(b),
                        writable,
                    },
                )?;
                return Ok(false);
            }
            Opcode::MemberGet | Opcode::MemberSet => {
                let object = self.read(s.regs, r(if op == Opcode::MemberGet { b } else { a }))?;
                if matches!(
                    self.heap.info(object).map_err(heap_fault)?.kind,
                    ObjectKind::FaultEnvelope
                        | ObjectKind::FaultStack
                        | ObjectKind::FaultCause
                        | ObjectKind::CoreError
                ) {
                    if op == Opcode::MemberSet {
                        return Err(FaultKind::ReadOnly);
                    }
                    let value = self
                        .heap
                        .fault_member(object, &self.program.package().symbols[c as usize])
                        .map_err(heap_fault)?
                        .ok_or(FaultKind::MissingKey)?;
                    self.write(s.regs, r(a), value)?;
                    s.pc = next_pc;
                    return Ok(true);
                }
                if matches!(
                    self.heap.info(object).map_err(heap_fault)?.kind,
                    ObjectKind::Class | ObjectKind::Instance | ObjectKind::Super
                ) {
                    let value = if op == Opcode::MemberSet {
                        Some(self.read(s.regs, r(b))?)
                    } else {
                        None
                    };
                    let result = self.class_member(object, SymbolId(c), value, s.effects)?;
                    if op == Opcode::MemberGet {
                        self.write(s.regs, r(a), result)?;
                    }
                    s.pc = next_pc;
                    return Ok(true);
                }
                if op == Opcode::MemberGet && self.heap.is_property_ref(object) {
                    if self.program.package().symbols[c as usize] != "value" {
                        return Err(FaultKind::MissingKey);
                    }
                    let (property, _) = self.heap.property_ref(object).map_err(heap_fault)?;
                    self.heap
                        .observe_read(property.host, Some(property.field))
                        .map_err(heap_fault)?;
                    let value = self.heap.host_property(property).map_err(heap_fault)?;
                    self.write(s.regs, r(a), value)?;
                    s.pc = next_pc;
                    return Ok(true);
                }
                if op == Opcode::MemberGet {
                    let kind = self.heap.info(object).map_err(heap_fault)?.kind;
                    if matches!(
                        kind,
                        ObjectKind::Array
                            | ObjectKind::Tuple
                            | ObjectKind::String
                            | ObjectKind::Generator
                            | ObjectKind::Bytes
                    ) {
                        let method =
                            Builtin::lookup(kind, &self.program.package().symbols[c as usize])
                                .ok_or(FaultKind::MissingKey)?;
                        let value = self.bound_builtin(object, method, s.effects)?;
                        self.write(s.regs, r(a), value)?;
                        s.pc = next_pc;
                        return Ok(false);
                    }
                }
                let host = self.is_text(object, ObjectKind::Host);
                let object = if host {
                    match self.heap.host_fields(object) {
                        Ok(fields) => fields,
                        Err(HeapError::RetiredHost) if op == Opcode::MemberGet => {
                            let key = self.value(self.constants, c).map_err(vm_fault)?;
                            self.start_host_method(s, r(b), r(a), key)?;
                            return Ok(false);
                        }
                        Err(error) => return Err(heap_fault(error)),
                    }
                } else {
                    object
                };
                let key = self.value(self.constants, c).map_err(vm_fault)?;
                let action = if op == Opcode::MemberGet {
                    if host {
                        TableAction::GetHost {
                            object: r(b),
                            dest: r(a),
                        }
                    } else {
                        TableAction::MemberGet {
                            dest: r(a),
                            symbol: SymbolId(c),
                        }
                    }
                } else if host {
                    TableAction::SetHost {
                        object: r(a),
                        value: r(b),
                    }
                } else {
                    TableAction::Set(r(b))
                };
                self.start_table(s, object, key, action)?;
                return Ok(false);
            }
            Opcode::IndexGet => {
                let (object, index) = self.read_pair(s.regs, b, c)?;
                if self.is_text(object, ObjectKind::Host) {
                    let fields = match self.heap.host_fields(object) {
                        Ok(fields) => fields,
                        Err(HeapError::RetiredHost) => {
                            self.start_host_method(s, r(b), r(a), index)?;
                            return Ok(false);
                        }
                        Err(error) => return Err(heap_fault(error)),
                    };
                    self.start_table(
                        s,
                        fields,
                        index,
                        TableAction::GetHost {
                            object: r(b),
                            dest: r(a),
                        },
                    )?;
                    return Ok(false);
                }
                if self.is_text(object, ObjectKind::Table) {
                    self.start_table(s, object, index, TableAction::Get(r(a)))?;
                    return Ok(false);
                }
                let value = self.sequence_get(object, index)?;
                self.write(s.regs, r(a), value)?;
            }
            Opcode::IndexSet => {
                let (object, index) = self.read_pair(s.regs, a, b)?;
                if self.is_text(object, ObjectKind::Host) {
                    let fields = self.heap.host_fields(object).map_err(heap_fault)?;
                    self.start_table(
                        s,
                        fields,
                        index,
                        TableAction::SetHost {
                            object: r(a),
                            value: r(c),
                        },
                    )?;
                    return Ok(false);
                }
                if self.is_text(object, ObjectKind::Table) {
                    self.start_table(s, object, index, TableAction::Set(r(c)))?;
                    return Ok(false);
                }
                let value = self.read(s.regs, r(c))?;
                self.sequence_set(object, index, value)?;
            }
            Opcode::LoadConst => {
                if let Some(value) = self.start_constant(ConstId(b), s.env, [1, 2])? {
                    self.write(s.regs, r(a), value)?;
                } else {
                    s.phase = ScriptPhase::Constant(constants::ConstantWork {
                        dest: r(a),
                        id: ConstId(b),
                        index: 0,
                    });
                    return Ok(false);
                }
            }
            Opcode::Move => {
                let v = self.read(s.regs, r(b))?;
                self.write(s.regs, r(a), v)?;
            }
            Opcode::Clear | Opcode::Clear2 | Opcode::Clear3 => {
                match op {
                    Opcode::Clear => self.slots.clear(&mut self.heap, at(s.regs, a)),
                    Opcode::Clear2 => self.slots.clear_registers(&mut self.heap, s.regs, [a, b]),
                    _ => self
                        .slots
                        .clear_registers(&mut self.heap, s.regs, [a, b, c]),
                }
                .map_err(heap_fault)?;
            }
            Opcode::CellNew => {
                let binding = f.bindings[a as usize].clone();
                let cell = self
                    .heap
                    .alloc_language_cell(self.contract(
                        ModuleId(s.module.0),
                        binding.ty,
                        binding.mutable,
                    ))
                    .map_err(heap_fault)?;
                self.write(s.regs, binding.storage.reg(), cell)?;
            }
            Opcode::CellRead => {
                let cell = self.read(s.regs, r(b))?;
                let v = self.heap.cell_read(cell).map_err(heap_fault)?;
                self.write(s.regs, r(a), v)?;
            }
            Opcode::CellWrite => {
                let (cell, v) = self.read_pair(s.regs, a, b)?;
                let contract = self.heap.cell_info(cell).map_err(heap_fault)?.contract;
                self.check_cell_type(contract, v)?;
                self.heap.cell_write(cell, v).map_err(heap_fault)?;
            }
            Opcode::BindInit | Opcode::BindWrite => {
                let binding = f.bindings[a as usize].clone();
                let v = self.read(s.regs, r(b))?;
                self.check_type(binding.ty, v)?;
                match binding.storage {
                    BindingStorage::Cell(reg) => {
                        let cell = self.read(s.regs, reg)?;
                        if op == Opcode::BindInit {
                            self.heap.cell_initialize(cell, v)
                        } else {
                            self.heap.cell_write(cell, v)
                        }
                        .map_err(heap_fault)?;
                    }
                    BindingStorage::Value(reg) => {
                        let old = self.slots.read(at(s.regs, reg.0));
                        if op == Opcode::BindInit {
                            if old.is_ok() {
                                return Err(FaultKind::InvalidState);
                            }
                        } else {
                            old.map_err(heap_fault)?;
                            if !binding.mutable {
                                return Err(FaultKind::ReadOnly);
                            }
                        }
                        self.write(s.regs, reg, v)?;
                    }
                }
            }
            Opcode::BindRead => {
                let binding = f.bindings[b as usize].clone();
                let mut v = self.read(s.regs, binding.storage.reg())?;
                if matches!(binding.storage, BindingStorage::Cell(_)) {
                    v = self.heap.cell_read(v).map_err(heap_fault)?;
                }
                self.write(s.regs, r(a), v)?;
            }
            Opcode::CaptureRead => {
                if !s.closure {
                    return Err(FaultKind::InvalidState);
                }
                let closure = self
                    .script_closure(self.slots.read(at(s.env, 0)).map_err(heap_fault)?)
                    .map_err(vm_fault)?;
                let v = self.heap.get_field(closure, b + 3).map_err(heap_fault)?;
                self.write(s.regs, r(a), v)?;
            }
            Opcode::GlobalRead | Opcode::GlobalInit | Opcode::GlobalWrite => {
                let Descriptor::Global { module, global } = f.descriptors[b as usize] else {
                    unreachable!()
                };
                let binding = self
                    .program
                    .global_binding(module, global)
                    .expect("verified module binding");
                let cell = self
                    .slots
                    .read(at(self.globals, binding.offset))
                    .map_err(heap_fault)?;
                if op == Opcode::GlobalRead {
                    let v = self.heap.cell_read(cell).map_err(|error| {
                        if s.context.init && matches!(error, HeapError::UninitializedCell) {
                            FaultKind::ModuleInitializationCycle
                        } else {
                            heap_fault(error)
                        }
                    })?;
                    self.write(s.regs, r(a), v)?;
                } else {
                    let v = self.read(s.regs, r(a))?;
                    if binding.imported {
                        return Err(FaultKind::ReadOnly);
                    }
                    self.check_type(binding.ty, v)?;
                    if op == Opcode::GlobalInit {
                        self.heap.cell_initialize(cell, v)
                    } else {
                        self.heap.cell_write(cell, v)
                    }
                    .map_err(heap_fault)?;
                }
            }
            Opcode::Jump | Opcode::BranchBool | Opcode::BranchNull => {
                s.pc = scalars::branch_target(op, [a, b, c], |register| {
                    self.read(s.regs, r(register))
                })?;
                return Ok(false);
            }
            Opcode::Neg | Opcode::Pos | Opcode::Not | Opcode::BitNot => {
                let v = unary(op, self.read(s.regs, r(b))?)?;
                self.write(s.regs, r(a), v)?;
            }
            Opcode::Is => {
                let v = self.read(s.regs, r(b))?;
                let result = self.check_type(TypeId(c), v).is_ok();
                self.write(s.regs, r(a), Value::bool(result))?;
            }
            Opcode::CheckType => self.check_type(TypeId(b), self.read(s.regs, r(a))?)?,
            Opcode::Add
            | Opcode::Sub
            | Opcode::Mul
            | Opcode::Div
            | Opcode::Rem
            | Opcode::BitAnd
            | Opcode::BitOr
            | Opcode::BitXor
            | Opcode::Shl
            | Opcode::Shr
            | Opcode::Eq
            | Opcode::Ne
            | Opcode::Lt
            | Opcode::Le
            | Opcode::Gt
            | Opcode::Ge => {
                let (left, right) = self.read_pair(s.regs, b, c)?;
                if matches!(op, Opcode::Eq | Opcode::Ne)
                    && left.tag() == ValueTag::Object
                    && right.tag() == ValueTag::Object
                    && matches!(
                        self.heap.info(left).map_err(heap_fault)?.kind,
                        ObjectKind::Tuple | ObjectKind::String | ObjectKind::Bytes
                    )
                {
                    self.active.equality_cursors.data.clear();
                    self.put(self.active.equality_roots.unwrap(), 0, left)
                        .map_err(vm_fault)?;
                    self.put(self.active.equality_roots.unwrap(), 1, right)
                        .map_err(vm_fault)?;
                    self.active.equality_cursors.data.push(0);
                    s.phase = ScriptPhase::Equality {
                        dest: r(a),
                        negated: op == Opcode::Ne,
                    };
                    return Ok(false);
                }
                if op == Opcode::Add
                    && self.is_text(left, ObjectKind::String)
                    && self.is_text(right, ObjectKind::String)
                {
                    if s.effects.0 & Effects::ALLOCATE == 0 {
                        return Err(FaultKind::Effect);
                    }
                    self.heap
                        .info(left)
                        .map_err(heap_fault)?
                        .len
                        .checked_add(self.heap.info(right).map_err(heap_fault)?.len)
                        .ok_or(FaultKind::Resource)?;
                    let builder = self.heap.alloc_byte_buffer(&[]).map_err(heap_fault)?;
                    self.put(s.env, 1, builder).map_err(vm_fault)?;
                    s.phase = ScriptPhase::Concat {
                        dest: r(a),
                        left: r(b),
                        right: r(c),
                        offset: 0,
                    };
                    return Ok(false);
                }
                let v = binary(op, left, right)?;
                self.write(s.regs, r(a), v)?;
            }
            Opcode::MakeClosure => {
                let Descriptor::Closure { function, captures } = &f.descriptors[b as usize] else {
                    unreachable!()
                };
                let function = *function;
                self.scratch.data.clear();
                self.scratch.data.extend([
                    Value::int(function.0 as i64),
                    Value::int(s.code.version as i64),
                    Value::int(self.program.package().functions[function.index()].module.0 as i64),
                ]);
                for source in captures {
                    let value = match *source {
                        CaptureSource::Binding(id) => {
                            let binding = &f.bindings[id.index()];
                            self.read(s.regs, binding.storage.reg())?
                        }
                        CaptureSource::Capture(id) => {
                            let closure = self
                                .script_closure(self.slots.read(at(s.env, 0)).map_err(heap_fault)?)
                                .map_err(vm_fault)?;
                            self.heap.get_field(closure, id.0 + 3).map_err(heap_fault)?
                        }
                    };
                    self.scratch.data.push(value);
                }
                let closure = self
                    .heap
                    .alloc_record(ObjectKind::Closure, &self.scratch.data)
                    .map_err(heap_fault)?;
                self.scratch.data.clear();
                self.write(s.regs, r(a), closure)?;
            }
            Opcode::ArgsNew => {
                let packed = self.heap.alloc_array(&[]).map_err(heap_fault)?;
                self.write(s.regs, r(a), packed)?;
            }
            Opcode::ArgsPush | Opcode::ArgsNamed => {
                let (packed, v) = self.read_pair(s.regs, a, b)?;
                let len = self.heap.packed_array_len(packed).map_err(heap_fault)?;
                if len / 2 >= self.limits.max_arguments {
                    return Err(FaultKind::Resource);
                }
                self.heap
                    .packed_pair_push(
                        packed,
                        Value::int(if op == Opcode::ArgsNamed {
                            c as i64
                        } else {
                            -1
                        }),
                        v,
                    )
                    .map_err(heap_fault)?;
            }
            Opcode::ArgsSpread | Opcode::ArraySpread => {
                let (packed, source) = self.read_pair(s.regs, a, b)?;
                let arguments = op == Opcode::ArgsSpread;
                let named = arguments && self.is_text(source, ObjectKind::Table);
                if !named
                    && !self.is_text(source, ObjectKind::Array)
                    && !self.is_text(source, ObjectKind::Tuple)
                {
                    return Err(FaultKind::Type);
                }
                let info = self.heap.info(source).map_err(heap_fault)?;
                if !self.is_text(packed, ObjectKind::Array) {
                    return Err(FaultKind::Type);
                }
                let destination_version =
                    self.heap.info(packed).map_err(heap_fault)?.content_version;
                if arguments {
                    let count = self.heap.packed_array_len(packed).map_err(heap_fault)? / 2;
                    if info.len > self.limits.max_arguments.saturating_sub(count) {
                        return Err(FaultKind::Resource);
                    }
                }
                self.put(s.env, 1, packed).map_err(vm_fault)?;
                self.put(s.env, 2, source).map_err(vm_fault)?;
                s.phase = ScriptPhase::Spread(collections::SpreadCapture {
                    kind: if named {
                        collections::CaptureKind::NamedArguments
                    } else if arguments {
                        collections::CaptureKind::PositionalArguments
                    } else {
                        collections::CaptureKind::Elements
                    },
                    index: 0,
                    len: info.len,
                    version: info.content_version,
                    destination_version,
                });
                return Ok(false);
            }
            Opcode::Call | Opcode::CallNoWait => {
                return self.execute_call(s, op, a, b, next_pc, None);
            }
            Opcode::Await => {
                self.start_await(s, r(a), r(b))?;
                return Ok(true);
            }
            Opcode::Yield => {
                self.generator_yield(s, r(a), r(b))?;
                return Ok(false);
            }
            Opcode::Return => {
                let v = self.read(s.regs, r(a))?;
                let signature = self.program.package().functions[s.code.function.index()].signature;
                self.check_type(
                    self.program.package().signatures[signature.index()].return_type,
                    v,
                )?;
                self.slots
                    .write(&mut self.heap, at(s.env, 1), v)
                    .map_err(heap_fault)?;
                s.phase = ScriptPhase::Exit {
                    origin: s.pc,
                    reason: ExitReason::Return,
                    skip: None,
                };
                return Ok(false);
            }
            Opcode::PatternRequire => {
                if !self.read(s.regs, r(a))?.as_bool().ok_or(FaultKind::Type)? {
                    return Err(FaultKind::PatternMismatch);
                }
            }
            Opcode::MatchFail => return Err(FaultKind::MatchError),
            Opcode::Throw => {
                let v = self.read(s.regs, r(a))?;
                self.put(s.env, 1, v).map_err(vm_fault)?;
                self.clear(s.env, 2).map_err(vm_fault)?;
                s.phase = ScriptPhase::Exit {
                    origin: s.pc,
                    reason: ExitReason::Throw(FaultSeed {
                        kind: FaultKind::Thrown,
                        location: self.loc(*s, s.pc),
                        fault: None,
                        explicit: None,
                        author_value: true,
                        awaited: None,
                    }),
                    skip: None,
                };
                return Ok(false);
            }
            Opcode::Rethrow => {
                let region = f
                    .regions
                    .iter()
                    .enumerate()
                    .filter_map(|(id, r)| {
                        r.catch
                            .as_ref()
                            .filter(|c| c.range.contains(s.pc))
                            .map(|c| (RegionId(id as u32), c.range.end.0 - c.range.start.0))
                    })
                    .min_by_key(|(_, width)| *width)
                    .map(|(id, _)| id)
                    .ok_or(FaultKind::InvalidState)?;
                let seed = self
                    .active
                    .caught
                    .data
                    .iter()
                    .find(|c| c.frame == s.id && c.region == region)
                    .map(|c| c.info)
                    .ok_or(FaultKind::InvalidState)?;
                let id = seed.fault.ok_or(FaultKind::InvalidState)?;
                let value = self.heap.fault_value(id).map_err(heap_fault)?;
                self.put(s.env, 1, value).map_err(vm_fault)?;
                self.put(s.env, 2, id.as_value()).map_err(vm_fault)?;
                s.phase = ScriptPhase::Exit {
                    origin: s.pc,
                    reason: ExitReason::Throw(seed),
                    skip: None,
                };
                return Ok(false);
            }
            Opcode::Leave => {
                self.slots
                    .write(&mut self.heap, at(s.env, 1), Value::NULL)
                    .map_err(heap_fault)?;
                s.phase = ScriptPhase::Exit {
                    origin: s.pc,
                    reason: ExitReason::Leave(Pc(a)),
                    skip: None,
                };
                return Ok(false);
            }
            Opcode::EndFinally => {
                let Some(u) = self
                    .active
                    .unwinds
                    .data
                    .last()
                    .copied()
                    .filter(|u| u.frame == s.id && u.region == RegionId(a))
                else {
                    return Err(FaultKind::InvalidState);
                };
                let value = self.slots.read(at(u.value, 0)).map_err(heap_fault)?;
                self.slots
                    .write(&mut self.heap, at(s.env, 1), value)
                    .map_err(heap_fault)?;
                if matches!(u.reason,ExitReason::Throw(info) if info.fault.is_some()) {
                    let cause = self.value(u.value, 1).map_err(vm_fault)?;
                    self.put(s.env, 2, cause).map_err(vm_fault)?;
                } else {
                    self.clear(s.env, 2).map_err(vm_fault)?;
                }
                self.slots
                    .release(&mut self.heap, u.value)
                    .map_err(heap_fault)?;
                self.active.unwinds.data.pop();
                s.phase = ScriptPhase::Exit {
                    origin: u.origin,
                    reason: u.reason,
                    skip: Some(u.region),
                };
                return Ok(false);
            }
            _ => return Err(FaultKind::InvalidState),
        }
        s.pc = next_pc;
        Ok(false)
    }
    pub(super) fn is_text(&self, v: Value, kind: ObjectKind) -> bool {
        v.tag() == ValueTag::Object && self.heap.info(v).is_ok_and(|i| i.kind == kind)
    }
}
