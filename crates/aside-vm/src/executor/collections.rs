//! Collection instructions keep unpublished builders in the frame's rooted
//! return/work slot. PC advances only after publication; cancellation uses the
//! existing frame/return-slot teardown, so partially built values never escape.
use super::*;

impl Vm {
    pub(super) fn concat_step(
        &mut self,
        s: &mut ScriptFrame,
        dest: Reg,
        left: Reg,
        right: Reg,
        offset: u32,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        // The immutable operands remain rooted in their original registers until
        // publication, including when dest aliases one of those registers.
        let left_value = self.slots.read(at(s.regs, left.0))?;
        let right_value = self.slots.read(at(s.regs, right.0))?;
        let left_len = self.heap.info(left_value)?.len;
        let total = left_len + self.heap.info(right_value)?.len;
        let builder = self.value(s.env, 1)?;
        if offset == total {
            if !meter.take(0, 1, 0) {
                return Ok(false);
            }
            self.heap.seal_string(builder)?;
            match self.write(*s, dest, builder) {
                Ok(()) => {
                    self.clear(s.env, 1)?;
                    s.pc.0 += 1;
                    s.phase = ScriptPhase::Body;
                }
                Err(kind) => self.collection_fault(s, kind)?,
            }
            return Ok(true);
        }
        let (source, source_offset, remaining) = if offset < left_len {
            (left_value, offset, left_len - offset)
        } else {
            (right_value, offset - left_len, total - offset)
        };
        let count = (remaining as usize)
            .min(self.limits.heap.max_mutation_bytes as usize)
            .min(BYTE_CHUNK_LEN);
        let chunks = (u64::from(offset) + count as u64).div_ceil(BYTE_CHUNK_LEN as u64)
            - u64::from(offset).div_ceil(BYTE_CHUNK_LEN as u64);
        if !meter.take(0, count as u64 * 2 + 1, chunks * BYTE_CHUNK_BYTES) {
            return Ok(false);
        }
        self.text_scratch.data.resize(count, 0);
        let result = self
            .heap
            .read_bytes(source, source_offset, &mut self.text_scratch.data)
            .and_then(|_| self.heap.bytes_append(builder, &self.text_scratch.data));
        self.text_scratch.data.clear();
        match result {
            Ok(()) => {
                s.phase = ScriptPhase::Concat {
                    dest,
                    left,
                    right,
                    offset: offset + count as u32,
                }
            }
            Err(error) => self.collection_fault(s, heap_fault(error))?,
        }
        Ok(true)
    }

    pub(super) fn collection_fault(
        &mut self,
        s: &mut ScriptFrame,
        kind: FaultKind,
    ) -> Result<(), VmError> {
        s.table_work = None;
        if kind != FaultKind::UnhashableKey {
            self.put(s.env, 1, Value::NULL)?;
        }
        self.clear(s.env, 2)?;
        s.phase = ScriptPhase::Exit {
            origin: s.pc,
            reason: ExitReason::Throw(FaultSeed {
                kind,
                location: self.loc(*s, s.pc),
                fault: None,
                explicit: None,
                author_value: false,
            }),
            skip: None,
        };
        Ok(())
    }

    pub(super) fn sequence_step(
        &mut self,
        s: &mut ScriptFrame,
        dest: Reg,
        window: RegWindow,
        index: u32,
        tuple: bool,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        let extra = if index < window.len && (index as usize).is_multiple_of(VALUE_CHUNK_LEN) {
            VALUE_CHUNK_BYTES
        } else {
            0
        };
        if !meter.take(0, 1, extra) {
            return Ok(false);
        }
        let builder = self.value(s.env, 1)?;
        let result = if index == window.len {
            if tuple {
                self.heap.seal_tuple(builder)?;
            }
            self.write(*s, dest, builder)
        } else {
            self.read(*s, Reg(window.start.0 + index))
                .and_then(|value| self.heap.array_push(builder, value).map_err(heap_fault))
        };
        match result {
            Err(kind) => self.collection_fault(s, kind)?,
            Ok(()) if index == window.len => {
                self.clear(s.env, 1)?;
                s.pc.0 += 1;
                s.phase = ScriptPhase::Body;
            }
            Ok(()) => {
                s.phase = ScriptPhase::Sequence {
                    dest,
                    window,
                    index: index + 1,
                    tuple,
                }
            }
        }
        Ok(true)
    }

