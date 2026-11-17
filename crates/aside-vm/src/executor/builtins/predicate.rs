//! Ordered predicate scans borrow the source and retain the active item across slices.
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
            self.put(s.env, 2, self.heap.sequence_get(receiver, work.index)?)?;
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
        if !meter.take(0, 3, 0) {
            return Ok(false);
        }
        if changed {
            self.collection_fault(s, FaultKind::ConcurrentModification)?;
        } else if work.waiting {
            let result = self.value(s.env, 3)?.as_bool();
            self.clear(s.env, 3)?;
            match result {
                Some(true) => {
                    let result = if work.call.method == Builtin::Any {
                        Value::bool(true)
                    } else {
                        self.value(s.env, 2)?
                    };
                    self.builtin_finish(s, work.call, Ok(result))?;
                }
                Some(false) => {
                    self.clear(s.env, 2)?;
                    work.index += 1;
                    work.waiting = false;
                    s.phase = ScriptPhase::Predicate(work);
                }
                None => self.collection_fault(s, FaultKind::Type)?,
            }
        } else {
            let result = if work.call.method == Builtin::Any {
                Value::bool(false)
            } else {
                Value::NULL
            };
            self.builtin_finish(s, work.call, Ok(result))?;
        }
        Ok(true)
    }
}
