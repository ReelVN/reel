//! Bound methods share argument validation and resumable execution.
use super::*;
mod bulk;
pub(super) use bulk::BulkWork;
mod sort;
pub(super) use sort::SortWork;
mod strings;
pub(super) use strings::TextWork;

#[derive(Clone, Copy, Debug)]
#[repr(i64)]
#[derive(serde::Serialize, serde::Deserialize)]
pub(super) enum Builtin {
    Len,
    Append,
    Pop,
    HasOwn,
    Slice,
    Insert,
    Remove,
    Resize,
    Reverse,
    Extend,
    Clear,
    Top,
    Swap,
    Keys,
    Values,
    Entries,
    ByteLen,
    ScalarCount,
    SliceBytes,
    SliceScalars,
    StartsWith,
    EndsWith,
    Join,
    AsciiLowercase,
    AsciiUppercase,
    Trim,
    TrimStart,
    TrimEnd,
    Get,
    Split,
    Sort,
}
impl Builtin {
    pub(super) fn lookup(kind: ObjectKind, name: &str) -> Option<Self> {
        use ObjectKind::*;
        match (kind, name) {
            (Array | Tuple | Table, "len") => Some(Self::Len),
            (Array, "append") => Some(Self::Append),
            (Array, "pop") => Some(Self::Pop),
            (Table, "has_own") => Some(Self::HasOwn),
            (Table, "get") => Some(Self::Get),
            (Array | Tuple, "slice") => Some(Self::Slice),
            (Array, "insert") => Some(Self::Insert),
            (Array, "remove") => Some(Self::Remove),
            (Array, "resize") => Some(Self::Resize),
            (Array, "sort") => Some(Self::Sort),
            (Array, "reverse") => Some(Self::Reverse),
            (Array, "extend") => Some(Self::Extend),
            (Array | Table, "clear") => Some(Self::Clear),
            (Array, "top") => Some(Self::Top),
            (Array, "swap") => Some(Self::Swap),
            (Table, "keys") => Some(Self::Keys),
            (Table, "values") => Some(Self::Values),
            (Table, "entries") => Some(Self::Entries),
            (String, "byte_len") => Some(Self::ByteLen),
            (String, "scalar_count") => Some(Self::ScalarCount),
            (String, "slice_bytes") => Some(Self::SliceBytes),
            (String, "slice_scalars") => Some(Self::SliceScalars),
            (String, "starts_with") => Some(Self::StartsWith),
            (String, "ends_with") => Some(Self::EndsWith),
            (String, "join") => Some(Self::Join),
            (String, "split") => Some(Self::Split),
            (String, "to_ascii_lowercase") => Some(Self::AsciiLowercase),
            (String, "to_ascii_uppercase") => Some(Self::AsciiUppercase),
            (String, "trim") => Some(Self::Trim),
            (String, "trim_start") => Some(Self::TrimStart),
            (String, "trim_end") => Some(Self::TrimEnd),
            _ => None,
        }
    }
    fn decode(id: i64) -> Result<Self, FaultKind> {
        const METHODS: &[Builtin] = &[
            Builtin::Len,
            Builtin::Append,
            Builtin::Pop,
            Builtin::HasOwn,
            Builtin::Slice,
            Builtin::Insert,
            Builtin::Remove,
            Builtin::Resize,
            Builtin::Reverse,
            Builtin::Extend,
            Builtin::Clear,
            Builtin::Top,
            Builtin::Swap,
            Builtin::Keys,
            Builtin::Values,
            Builtin::Entries,
            Builtin::ByteLen,
            Builtin::ScalarCount,
            Builtin::SliceBytes,
            Builtin::SliceScalars,
            Builtin::StartsWith,
            Builtin::EndsWith,
            Builtin::Join,
            Builtin::AsciiLowercase,
            Builtin::AsciiUppercase,
            Builtin::Trim,
            Builtin::TrimStart,
            Builtin::TrimEnd,
            Builtin::Get,
            Builtin::Split,
            Builtin::Sort,
        ];
        usize::try_from(id)
            .ok()
            .and_then(|i| METHODS.get(i))
            .copied()
            .ok_or(FaultKind::Type)
    }
    pub(super) fn parameters(self) -> (&'static [&'static str], usize) {
        match self {
            Self::HasOwn => (&["key"], 1),
            Self::Get => (&["key", "default"], 1),
            Self::Slice | Self::SliceBytes | Self::SliceScalars => (&["range"], 1),
            Self::Insert => (&["index", "value"], 2),
            Self::Remove => (&["index"], 1),
            Self::Resize => (&["length", "value"], 1),
            Self::Extend | Self::Join => (&["items"], 1),
            Self::Sort => (&["compare"], 0),
            Self::Split => (&["separator"], 1),
            Self::Swap => (&["a", "b"], 2),
            Self::StartsWith => (&["prefix"], 1),
            Self::EndsWith => (&["suffix"], 1),
            _ => (&[], 0),
        }
    }
    fn mutates(self) -> bool {
        matches!(
            self,
            Self::Append
                | Self::Pop
                | Self::Insert
                | Self::Remove
                | Self::Resize
                | Self::Reverse
                | Self::Extend
                | Self::Clear
                | Self::Swap
                | Self::Sort
        )
    }
}

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
        if !method.mutates() {
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
        match result.and_then(|value| self.write(*s, call.dest, value)) {
            Ok(()) => {
                self.clear(s.env, 1)?;
                self.clear(s.env, 2)?;
                s.pc.0 += 1;
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
        let source = ArgSource::Caller {
            frame: s.id,
            descriptor: call.descriptor,
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
                        if matches!(call.method, Builtin::Append) && name.as_int() == Some(-1) {
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
        let extra = if matches!(call.method, Builtin::Split)
            || (matches!(call.method, Builtin::Append)
                && call.index < call.count
                && (self.heap.info(receiver)?.len as usize).is_multiple_of(VALUE_CHUNK_LEN))
        {
            VALUE_CHUNK_BYTES
        } else {
            0
        };
        if !meter.take(0, 3, extra) {
            return Ok(false);
        }
        let result = match call.method {
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
            Builtin::HasOwn | Builtin::Get => {
                let key = self.builtin_argument(s, call, 0)?;
                let action = if matches!(call.method, Builtin::Get) {
                    TableAction::GetDefault {
                        dest: call.dest,
                        descriptor: call.descriptor,
                        fallback: call.arguments[1],
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
            | Builtin::Join
            | Builtin::Split
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
