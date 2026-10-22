//! Sequence searches borrow their source and share the ordinary equality walker.
use super::*;

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
enum SearchPhase {
    Next,
    Compare,
    Cleanup(Result<bool, FaultKind>),
}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub(in crate::executor) struct SearchWork {
    call: BuiltinCall,
    index: u32,
    len: u32,
    version: u64,
    phase: SearchPhase,
}

impl Vm {
    pub(super) fn start_search(
        &mut self,
        s: &mut ScriptFrame,
        call: BuiltinCall,
    ) -> Result<(), VmError> {
        let receiver = self.value(s.env, 1)?;
        let info = self.heap.info(receiver)?;
        s.phase = ScriptPhase::Search(SearchWork {
            call,
            index: 0,
            len: info.len,
            version: info.content_version,
            phase: SearchPhase::Next,
        });
        Ok(())
    }

    pub(in crate::executor) fn search_step(
        &mut self,
        s: &mut ScriptFrame,
        mut work: SearchWork,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        if !meter.take(0, 4, 0) {
            return Ok(false);
        }
        let receiver = self.value(s.env, 1)?;
        if self.heap.info(receiver)?.content_version != work.version {
            work.phase = SearchPhase::Cleanup(Err(FaultKind::ConcurrentModification));
        }
        match work.phase {
            SearchPhase::Next if work.index == work.len => {
                let result = if matches!(work.call.method, Builtin::Contains) {
                    Value::bool(false)
                } else {
                    Value::NULL
                };
                self.builtin_finish(s, work.call, Ok(result))?;
                return Ok(true);
            }
            SearchPhase::Next => {
                let candidate = self.heap.sequence_get(receiver, work.index)?;
                let value = self.builtin_argument(s, work.call, 0)?;
                let roots = self.active.equality_roots.unwrap();
                self.put(roots, 0, candidate)?;
                self.put(roots, 1, value)?;
                self.active.equality_cursors.data.push(0);
                work.phase = SearchPhase::Compare;
            }
            SearchPhase::Compare => {
                if let Some(result) = self.equality_compare_step()? {
                    work.phase = SearchPhase::Cleanup(result);
                }
            }
            SearchPhase::Cleanup(result) => {
                if !self.active.equality_cursors.data.is_empty() {
                    let depth = self.active.equality_cursors.data.len() as u32 - 1;
                    let roots = self.active.equality_roots.unwrap();
                    self.clear(roots, depth * 2)?;
                    self.clear(roots, depth * 2 + 1)?;
                    self.active.equality_cursors.data.pop();
                } else {
                    match result {
                        Ok(false) => {
                            work.index += 1;
                            work.phase = SearchPhase::Next;
                        }
                        result => {
                            let result = result.map(|_| {
                                if matches!(work.call.method, Builtin::Contains) {
                                    Value::bool(true)
                                } else {
                                    Value::int(i64::from(work.index))
                                }
                            });
                            self.builtin_finish(s, work.call, result)?;
                            return Ok(true);
                        }
                    }
                }
            }
        }
        s.phase = ScriptPhase::Search(work);
        Ok(true)
    }
}
