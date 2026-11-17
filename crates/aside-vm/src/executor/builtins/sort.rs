//! Stable bottom-up merges with one rooted, double-width working array.
use super::*;

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
enum SortPhase {
    Copy,
    Merge,
    Compare,
    Callback,
    Write { left: bool },
    Normalize,
    Trim,
}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub(in crate::executor) struct SortWork {
    call: BuiltinCall,
    comparator: Option<FunctionId>,
    version: u64,
    len: u32,
    position: u32,
    width: u32,
    base: u32,
    mid: u32,
    end: u32,
    left: u32,
    right: u32,
    output: u32,
    text_offset: u32,
    phase: SortPhase,
}

impl SortWork {
    fn run(&mut self, start: u32) {
        self.mid = start.saturating_add(self.width).min(self.len);
        self.end = self.mid.saturating_add(self.width).min(self.len);
        self.left = start;
        self.right = self.mid;
        self.output = start;
        self.phase = SortPhase::Merge;
    }

    fn ordered(&mut self, order: Ordering) {
        self.text_offset = 0;
        // Taking the left run on equality is what preserves stability.
        self.phase = SortPhase::Write {
            left: order != Ordering::Greater,
        };
    }
}

impl Vm {
    pub(super) fn start_sort(
        &mut self,
        s: &mut ScriptFrame,
        call: BuiltinCall,
    ) -> Result<(), FaultKind> {
        let compare = self.builtin_argument(s, call, 0).map_err(vm_fault)?;
        let comparator = if compare.same_bits(Value::NULL) {
            None
        } else {
            Some(
                self.closure_function(compare)
                    .map_err(|_| FaultKind::Type)?,
            )
        };
        let receiver = self.value(s.env, 1).map_err(vm_fault)?;
        let info = self.heap.info(receiver).map_err(heap_fault)?;
        if info.len < 2 {
            return self.builtin_finish(s, call, Ok(receiver)).map_err(vm_fault);
        }
        if s.effects.0 & Effects::ALLOCATE == 0 {
            return Err(FaultKind::Effect);
        }
        // Both halves use the heap's ordinary u32 collection indices.
        info.len.checked_mul(2).ok_or(FaultKind::Resource)?;
        let buffer = self.heap.alloc_array(&[]).map_err(heap_fault)?;
        self.put(s.env, 2, buffer).map_err(vm_fault)?;
        s.phase = ScriptPhase::Sort(SortWork {
            call,
            comparator,
            version: info.content_version,
            len: info.len,
            position: 0,
            width: 1,
            base: 0,
            mid: 0,
            end: 0,
            left: 0,
            right: 0,
            output: 0,
            text_offset: 0,
            phase: SortPhase::Copy,
        });
        Ok(())
    }

    pub(super) fn sort_argument(
        &self,
        s: &ScriptFrame,
        work: SortWork,
        index: u32,
    ) -> Result<(Value, Value), VmError> {
        let index = work.base + if index == 0 { work.left } else { work.right };
        let value = self.heap.sequence_get(self.value(s.env, 2)?, index)?;
        Ok((Value::int(-1), value))
    }