    fn sequence_index(&self, object: Value, index: Value, write: bool) -> Result<u32, FaultKind> {
        if object.tag() != ValueTag::Object {
            return Err(FaultKind::Type);
        }
        let info = self.heap.info(object).map_err(heap_fault)?;
        match info.kind {
            ObjectKind::Array => {}
            ObjectKind::Tuple if write => return Err(FaultKind::ReadOnly),
            ObjectKind::Tuple => {}
            _ => return Err(FaultKind::Type),
        }
        let index = index.as_int().ok_or(FaultKind::Type)?;
        let index = u32::try_from(index).map_err(|_| FaultKind::OutOfBounds)?;
        if index >= info.len {
            return Err(FaultKind::OutOfBounds);
        }
        Ok(index)
    }
    pub(super) fn sequence_get(&self, object: Value, index: Value) -> Result<Value, FaultKind> {
        let index = self.sequence_index(object, index, false)?;
        self.heap.get_field(object, index).map_err(heap_fault)
    }
    pub(super) fn sequence_set(
        &mut self,
        object: Value,
        index: Value,
        value: Value,
    ) -> Result<(), FaultKind> {
        let index = self.sequence_index(object, index, true)?;
        self.heap
            .set_field(object, index, value)
            .map_err(heap_fault)
    }
}

#[derive(Clone, Copy)]
pub(super) enum TableAction {
    Build {
        dest: Reg,
        descriptor: DescriptorId,
        field: u32,
    },
    Get(Reg),
    MemberGet {
        dest: Reg,
        symbol: SymbolId,
    },
    HasOwn(Reg),
    Set(Reg),
    SetHost {
        object: Reg,
        value: Reg,
    },
}
#[derive(Clone, Copy)]
pub(super) struct TableWork {
    action: TableAction,
    entry: u32,
    version: u64,
    matched: bool,
}

