//! Ordered callback scans borrow the source, rooting the active item and private output.
use super::*;

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub(in crate::executor) struct PredicateWork {
    call: BuiltinCall,
    function: FunctionId,
    len: u32,
    version: u64,
    index: u32,
    waiting: bool,
}

impl PredicateWork {
    pub(super) fn argument_slot(self) -> u32 {
        // Collectors keep the output in slot 2. The return replaces the item
        // in slot 3, so filter reads the original from the unchanged source.
        if matches!(self.call.method, Builtin::Map | Builtin::Filter) {
            3
        } else {
            2
        }
    }
}

impl Vm {
    pub(super) fn start_predicate(
        &mut self,
        s: &mut ScriptFrame,
        call: BuiltinCall,
    ) -> Result<(), FaultKind> {
        let callback = self.builtin_argument(s, call, 0).map_err(vm_fault)?;
        let function = self
            .closure_function(callback)
            .map_err(|_| FaultKind::Type)?;
        let receiver = self.value(s.env, 1).map_err(vm_fault)?;
        let info = self.heap.info(receiver).map_err(heap_fault)?;
        if matches!(call.method, Builtin::Map | Builtin::Filter) {
            if s.effects.0 & Effects::ALLOCATE == 0 {
                return Err(FaultKind::Effect);
            }
            let output = if call.method == Builtin::Filter {
                self.heap.alloc_array_like(receiver)
            } else {
                self.heap.alloc_array(&[])
            }
            .map_err(heap_fault)?;
            self.put(s.env, 2, output).map_err(vm_fault)?;
        }
        s.phase = ScriptPhase::Predicate(PredicateWork {
            call,
            function,
            len: info.len,
            version: info.content_version,
            index: 0,
            waiting: false,
        });
        Ok(())
    }

    pub(in crate::executor) fn predicate_step(
        &mut self,
        s: &mut ScriptFrame,
        mut work: PredicateWork,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        let receiver = self.value(s.env, 1)?;
        let changed = self.heap.info(receiver)?.content_version != work.version;
        if !changed && !work.waiting && work.index < work.len {
            let callback = self.builtin_argument(s, work.call, 0)?;
            self.put(
                s.env,
                work.argument_slot(),
                self.heap.sequence_get(receiver, work.index)?,
            )?;
            work.waiting = true;
            return self.push_builtin_callback(
                s,
                work.function,
                callback,
                1,
                ScriptPhase::Predicate(work),
                meter,
            );
        }
        let method = work.call.method;
        let returned = if !changed && work.waiting {
            self.value(s.env, 3)?
        } else {
            Value::NULL
        };
        let accepted = returned.as_bool();
        let append = !changed
            && work.waiting
            && (method == Builtin::Map || (method == Builtin::Filter && accepted == Some(true)));
        let (work_units, extra) = if append {
            let output = self.value(s.env, 2)?;
            (6 + self.heap.array_item_work(output)?, Values::growth_bytes(work.call.index.into(), 1))
        } else {
            (3, 0)
        };
        if !meter.take(0, work_units, extra) {
            return Ok(false);
        }
        self.clear(s.env, 3)?;
        let result = if changed {
            Some(Err(FaultKind::ConcurrentModification))
        } else if !work.waiting {
            Some(Ok(match method {
                Builtin::Any => Value::bool(false),
                Builtin::All => Value::bool(true),
                Builtin::Count => Value::int(work.call.index.into()),
                Builtin::Map | Builtin::Filter => self.value(s.env, 2)?,
                _ => Value::NULL,
            }))
        } else if append {
            let value = if method == Builtin::Map {
                returned
            } else {
                self.heap.sequence_get(receiver, work.index)?
            };
            let output = self.value(s.env, 2)?;
            match self.heap.array_push(output, value) {
                Ok(()) => {
                    work.call.index += 1;
                    None
                }
                Err(error) => Some(Err(heap_fault(error))),
            }
        } else {
            match (method, accepted) {
                (_, None) => Some(Err(FaultKind::Type)),
                (Builtin::Any, Some(true)) => Some(Ok(Value::bool(true))),
                (Builtin::All, Some(false)) => Some(Ok(Value::bool(false))),
                (Builtin::FirstWhere, Some(true)) => Some(Ok(self.value(s.env, 2)?)),
                (Builtin::IndexWhere, Some(true)) => Some(Ok(Value::int(work.index.into()))),
                (Builtin::Count, Some(true)) => {
                    work.call.index += 1;
                    None
                }
                _ => None,
            }
        };
        if let Some(result) = result {
            self.builtin_finish(s, work.call, result)?;
        } else {
            if work.argument_slot() == 2 {
                self.clear(s.env, 2)?;
            }
            work.index += 1;
            work.waiting = false;
            s.phase = ScriptPhase::Predicate(work);
        }
        Ok(true)
    }
}
