//! Bound collection methods share argument validation and resumable execution.
use super::*;
mod bulk;
pub(super) use bulk::BulkWork;

#[derive(Clone, Copy, Debug)]
#[repr(i64)]
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
}
impl Builtin {
    pub(super) fn lookup(kind: ObjectKind, name: &str) -> Option<Self> {
        use ObjectKind::*;
        match (kind, name) {
            (Array | Tuple | Table, "len") => Some(Self::Len),
            (Array, "append") => Some(Self::Append),
            (Array, "pop") => Some(Self::Pop),
            (Table, "has_own") => Some(Self::HasOwn),
            (Array | Tuple, "slice") => Some(Self::Slice),
            (Array, "insert") => Some(Self::Insert),
            (Array, "remove") => Some(Self::Remove),
            (Array, "resize") => Some(Self::Resize),
            (Array, "reverse") => Some(Self::Reverse),
            (Array, "extend") => Some(Self::Extend),
            (Array | Table, "clear") => Some(Self::Clear),
            (Array, "top") => Some(Self::Top),
            (Array, "swap") => Some(Self::Swap),
            (Table, "keys") => Some(Self::Keys),
            (Table, "values") => Some(Self::Values),
            (Table, "entries") => Some(Self::Entries),
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
        ];
        usize::try_from(id)
            .ok()
            .and_then(|i| METHODS.get(i))
            .copied()
            .ok_or(FaultKind::Type)
    }
    fn parameters(self) -> (&'static [&'static str], usize) {
        match self {
            Self::HasOwn => (&["key"], 1),
            Self::Slice => (&["range"], 1),
            Self::Insert => (&["index", "value"], 2),
            Self::Remove => (&["index"], 1),
            Self::Resize => (&["length", "value"], 1),
            Self::Extend => (&["items"], 1),
            Self::Swap => (&["a", "b"], 2),
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
        )
    }
}

#[derive(Clone, Copy)]
pub(super) struct BuiltinCall {
    descriptor: DescriptorId,
    dest: Reg,
    method: Builtin,
    count: u32,
    index: u32,
    positional: usize,
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
        if method.mutates() && (s.context.derived || s.effects.0 & Effects::WRITE == 0) {
            return Err(FaultKind::Effect);
        }
        self.put(s.env, 1, receiver).map_err(vm_fault)?;
        s.phase = ScriptPhase::Builtin(BuiltinCall {
            descriptor,
            dest,
            method,
            count,
            index: 0,
            positional: 0,
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
            if !meter.take(0, 3, 0) {
                return Ok(false);
            }
            let (parameters, required) = call.method.parameters();
            if call.index < call.count {
                let (name, _) = self.argument(source, call.index)?;
                let binding = if matches!(call.method, Builtin::Append) {
                    if name == -1 {
                        Ok(())
                    } else {
                        Err(FaultKind::UnknownArgument)
                    }
                } else {
                    let position = if name == -1 {
                        let position = call.positional;
                        call.positional += 1;
                        Some(position)
                    } else {
                        self.program
                            .package()
                            .symbols
                            .get(name as usize)
                            .and_then(|name| parameters.iter().position(|p| *p == name))
                    };
                    match position.filter(|p| *p < parameters.len()) {
                        None => Err(FaultKind::UnknownArgument),
                        Some(p) if call.arguments[p].is_some() => Err(FaultKind::DuplicateArgument),
                        Some(p) => {
                            call.arguments[p] = Some(call.index);
                            Ok(())
                        }
                    }
                };
                if let Err(kind) = binding {
                    self.collection_fault(s, kind)?;
                } else {
                    call.index += 1;
                    s.phase = ScriptPhase::Builtin(call);
                }
                return Ok(true);
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
        let extra = if matches!(call.method, Builtin::Append)
            && call.index < call.count
            && (self.heap.info(receiver)?.len as usize).is_multiple_of(VALUE_CHUNK_LEN)
        {
            VALUE_CHUNK_BYTES
        } else {
            0
        };
        if !meter.take(0, 3, extra) {
            return Ok(false);
        }
        let result = match call.method {
            Builtin::Len => Ok(Value::int(i64::from(self.heap.info(receiver)?.len))),
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
            Builtin::HasOwn => {
                let key = self.builtin_argument(s, call, 0)?;
                if let Err(kind) =
                    self.start_table(s, receiver, key, TableAction::HasOwn(call.dest))
                {
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
}

fn index(value: Value) -> Result<u32, FaultKind> {
    let value = value.as_int().ok_or(FaultKind::Type)?;
    u32::try_from(value).map_err(|_| FaultKind::OutOfBounds)
}