    pub(in crate::executor) fn sort_step(
        &mut self,
        s: &mut ScriptFrame,
        mut work: SortWork,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        let receiver = self.value(s.env, 1)?;
        let buffer = self.value(s.env, 2)?;
        if self.heap.info(receiver)?.content_version != work.version {
            if !meter.take(0, 6, 0) {
                return Ok(false);
            }
            self.clear(s.env, 3)?;
            self.collection_fault(s, FaultKind::ConcurrentModification)?;
            return Ok(true);
        }
        if matches!(work.phase, SortPhase::Compare) {
            return self.sort_compare(s, work, meter);
        }
        let extra = if matches!(work.phase, SortPhase::Copy) && work.position < work.len * 2 {
            Values::growth_bytes(work.position.into(), 1)
        } else {
            0
        };
        if !meter.take(0, 8, extra) {
            return Ok(false);
        }
        let result = match work.phase {
            SortPhase::Copy if work.position < work.len * 2 => {
                let value = if work.position < work.len {
                    self.heap.sequence_get(receiver, work.position)?
                } else {
                    Value::NULL
                };
                work.position += 1;
                self.heap.array_push(buffer, value).map_err(heap_fault)
            }
            SortPhase::Copy => {
                work.run(0);
                Ok(())
            }
            SortPhase::Merge => {
                if work.output == work.end {
                    if work.end < work.len {
                        work.run(work.end);
                    } else {
                        work.base = work.len - work.base;
                        work.width = work.width.saturating_mul(2).min(work.len);
                        if work.width == work.len {
                            work.position = 0;
                            work.phase = if work.base == 0 {
                                SortPhase::Trim
                            } else {
                                SortPhase::Normalize
                            };
                        } else {
                            work.run(0);
                        }
                    }
                } else if work.left == work.mid {
                    work.phase = SortPhase::Write { left: false };
                } else if work.right == work.end {
                    work.phase = SortPhase::Write { left: true };
                } else {
                    work.phase = SortPhase::Compare;
                }
                Ok(())
            }
            SortPhase::Callback => {
                let order = self.value(s.env, 3)?.as_int();
                self.clear(s.env, 3)?;
                order.map_or(Err(FaultKind::Type), |order| {
                    work.ordered(order.cmp(&0));
                    Ok(())
                })
            }
            SortPhase::Write { left } => {
                let index = if left { work.left } else { work.right };
                let value = self.heap.sequence_get(buffer, work.base + index)?;
                let target = work.len - work.base + work.output;
                work.output += 1;
                if left {
                    work.left += 1;
                } else {
                    work.right += 1;
                }
                work.phase = SortPhase::Merge;
                self.heap
                    .set_field(buffer, target, value)
                    .map_err(heap_fault)
            }
            SortPhase::Normalize if work.position < work.len => {
                let value = self.heap.sequence_get(buffer, work.len + work.position)?;
                let target = work.position;
                work.position += 1;
                self.heap
                    .set_field(buffer, target, value)
                    .map_err(heap_fault)
            }
            SortPhase::Normalize => {
                work.phase = SortPhase::Trim;
                Ok(())
            }
            SortPhase::Trim if self.heap.info(buffer)?.len > work.len => {
                self.heap.array_pop(buffer).map(|_| ()).map_err(heap_fault)
            }
            SortPhase::Trim => {
                let result = self
                    .heap
                    .commit_collection(receiver, buffer)
                    .map_err(heap_fault);
                self.builtin_finish(s, work.call, result.map(|()| receiver))?;
                return Ok(true);
            }
            SortPhase::Compare => unreachable!(),
        };
        match result {
            Ok(()) => s.phase = ScriptPhase::Sort(work),
            Err(kind) => self.collection_fault(s, kind)?,
        }
        Ok(true)
    }

    fn sort_compare(
        &mut self,
        s: &mut ScriptFrame,
        mut work: SortWork,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        if let Some(function) = work.comparator {
            let callback = self.builtin_argument(s, work.call, 0)?;
            work.phase = SortPhase::Callback;
            return self.push_builtin_callback(
                s,
                function,
                callback,
                2,
                ScriptPhase::Sort(work),
                meter,
            );
        }
        let buffer = self.value(s.env, 2)?;
        let left = self.heap.sequence_get(buffer, work.base + work.left)?;
        let right = self.heap.sequence_get(buffer, work.base + work.right)?;
        let strings =
            self.is_text(left, ObjectKind::String) && self.is_text(right, ObjectKind::String);
        let bytes = if strings {
            (self.heap.info(left)?.len.min(self.heap.info(right)?.len) - work.text_offset).min(64)
        } else {
            0
        };
        if !meter.take(0, 8 + u64::from(bytes) * 2, 0) {
            return Ok(false);
        }
        let order = if strings {
            let mut a = [0; 64];
            let mut b = [0; 64];
            self.heap
                .read_bytes(left, work.text_offset, &mut a[..bytes as usize])?;
            self.heap
                .read_bytes(right, work.text_offset, &mut b[..bytes as usize])?;
            let order = a[..bytes as usize].cmp(&b[..bytes as usize]);
            work.text_offset += bytes;
            if order != Ordering::Equal {
                Some(order)
            } else if work.text_offset == self.heap.info(left)?.len.min(self.heap.info(right)?.len)
            {
                Some(self.heap.info(left)?.len.cmp(&self.heap.info(right)?.len))
            } else {
                s.phase = ScriptPhase::Sort(work);
                return Ok(true);
            }
        } else {
            left.numeric_cmp(right)
        };
        if let Some(order) = order {
            work.ordered(order);
            s.phase = ScriptPhase::Sort(work);
        } else {
            self.collection_fault(s, FaultKind::Type)?;
        }
        Ok(true)
    }
}