impl Vm {
    fn key_stack_push(&mut self, key: Value) -> Result<(), VmError> {
        let depth = self.active.equality_cursors.data.len() as u32;
        self.put(self.active.equality_roots.unwrap(), depth * 2, key)?;
        self.put(self.active.equality_roots.unwrap(), depth * 2 + 1, key)?;
        self.active.equality_cursors.data.push(0);
        Ok(())
    }
    pub(super) fn start_table(
        &mut self,
        s: &mut ScriptFrame,
        table: Value,
        key: Value,
        action: TableAction,
    ) -> Result<(), FaultKind> {
        if !self.is_text(table, ObjectKind::Table) {
            return Err(FaultKind::Type);
        }
        self.heap.table_len(table).map_err(heap_fault)?;
        let version = self.heap.info(table).map_err(heap_fault)?.content_version;
        self.put(s.env, 1, table).map_err(vm_fault)?;
        self.put(s.env, 2, key).map_err(vm_fault)?;
        s.table_work = Some(TableWork {
            action,
            entry: 0,
            version,
            matched: false,
        });
        self.key_stack_push(key).map_err(vm_fault)?;
        s.phase = ScriptPhase::TableValidate;
        Ok(())
    }
    pub(super) fn next_table_field(
        &mut self,
        s: &mut ScriptFrame,
        dest: Reg,
        descriptor: DescriptorId,
        field: u32,
    ) -> Result<(), VmError> {
        let Descriptor::Table(fields) = &self.program.package().functions[s.code.function.index()]
            .descriptors[descriptor.index()]
        else {
            unreachable!()
        };
        let table = self.value(s.env, 1)?;
        if field as usize == fields.len() {
            self.put(s.regs, dest.0, table)?;
            self.clear(s.env, 1)?;
            self.clear(s.env, 2)?;
            s.table_work = None;
            s.pc.0 += 1;
            s.phase = ScriptPhase::Body;
        } else {
            let key = match self.read(*s, fields[field as usize][0]) {
                Ok(key) => key,
                Err(kind) => return self.collection_fault(s, kind),
            };
            if let Err(kind) = self.start_table(
                s,
                table,
                key,
                TableAction::Build {
                    dest,
                    descriptor,
                    field,
                },
            ) {
                self.collection_fault(s, kind)?;
            }
        }
        Ok(())
    }
    pub(super) fn table_changed(&self, s: ScriptFrame) -> Result<bool, VmError> {
        let Some(work) = s.table_work else {
            return Ok(false);
        };
        Ok(self.heap.info(self.value(s.env, 1)?)?.content_version != work.version)
    }
    pub(super) fn table_step(
        &mut self,
        s: &mut ScriptFrame,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        let work = s.table_work.unwrap();
        let table = self.value(s.env, 1)?;
        let len = self.heap.info(table)?.len;
        let scan = matches!(s.phase, ScriptPhase::TableScan);
        let store = scan
            && (work.matched || work.entry == len)
            && !matches!(
                work.action,
                TableAction::Get(_) | TableAction::MemberGet { .. } | TableAction::HasOwn(_)
            );
        let fallback = scan
            && work.entry == len
            && !work.matched
            && matches!(work.action, TableAction::MemberGet { symbol, .. } if Builtin::lookup(ObjectKind::Table, &self.program.package().symbols[symbol.index()]).is_some());
        let extra = if fallback
            || (store
                && !matches!(work.action, TableAction::SetHost { .. })
                && work.entry == len
                && (len as usize * 2).is_multiple_of(VALUE_CHUNK_LEN))
        {
            VALUE_CHUNK_BYTES
        } else {
            0
        };
        if !meter.take(0, 1, extra) {
            return Ok(false);
        }
        if store
            && !work.matched
            && matches!(work.action, TableAction::Set(_))
            && s.effects.0 & Effects::ALLOCATE == 0
        {
            self.collection_fault(s, FaultKind::Effect)?;
            return Ok(true);
        }
        if self.table_changed(*s)? {
            // Key validation may still own an arbitrarily deep rooted stack.
            s.phase = ScriptPhase::EqualityCleanup {
                dest: Reg(0),
                negated: false,
                result: Err(FaultKind::ConcurrentModification),
            };
            return Ok(true);
        }
        if !scan {
            self.validate_key_step(s)?;
        } else if store || work.matched || work.entry == len {
            self.table_commit(s)?;
        } else {
            let key = self.value(s.env, 2)?;
            let (candidate, _) = self.heap.table_entry(table, work.entry)?;
            self.put(self.active.equality_roots.unwrap(), 0, key)?;
            self.put(self.active.equality_roots.unwrap(), 1, candidate)?;
            self.active.equality_cursors.data.push(0);
            s.phase = ScriptPhase::Equality {
                dest: Reg(0),
                negated: false,
            };
        }
        Ok(true)
    }
    fn validate_key_step(&mut self, s: &mut ScriptFrame) -> Result<(), VmError> {
        let depth = self.active.equality_cursors.data.len() as u32 - 1;
        let root = self.active.equality_roots.unwrap();
        let key = self.value(root, depth * 2)?;
        let mut done = true;
        let mut valid = true;
        match key.tag() {
            ValueTag::Null | ValueTag::Bool | ValueTag::Int => {}
            ValueTag::Float => valid = !key.as_float().unwrap().is_nan(),
            ValueTag::Object => {
                let info = self.heap.info(key)?;
                match info.kind {
                    ObjectKind::String | ObjectKind::Bytes => {}
                    ObjectKind::Tuple => {
                        let index = self.active.equality_cursors.data[depth as usize];
                        if index < info.len {
                            if self.active.equality_cursors.data.len()
                                == self.limits.max_unwinds as usize
                            {
                                s.phase = ScriptPhase::EqualityCleanup {
                                    dest: Reg(0),
                                    negated: false,
                                    result: Err(FaultKind::Resource),
                                };
                                return Ok(());
                            }
                            let child = self.heap.get_field(key, index)?;
                            self.active.equality_cursors.data[depth as usize] += 1;
                            self.key_stack_push(child)?;
                            done = false;
                        }
                    }
                    _ => valid = false,
                }
            }
        }
        if !valid {
            match self.heap.alloc_array(&[]) {
                Ok(path) => {
                    self.put(s.env, 1, path)?;
                    s.phase = ScriptPhase::KeyPath { index: 0 };
                }
                Err(error) => {
                    s.phase = ScriptPhase::EqualityCleanup {
                        dest: Reg(0),
                        negated: false,
                        result: Err(heap_fault(error)),
                    }
                }
            }
        } else if done {
            self.clear(root, depth * 2)?;
            self.clear(root, depth * 2 + 1)?;
            self.active.equality_cursors.data.pop();
            if self.active.equality_cursors.data.is_empty() {
                s.phase = ScriptPhase::TableScan;
            }
        }
        Ok(())
    }
    pub(super) fn table_comparison_done(
        &mut self,
        s: &mut ScriptFrame,
        result: Result<bool, FaultKind>,
    ) -> Result<(), VmError> {
        if let Err(kind) = result {
            return self.collection_fault(s, kind);
        }
        if self.table_changed(*s)? {
            return self.collection_fault(s, FaultKind::ConcurrentModification);
        }
        let work = s.table_work.as_mut().unwrap();
        work.matched = result.unwrap();
        if !work.matched {
            work.entry += 1;
        }
        s.phase = ScriptPhase::TableScan;
        Ok(())
    }
    fn table_commit(&mut self, s: &mut ScriptFrame) -> Result<(), VmError> {
        let work = s.table_work.unwrap();
        let table = self.value(s.env, 1)?;
        let key = self.value(s.env, 2)?;
        match work.action {
            TableAction::HasOwn(dest) => self.put(s.regs, dest.0, Value::bool(work.matched))?,
            TableAction::Get(dest) | TableAction::MemberGet { dest, .. } => {
                let value = if work.matched {
                    self.heap.table_entry(table, work.entry)?.1
                } else if let TableAction::MemberGet { symbol, .. } = work.action {
                    let Some(method) = Builtin::lookup(
                        ObjectKind::Table,
                        &self.program.package().symbols[symbol.index()],
                    ) else {
                        return self.collection_fault(s, FaultKind::MissingKey);
                    };
                    match self.bound_builtin(table, method, s.effects) {
                        Ok(value) => value,
                        Err(kind) => return self.collection_fault(s, kind),
                    }
                } else {
                    return self.collection_fault(s, FaultKind::MissingKey);
                };
                self.put(s.regs, dest.0, value)?;
            }
            TableAction::Set(reg) | TableAction::SetHost { value: reg, .. } => {
                if matches!(work.action, TableAction::SetHost { .. }) && !work.matched {
                    return self.collection_fault(s, FaultKind::MissingKey);
                }
                let value = match self.read(*s, reg) {
                    Ok(value) => value,
                    Err(kind) => return self.collection_fault(s, kind),
                };
                if let TableAction::SetHost { object, .. } = work.action {
                    let host = match self.read(*s, object) {
                        Ok(host) => host,
                        Err(kind) => return self.collection_fault(s, kind),
                    };
                    if let Err(error) = self.heap.validate_host_property(host, work.entry, value) {
                        return self.collection_fault(s, heap_fault(error));
                    }
                }
                if let Err(error) = self.heap.table_store(table, work.entry, key, value) {
                    return self.collection_fault(s, heap_fault(error));
                }
            }
            TableAction::Build {
                dest,
                descriptor,
                field,
            } => {
                let Descriptor::Table(fields) = &self.program.package().functions
                    [s.code.function.index()]
                .descriptors[descriptor.index()] else {
                    unreachable!()
                };
                let value = match self.read(*s, fields[field as usize][1]) {
                    Ok(value) => value,
                    Err(kind) => return self.collection_fault(s, kind),
                };
                if let Err(error) = self.heap.table_store(table, work.entry, key, value) {
                    return self.collection_fault(s, heap_fault(error));
                }
                return self.next_table_field(s, dest, descriptor, field + 1);
            }
        }
        s.table_work = None;
        self.clear(s.env, 1)?;
        self.clear(s.env, 2)?;
        s.pc.0 += 1;
        s.phase = ScriptPhase::Body;
        Ok(())
    }
}

