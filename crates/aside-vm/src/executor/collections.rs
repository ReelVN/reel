//! Collection instructions keep unpublished builders in the frame's rooted
//! return/work slot. PC advances only after publication; cancellation uses the
//! existing frame/return-slot teardown, so partially built values never escape.
use super::builtins::BuiltinCall;
use super::*;
mod spread;
pub(super) use spread::{CaptureKind, SpreadCapture, TableFinish, TableSpread};

impl Vm {
    pub(super) fn start_host_method(
        &mut self,
        frame: &mut ScriptFrame,
        object: Reg,
        dest: Reg,
        key: Value,
    ) -> Result<(), FaultKind> {
        self.put(frame.env, 2, key).map_err(vm_fault)?;
        frame.phase = ScriptPhase::HostMethod { object, dest };
        Ok(())
    }

    pub(super) fn host_method_step(
        &mut self,
        frame: &mut ScriptFrame,
        object: Reg,
        dest: Reg,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        let host = self.value(frame.regs, object.0)?;
        let key = self.value(frame.env, 2)?;
        let name = self.host_member_symbol(frame);
        let bytes = if name.is_none() && key.tag() == ValueTag::Object {
            self.heap.info(key)?.len
        } else {
            0
        };
        let lookup =
            1 + if name.is_none() {
                self.type_checks.symbol_work(bytes)
            } else {
                0
            } + self.heap.host_method_work(host)?;
        if !meter.admits(0, lookup, 0) {
            return Ok(false);
        }
        let function = match self.shared_host_method(host, key, name) {
            Ok(Some(function)) => function,
            result => {
                assert!(meter.take(0, lookup, 0));
                let kind = match result {
                    Err(error) => heap_fault(error),
                    _ => FaultKind::InvalidState,
                };
                self.collection_fault(frame, kind)?;
                return Ok(true);
            }
        };
        let signature = self.program.package().functions[function.index()].signature;
        let receiver = self.program.package().signatures[signature.index()].parameters[0].ty;
        if !meter.take(
            0,
            lookup + 4 + self.type_checks.extra(receiver),
            Values::allocation_bytes(4),
        ) {
            meter.usage.work += lookup;
            return Ok(false);
        }
        let result = if frame.effects.0 & Effects::ALLOCATE == 0 {
            Err(FaultKind::Effect)
        } else {
            self.bind_native(host, function).map_err(vm_fault)
        };
        match result {
            Ok(value) => {
                self.put(frame.regs, dest.0, value)?;
                self.clear(frame.env, 2)?;
                frame.pc = frame.instruction_end;
                frame.phase = ScriptPhase::Body;
            }
            Err(kind) => self.collection_fault(frame, kind)?,
        }
        Ok(true)
    }

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
            match self.write(s.regs, dest, builder) {
                Ok(()) => {
                    self.clear(s.env, 1)?;
                    s.pc = s.instruction_end;
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
        let bytes = Bytes::growth_bytes(offset.into(), count as u64);
        if !meter.take(0, count as u64 * 2 + 1, bytes) {
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
        self.abort_class(*s)?;
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
                awaited: None,
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
        let extra = if index < window.len {
            Values::growth_bytes(index.into(), 1)
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
            self.write(s.regs, dest, builder)
        } else {
            self.read(s.regs, Reg(window.start.0 + index))
                .and_then(|value| self.heap.array_push(builder, value).map_err(heap_fault))
        };
        match result {
            Err(kind) => self.collection_fault(s, kind)?,
            Ok(()) if index == window.len => {
                self.clear(s.env, 1)?;
                s.pc = s.instruction_end;
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
            ObjectKind::Tuple | ObjectKind::Bytes if write => return Err(FaultKind::ReadOnly),
            ObjectKind::Tuple | ObjectKind::Bytes => {}
            _ => return Err(FaultKind::Type),
        }
        let index = index.as_int().ok_or(FaultKind::Type)?;
        let index = u32::try_from(index).map_err(|_| FaultKind::OutOfBounds)?;
        if index >= info.len {
            return Err(FaultKind::OutOfBounds);
        }
        Ok(index)
    }
    pub(super) fn sequence_get(&mut self, object: Value, index: Value) -> Result<Value, FaultKind> {
        self.heap.observe_read(object, None).map_err(heap_fault)?;
        let index = self.sequence_index(object, index, false)?;
        if self.heap.info(object).map_err(heap_fault)?.kind == ObjectKind::Bytes {
            let mut byte = [0];
            self.heap
                .read_bytes(object, index, &mut byte)
                .map_err(heap_fault)?;
            Ok(Value::int(i64::from(byte[0])))
        } else {
            self.heap.get_field(object, index).map_err(heap_fault)
        }
    }
    pub(super) fn sequence_set(
        &mut self,
        object: Value,
        index: Value,
        value: Value,
    ) -> Result<(), FaultKind> {
        if self
            .heap
            .collection_is_readonly(object)
            .map_err(heap_fault)?
        {
            return Err(FaultKind::ReadOnly);
        }
        self.heap.observe_write(object, None).map_err(heap_fault)?;
        let index = self.sequence_index(object, index, true)?;
        self.heap
            .set_field(object, index, value)
            .map_err(heap_fault)
    }
}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub(super) enum TableAction {
    Spread(TableSpread),
    Build {
        dest: Reg,
        descriptor: DescriptorId,
        field: u32,
    },
    Get(Reg),
    PropertyRef {
        dest: Reg,
        object: Reg,
        writable: bool,
    },
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
    GetHost {
        object: Reg,
        dest: Reg,
    },
    GetDefault {
        dest: Reg,
        descriptor: DescriptorId,
        // The caller retains argument roots through key validation and lookup.
        fallback: Option<u32>,
    },
    Remove(BuiltinCall),
}
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub(super) struct TableWork {
    action: TableAction,
    entry: u32,
    version: u64,
    matched: bool,
}

#[cfg(test)]
impl TableWork {
    pub(super) fn matched(self) -> bool {
        self.matched
    }
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
        let version = self.table_version(table, action).map_err(vm_fault)?;
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
        let table = self.value(s.env, 1)?;
        s.table_work = None;
        let Some(source) = self.table_source(*s, descriptor, field) else {
            self.put(s.regs, dest.0, table)?;
            self.clear(s.env, 1)?;
            self.clear(s.env, 2)?;
            s.table_work = None;
            s.pc = s.instruction_end;
            s.phase = ScriptPhase::Body;
            return Ok(());
        };
        match source {
            TableSource::Spread(source) => {
                if let Err(kind) = self.start_table_spread(
                    s,
                    table,
                    source,
                    TableFinish::Build {
                        dest,
                        descriptor,
                        field: field + 1,
                    },
                ) {
                    self.collection_fault(s, kind)?;
                }
            }
            TableSource::Pair(pair) => {
                let key = match self.read(s.regs, pair[0]) {
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
        }
        Ok(())
    }
    fn table_source(
        &self,
        s: ScriptFrame,
        descriptor: DescriptorId,
        field: u32,
    ) -> Option<TableSource> {
        match &self.program.package().functions[s.code.function.index()].descriptors
            [descriptor.index()]
        {
            Descriptor::Table(fields) => fields.get(field as usize).copied().map(TableSource::Pair),
            Descriptor::TableSources(sources) => sources.get(field as usize).copied(),
            _ => unreachable!(),
        }
    }
    fn table_version(&self, table: Value, action: TableAction) -> Result<u64, VmError> {
        let info = self.heap.info(table)?;
        // Point lookups commit against the current slot after comparing stable
        // keys. Value overwrites cannot invalidate that scan. Bulk operations
        // retain their complete source-content snapshot.
        Ok(match action {
            TableAction::Get(_)
            | TableAction::MemberGet { .. }
            | TableAction::GetDefault { .. }
            | TableAction::GetHost { .. }
            | TableAction::Set(_)
            | TableAction::SetHost { .. }
            | TableAction::HasOwn(_)
            | TableAction::PropertyRef { .. } => info.structure_version,
            TableAction::Build { .. } | TableAction::Spread(_) | TableAction::Remove(_) => {
                info.content_version
            }
        })
    }

    pub(super) fn table_changed(&self, s: ScriptFrame) -> Result<bool, VmError> {
        let Some(work) = s.table_work else {
            return Ok(false);
        };
        let source_changed = if let TableAction::Spread(spread) = work.action {
            self.heap
                .info(self.value(s.regs, spread.source.0)?)?
                .content_version
                != spread.version
        } else {
            false
        };
        Ok(source_changed
            || self.table_version(self.value(s.env, 1)?, work.action)? != work.version)
    }
    pub(super) fn table_step(
        &mut self,
        s: &mut ScriptFrame,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        let work = s.table_work.unwrap();
        let table = self.value(s.env, 1)?;
        if self.table_changed(*s)? {
            if !meter.take(0, 1, 0) {
                return Ok(false);
            }
            // Validate the saved entry before resolving or refreshing its bound
            // source. Key validation may still own a rooted comparison stack.
            s.phase = ScriptPhase::EqualityCleanup {
                dest: Reg(0),
                negated: false,
                result: Err(FaultKind::ConcurrentModification),
            };
            return Ok(true);
        }
        let len = self.heap.info(table)?.len;
        // Source keys are unique. Entries appended by this spread cannot
        // match a later source key, so only scan the original destination.
        let scan_len = match work.action {
            TableAction::Spread(spread) => spread.prefix_len,
            _ => len,
        };
        let scan = matches!(s.phase, ScriptPhase::TableScan);
        let hint = if scan && !work.matched && work.entry == 0 {
            self.host_member_symbol(s).and_then(|symbol| {
                let (entry, trusted) =
                    self.member_hints
                        .get(s.code.function, s.pc, table, work.version)?;
                let bytes = self.program.package().symbols[symbol.index()].len();
                (entry < len && bytes <= member_hints::MAX_KEY_BYTES)
                    .then_some((entry, symbol, bytes, trusted))
            })
        } else {
            None
        };
        let store = scan
            && (work.matched || work.entry == scan_len)
            && !matches!(
                work.action,
                TableAction::Get(_)
                    | TableAction::GetDefault { .. }
                    | TableAction::GetHost { .. }
                    | TableAction::MemberGet { .. }
                    | TableAction::HasOwn(_)
                    | TableAction::Remove(_)
            );
        let fallback = scan
            && work.entry == len
            && !work.matched
            && matches!(work.action, TableAction::MemberGet { symbol, .. } if Builtin::lookup(ObjectKind::Table, &self.program.package().symbols[symbol.index()]).is_some());
        let extra = if (work.matched
            && matches!(work.action, TableAction::Remove(_))
            && self.heap.is_model_table(table)?)
            || (store && work.matched && matches!(work.action, TableAction::PropertyRef { .. }))
            || fallback
        {
            Values::allocation_bytes(4)
        } else if store && !matches!(work.action, TableAction::SetHost { .. }) && !work.matched {
            Values::growth_bytes(u64::from(len) * 2, 2) + self.heap.model_append_bytes(table)?
        } else {
            0
        };
        let mut cost = (1, extra);
        if let Some((_, _, bytes, trusted)) = hint {
            // Bounded metadata reads, short-key bytes and comparison are paid
            // before probing. A mismatch keeps the original ordered scan.
            cost.0 += if trusted { 1 } else { 3 + bytes as u64 };
        }
        let mut host_method = None;
        let mut host_lookup_work = 0;
        if scan
            && !work.matched
            && work.entry == len
            && let TableAction::GetHost { object, .. } | TableAction::SetHost { object, .. } =
                work.action
        {
            let host = self.value(s.regs, object.0)?;
            let key = self.value(s.env, 2)?;
            let name = self.host_member_symbol(s);
            let bytes = if name.is_none() && key.tag() == ValueTag::Object {
                self.heap.info(key)?.len
            } else {
                0
            };
            cost.0 += if name.is_none() {
                self.type_checks.symbol_work(bytes)
            } else {
                0
            } + self.heap.host_method_work(host)?;
            if !meter.admits(0, cost.0, 0) {
                return Ok(false);
            }
            host_lookup_work = cost.0;
            host_method = match self.shared_host_method(host, key, name) {
                Ok(method) => method,
                Err(error) => {
                    assert!(meter.take(0, cost.0, 0));
                    self.collection_fault(s, heap_fault(error))?;
                    return Ok(true);
                }
            };
            if matches!(work.action, TableAction::GetHost { .. })
                && let Some(function) = host_method
            {
                cost.1 += Values::allocation_bytes(4);
                let signature = self.program.package().functions[function.index()].signature;
                let receiver =
                    self.program.package().signatures[signature.index()].parameters[0].ty;
                cost.0 += 4 + self.type_checks.extra(receiver);
            }
        }
        let property = if scan && work.matched {
            match work.action {
                TableAction::GetHost { object, .. } => Some(crate::PropertyId {
                    host: self.value(s.regs, object.0)?,
                    field: work.entry,
                }),
                TableAction::Get(_)
                | TableAction::GetDefault { .. }
                | TableAction::MemberGet { .. }
                    if self.heap.is_model_table(table)? =>
                {
                    Some(crate::PropertyId {
                        host: table,
                        field: self.heap.model_field(table, work.entry)?,
                    })
                }
                _ => None,
            }
        } else {
            None
        };
        let refresh = if let Some(property) = property {
            match self.reactive_read_plan(property, &mut cost) {
                Ok(read) => read,
                Err(error) => {
                    self.collection_fault(s, vm_fault(error))?;
                    return Ok(true);
                }
            }
        } else {
            None
        };
        if !meter.take(0, cost.0, cost.1) {
            // The lookup already ran under its own admission check. Preserve
            // the full retry requirement while accounting for that work.
            meter.usage.work += host_lookup_work;
            return Ok(false);
        }
        if let Some((entry, symbol, _, trusted)) = hint
            && (trusted || self.member_hint_matches(table, entry, symbol)?)
        {
            let lookup = s.table_work.as_mut().unwrap();
            lookup.entry = entry;
            lookup.matched = true;
            #[cfg(test)]
            self.work_probe
                .member_hint_hits
                .set(self.work_probe.member_hint_hits.get() + 1);
            // The following ordinary lookup step still checks its version,
            // resolves reactive reads and admits the original commit work.
            return Ok(true);
        }
        if let Some(read) = refresh {
            let index = self.index(s.id)?;
            self.active.frames.data[index] = Frame::Script(*s);
            if let Err(error) = self.push_reactive_read(*s, read) {
                self.collection_fault(s, vm_fault(error))?;
            }
            return Ok(true);
        }
        if store
            && !work.matched
            && matches!(work.action, TableAction::Set(_))
            && s.effects.0 & Effects::ALLOCATE == 0
        {
            self.collection_fault(s, FaultKind::Effect)?;
            return Ok(true);
        }
        if !scan {
            self.validate_key_step(s)?;
        } else if store || work.matched || work.entry == scan_len {
            self.table_commit(s, host_method)?;
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
    fn table_commit(
        &mut self,
        s: &mut ScriptFrame,
        host_method: Option<FunctionId>,
    ) -> Result<(), VmError> {
        let work = s.table_work.unwrap();
        let table = self.value(s.env, 1)?;
        let key = self.value(s.env, 2)?;
        if work.matched
            && let Some(symbol) = self.host_member_symbol(s)
            && self.program.package().symbols[symbol.index()].len() <= member_hints::MAX_KEY_BYTES
        {
            self.member_hints
                .remember(s.code.function, s.pc, work.entry, table, work.version);
        }
        match work.action {
            TableAction::Remove(call) => {
                if !work.matched {
                    return self.collection_fault(s, FaultKind::MissingKey);
                }
                if let Err(kind) = self.start_table_remove(s, call, work.entry) {
                    return self.collection_fault(s, kind);
                }
                s.table_work = None;
                return Ok(());
            }
            TableAction::Spread(mut spread) => {
                let source = self.value(s.regs, spread.source.0)?;
                let value = self.heap.table_entry(source, spread.index)?.1;
                let position = if work.matched {
                    work.entry
                } else {
                    self.heap.table_len(table)?
                };
                if let Err(error) = self.heap.table_store(table, position, key, value) {
                    return self.collection_fault(s, heap_fault(error));
                }
                spread.index += 1;
                spread.destination_version = self.heap.info(table)?.content_version;
                s.table_work = None;
                self.clear(s.env, 2)?;
                s.phase = ScriptPhase::TableSpread(spread);
                return Ok(());
            }
            TableAction::PropertyRef {
                dest,
                object,
                writable,
            } => {
                if !work.matched {
                    return self.collection_fault(s, FaultKind::MissingKey);
                }
                let host = self.value(s.regs, object.0)?;
                match self.heap.alloc_property_ref(
                    crate::PropertyId {
                        host,
                        field: self.heap.model_field(host, work.entry)?,
                    },
                    writable,
                ) {
                    Ok(reference) => self.put(s.regs, dest.0, reference)?,
                    Err(error) => return self.collection_fault(s, heap_fault(error)),
                }
            }
            TableAction::HasOwn(dest) => {
                self.heap.observe_read(table, None)?;
                self.put(s.regs, dest.0, Value::bool(work.matched))?;
            }
            TableAction::GetHost { object, dest } => {
                let host = self.value(s.regs, object.0)?;
                if !work.matched {
                    let Some(function) = host_method else {
                        return self.collection_fault(s, FaultKind::MissingKey);
                    };
                    if s.effects.0 & Effects::ALLOCATE == 0 {
                        return self.collection_fault(s, FaultKind::Effect);
                    }
                    let value = match self.bind_native(host, function) {
                        Ok(value) => value,
                        Err(error) => return self.collection_fault(s, vm_fault(error)),
                    };
                    self.put(s.regs, dest.0, value)?;
                    s.table_work = None;
                    self.clear(s.env, 1)?;
                    self.clear(s.env, 2)?;
                    s.pc = s.instruction_end;
                    s.phase = ScriptPhase::Body;
                    return Ok(());
                }
                let property = crate::PropertyId {
                    host,
                    field: work.entry,
                };
                // Revalidate liveness and read the current value at commit. The
                // rooted fields table alone may outlive a retired host.
                let value = match self.heap.host_property(property) {
                    Ok(value) => value,
                    Err(error) => return self.collection_fault(s, heap_fault(error)),
                };
                self.heap.observe_read(host, Some(work.entry))?;
                self.put(s.regs, dest.0, value)?;
            }
            TableAction::Get(dest)
            | TableAction::GetDefault { dest, .. }
            | TableAction::MemberGet { dest, .. } => {
                let value = if work.matched {
                    self.heap
                        .observe_read(table, Some(self.heap.model_field(table, work.entry)?))?;
                    self.heap.table_entry(table, work.entry)?.1
                } else if let TableAction::GetDefault {
                    descriptor,
                    fallback,
                    ..
                } = work.action
                {
                    // Missing keys depend on the collection so insertion also
                    // invalidates a derived read. Null values remain present.
                    self.heap.observe_read(table, None)?;
                    match fallback {
                        Some(index) => {
                            self.argument(
                                ArgSource::Caller {
                                    frame: s.id,
                                    descriptor,
                                },
                                index,
                            )?
                            .1
                        }
                        None => Value::NULL,
                    }
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
                if let TableAction::SetHost { .. } = work.action
                    && !work.matched
                {
                    let kind = if host_method.is_some() {
                        FaultKind::ReadOnly
                    } else {
                        FaultKind::MissingKey
                    };
                    return self.collection_fault(s, kind);
                }
                let value = match self.read(s.regs, reg) {
                    Ok(value) => value,
                    Err(kind) => return self.collection_fault(s, kind),
                };
                if self.heap.is_property_ref(value)
                    && s.effects.0 & Effects::ALLOCATE == 0
                    && (matches!(work.action, TableAction::SetHost { .. })
                        || self.heap.is_model_table(table)?)
                {
                    return self.collection_fault(s, FaultKind::Effect);
                }
                if let Err(error) = self
                    .heap
                    .observe_write(table, Some(self.heap.model_field(table, work.entry)?))
                {
                    return self.collection_fault(s, heap_fault(error));
                }
                let result = if let TableAction::SetHost { object, .. } = work.action {
                    let host = self.value(s.regs, object.0)?;
                    self.heap.write_host_property(
                        crate::PropertyId {
                            host,
                            field: work.entry,
                        },
                        value,
                        true,
                    )
                } else {
                    self.heap.table_store(table, work.entry, key, value)
                };
                if let Err(error) = result {
                    return self.collection_fault(s, heap_fault(error));
                }
            }
            TableAction::Build {
                dest,
                descriptor,
                field,
            } => {
                let Some(TableSource::Pair(pair)) = self.table_source(*s, descriptor, field) else {
                    unreachable!()
                };
                let value = match self.read(s.regs, pair[1]) {
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
        s.pc = s.instruction_end;
        s.phase = ScriptPhase::Body;
        Ok(())
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
        let extra = if index < len {
            Values::growth_bytes(index.into(), 1)
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
