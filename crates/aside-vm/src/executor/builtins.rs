//! Bound standard methods use the caller's rooted arguments and metered continuations.
use super::*;

#[derive(Clone, Copy, Debug)]
#[repr(i64)]
pub(super) enum Builtin {
    Len,
    Append,
    Pop,
    HasOwn,
}
impl Builtin {
    pub(super) fn lookup(kind: ObjectKind, name: &str) -> Option<Self> {
        match (kind, name) {
            (ObjectKind::Array | ObjectKind::Tuple | ObjectKind::Table, "len") => Some(Self::Len),
            (ObjectKind::Array, "append") => Some(Self::Append),
            (ObjectKind::Array, "pop") => Some(Self::Pop),
            (ObjectKind::Table, "has_own") => Some(Self::HasOwn),
            _ => None,
        }
    }
    fn decode(id: i64) -> Result<Self, FaultKind> {
        match id {
            0 => Ok(Self::Len),
            1 => Ok(Self::Append),
            2 => Ok(Self::Pop),
            3 => Ok(Self::HasOwn),
            _ => Err(FaultKind::Type),
        }
    }
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
        let expected = match method {
            Builtin::Len | Builtin::Pop => Some(0),
            Builtin::HasOwn => Some(1),
            Builtin::Append => None,
        };
        if let Some(expected) = expected {
            if count < expected {
                return Err(FaultKind::MissingArgument);
            }
            if count > expected {
                return Err(FaultKind::UnknownArgument);
            }
        }
        if matches!(method, Builtin::Append | Builtin::Pop)
            && (s.context.derived || s.effects.0 & Effects::WRITE == 0)
        {
            return Err(FaultKind::Effect);
        }
        self.put(s.env, 1, receiver).map_err(vm_fault)?;
        s.phase = ScriptPhase::Builtin {
            descriptor,
            dest,
            method,
            count,
            index: 0,
            prepared: false,
        };
        Ok(())
    }

    pub(super) fn builtin_step(
        &mut self,
        s: &mut ScriptFrame,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        let ScriptPhase::Builtin {
            descriptor,
            dest,
            method,
            count,
            index,
            prepared,
        } = s.phase
        else {
            unreachable!()
        };
        let receiver = self.value(s.env, 1)?;
        let source = ArgSource::Caller {
            frame: s.id,
            descriptor,
        };
        // Check all names before starting effects. Argument values stay in the caller.
        if !prepared && index < count {
            if !meter.take(0, 1, 0) {
                return Ok(false);
            }
            let (name, _) = self.argument(source, index)?;
            if name != -1
                && !(matches!(method, Builtin::HasOwn)
                    && self.program.package().symbols[name as usize] == "key")
            {
                self.collection_fault(s, FaultKind::UnknownArgument)?;
            } else {
                s.phase = ScriptPhase::Builtin {
                    descriptor,
                    dest,
                    method,
                    count,
                    index: index + 1,
                    prepared: false,
                };
            }
            return Ok(true);
        }
        let index = if prepared { index } else { 0 };
        let extra = if matches!(method, Builtin::Append)
            && index < count
            && (self.heap.info(receiver)?.len as usize).is_multiple_of(VALUE_CHUNK_LEN)
        {
            VALUE_CHUNK_BYTES
        } else {
            0
        };
        if !meter.take(0, 1, extra) {
            return Ok(false);
        }
        let result = match method {
            Builtin::Len => Ok(Value::int(i64::from(
                if self.heap.info(receiver)?.kind == ObjectKind::Table {
                    self.heap.table_len(receiver)?
                } else {
                    self.heap.sequence_len(receiver)?
                },
            ))),
            Builtin::Pop => self
                .heap
                .array_pop(receiver)
                .map_err(heap_fault)
                .and_then(|v| v.ok_or(FaultKind::OutOfBounds)),
            Builtin::HasOwn => {
                let (_, key) = self.argument(source, 0)?;
                if let Err(kind) = self.start_table(s, receiver, key, TableAction::HasOwn(dest)) {
                    self.collection_fault(s, kind)?;
                }
                return Ok(true);
            }
            Builtin::Append if index < count => {
                if s.effects.0 & Effects::ALLOCATE == 0 {
                    self.collection_fault(s, FaultKind::Effect)?;
                    return Ok(true);
                }
                let (_, value) = self.argument(source, index)?;
                match self.heap.array_push(receiver, value) {
                    Ok(()) => {
                        s.phase = ScriptPhase::Builtin {
                            descriptor,
                            dest,
                            method,
                            count,
                            index: index + 1,
                            prepared: true,
                        }
                    }
                    Err(error) => self.collection_fault(s, heap_fault(error))?,
                }
                return Ok(true);
            }
            Builtin::Append => Ok(receiver),
        };
        match result {
            Ok(value) => {
                self.put(s.regs, dest.0, value)?;
                self.clear(s.env, 1)?;
                s.pc.0 += 1;
                s.phase = ScriptPhase::Body;
            }
            Err(kind) => self.collection_fault(s, kind)?,
        }
        Ok(true)
    }
}