impl Vm {
    pub(super) fn spread_step(
        &mut self,
        s: &mut ScriptFrame,
        index: u32,
        len: u32,
        version: u64,
        destination_version: u64,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        let packed = self.value(s.env, 1)?;
        let source = self.value(s.env, 2)?;
        let destination = self.heap.info(packed)?;
        let changed = destination.content_version != destination_version
            || self.heap.info(source)?.content_version != version;
        let shape = self.heap.packed_array_len(packed);
        let extra = if !changed
            && index < len
            && shape
                .as_ref()
                .is_ok_and(|length| (*length as usize).is_multiple_of(VALUE_CHUNK_LEN))
        {
            VALUE_CHUNK_BYTES
        } else {
            0
        };
        if !meter.take(0, 2, extra) {
            return Ok(false);
        }
        if changed {
            self.collection_fault(s, FaultKind::ConcurrentModification)?;
        } else if let Err(error) = shape {
            self.collection_fault(s, heap_fault(error))?;
        } else if shape.unwrap() / 2 >= self.limits.max_arguments && index < len {
            self.collection_fault(s, FaultKind::Resource)?;
        } else if index == len {
            self.clear(s.env, 1)?;
            self.clear(s.env, 2)?;
            s.pc.0 += 1;
            s.phase = ScriptPhase::Body;
        } else {
            let value = self.heap.sequence_get(source, index)?;
            let result = self.heap.packed_pair_push(packed, Value::int(-1), value);
            match result {
                Ok(()) => {
                    s.phase = ScriptPhase::Spread {
                        index: index + 1,
                        len,
                        version,
                        destination_version: self.heap.info(packed)?.content_version,
                    }
                }
                Err(error) => self.collection_fault(s, heap_fault(error))?,
            }
        }
        Ok(true)
    }
}

impl Vm {
    pub(super) fn key_path_step(
        &mut self,
        s: &mut ScriptFrame,
        index: u32,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        let len = self.active.equality_cursors.data.len() as u32 - 1;
        let extra = if index < len && (index as usize).is_multiple_of(VALUE_CHUNK_LEN) {
            VALUE_CHUNK_BYTES
        } else {
            0
        };
        if !meter.take(0, 1, extra) {
            return Ok(false);
        }
        let path = self.value(s.env, 1)?;
        if index == len {
            self.heap.seal_tuple(path)?;
            s.phase = ScriptPhase::EqualityCleanup {
                dest: Reg(0),
                negated: false,
                result: Err(FaultKind::UnhashableKey),
            };
        } else {
            let position = self.active.equality_cursors.data[index as usize] - 1;
            match self.heap.array_push(path, Value::int(position as i64)) {
                Ok(()) => s.phase = ScriptPhase::KeyPath { index: index + 1 },
                Err(error) => {
                    s.phase = ScriptPhase::EqualityCleanup {
                        dest: Reg(0),
                        negated: false,
                        result: Err(heap_fault(error)),
                    }
                }
            }
        }
        Ok(true)
    }
}
