//! Collection payloads build privately before publication. Observable field
//! retirement also disconnects the existing property graph during commit.
use super::*;

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub(in crate::executor) struct BulkWork {
    call: BuiltinCall,
    index: u32,
    len: u32,
    original_len: u32,
    offset: u32,
    receiver_version: u64,
    source_version: Option<u64>,
    model_fields: u32,
}

impl Vm {
    pub(super) fn start_bulk(
        &mut self,
        s: &mut ScriptFrame,
        call: BuiltinCall,
    ) -> Result<(), FaultKind> {
        if s.effects.0 & Effects::ALLOCATE == 0 {
            return Err(FaultKind::Effect);
        }
        let receiver = self.value(s.env, 1).map_err(vm_fault)?;
        let info = self.heap.info(receiver).map_err(heap_fault)?;
        let argument = |position| self.builtin_argument(s, call, position).map_err(vm_fault);
        let mut work = BulkWork {
            call,
            index: 0,
            len: info.len,
            original_len: info.len,
            offset: 0,
            receiver_version: info.content_version,
            source_version: None,
            model_fields: 0,
        };
        match call.method {
            Builtin::Insert => {
                work.offset = index(argument(0)?)?;
                if work.offset > info.len {
                    return Err(FaultKind::OutOfBounds);
                }
                work.len = info.len.checked_add(1).ok_or(FaultKind::OutOfBounds)?;
            }
            Builtin::Remove => {
                work.offset = index(argument(0)?)?;
                if work.offset >= info.len {
                    return Err(FaultKind::OutOfBounds);
                }
                work.len -= 1;
            }
            Builtin::Resize => work.len = index(argument(0)?)?,
            Builtin::Clear => work.len = 0,
            Builtin::Extend => {
                let source = argument(0)?;
                self.heap.observe_read(source, None).map_err(heap_fault)?;
                let len = self.heap.sequence_len(source).map_err(heap_fault)?;
                work.len = info.len.checked_add(len).ok_or(FaultKind::OutOfBounds)?;
                work.source_version =
                    Some(self.heap.info(source).map_err(heap_fault)?.content_version);
            }
            Builtin::Slice => {
                let (begin, end) = self.range_bounds(argument(0)?)?;
                if end > info.len {
                    return Err(FaultKind::OutOfBounds);
                }
                work.offset = begin;
                work.len = end - begin;
            }
            Builtin::Reverse | Builtin::Keys | Builtin::Values | Builtin::Entries => {}
            _ => unreachable!("bulk method dispatch"),
        }
        let buffer = if info.kind == ObjectKind::Table && matches!(call.method, Builtin::Clear) {
            self.heap.alloc_table()
        } else {
            self.heap.alloc_array(&[])
        }
        .map_err(heap_fault)?;
        self.put(s.env, 2, buffer).map_err(vm_fault)?;
        s.phase = ScriptPhase::Bulk(work);
        Ok(())
    }

    pub(in crate::executor) fn start_table_remove(
        &mut self,
        s: &mut ScriptFrame,
        call: BuiltinCall,
        offset: u32,
    ) -> Result<(), FaultKind> {
        if s.effects.0 & Effects::ALLOCATE == 0 {
            return Err(FaultKind::Effect);
        }
        let receiver = self.value(s.env, 1).map_err(vm_fault)?;
        let info = self.heap.info(receiver).map_err(heap_fault)?;
        let buffer = self.heap.alloc_table().map_err(heap_fault)?;
        self.put(s.env, 2, buffer).map_err(vm_fault)?;
        let model_fields = self
            .heap
            .prepare_model_table_remove(receiver, buffer)
            .map_err(heap_fault)?;
        s.phase = ScriptPhase::Bulk(BulkWork {
            call,
            index: 0,
            len: (info.len - 1).max(model_fields),
            original_len: info.len,
            offset,
            receiver_version: info.content_version,
            source_version: None,
            model_fields,
        });
        Ok(())
    }

    pub(in crate::executor) fn bulk_step(
        &mut self,
        s: &mut ScriptFrame,
        mut work: BulkWork,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        let receiver = self.value(s.env, 1)?;
        let buffer = self.value(s.env, 2)?;
        let changed = self.heap.info(receiver)?.content_version != work.receiver_version
            || if let Some(version) = work.source_version {
                self.heap
                    .info(self.builtin_argument(s, work.call, 0)?)?
                    .content_version
                    != version
            } else {
                false
            };
        let removing_table = matches!(work.call.method, Builtin::TableRemove);
        let extra = if !changed && work.index < work.len && removing_table {
            let copying = work.index < work.original_len - 1;
            (if copying {
                Values::growth_bytes(u64::from(work.index) * 2, 2)
            } else {
                0
            }) + if work.model_fields != 0 {
                Values::growth_bytes(work.index.into(), 1) * (1 + u64::from(copying))
            } else {
                0
            }
        } else if !changed && work.index < work.len {
            Values::growth_bytes(work.index.into(), 1)
                + if matches!(work.call.method, Builtin::Entries) {
                    Values::allocation_bytes(2)
                } else {
                    0
                }
        } else {
            0
        };
        if !meter.take(0, 6, extra) {
            return Ok(false);
        }
        if changed {
            self.collection_fault(s, FaultKind::ConcurrentModification)?;
        } else if work.index == work.len {
            let result = if removing_table {
                let removed = self.heap.table_entry(receiver, work.offset)?.1;
                self.heap
                    .commit_table_remove(receiver, buffer, work.offset)
                    .map(|()| {
                        self.heap.request_collection();
                        removed
                    })
                    .map_err(heap_fault)
            } else if work.call.method.mutates() {
                let result = if matches!(work.call.method, Builtin::Remove) {
                    self.heap.sequence_get(receiver, work.offset)?
                } else {
                    receiver
                };
                self.heap
                    .commit_collection(receiver, buffer)
                    .map(|()| {
                        if matches!(work.call.method, Builtin::Clear) && work.original_len != 0 {
                            self.heap.request_collection();
                        }
                        result
                    })
                    .map_err(heap_fault)
            } else {
                if self.heap.info(receiver)?.kind == ObjectKind::Tuple {
                    self.heap.seal_tuple(buffer)?;
                }
                Ok(buffer)
            };
            self.builtin_finish(s, work.call, result)?;
        } else {
            let result = if removing_table {
                (|| {
                    if work.index < work.original_len - 1 {
                        let source = work.index + u32::from(work.index >= work.offset);
                        let (key, value) = self.heap.table_entry(receiver, source)?;
                        self.heap.table_store(buffer, work.index, key, value)?;
                    }
                    self.heap
                        .model_table_remove_step(receiver, buffer, work.offset, work.index)
                })()
                .map_err(heap_fault)
            } else {
                self.bulk_value(s, work, receiver)
                    .and_then(|value| self.heap.array_push(buffer, value).map_err(heap_fault))
            };
            match result {
                Ok(()) => {
                    work.index += 1;
                    s.phase = ScriptPhase::Bulk(work);
                }
                Err(kind) => self.collection_fault(s, kind)?,
            }
        }
        Ok(true)
    }

    fn bulk_value(
        &mut self,
        s: &ScriptFrame,
        work: BulkWork,
        receiver: Value,
    ) -> Result<Value, FaultKind> {
        let position = match work.call.method {
            Builtin::Insert if work.index == work.offset => {
                return self.builtin_argument(s, work.call, 1).map_err(vm_fault);
            }
            Builtin::Insert => work.index - u32::from(work.index > work.offset),
            Builtin::Remove => work.index + u32::from(work.index >= work.offset),
            Builtin::Resize if work.index >= work.original_len => {
                return self.builtin_argument(s, work.call, 1).map_err(vm_fault);
            }
            Builtin::Reverse => work.len - work.index - 1,
            Builtin::Extend if work.index >= work.original_len => {
                let source = self.builtin_argument(s, work.call, 0).map_err(vm_fault)?;
                return self
                    .heap
                    .sequence_get(source, work.index - work.original_len)
                    .map_err(heap_fault);
            }
            Builtin::Slice => work.offset + work.index,
            Builtin::Keys | Builtin::Values | Builtin::Entries => {
                let (key, value) = self
                    .heap
                    .table_entry(receiver, work.index)
                    .map_err(heap_fault)?;
                return match work.call.method {
                    Builtin::Keys => Ok(key),
                    Builtin::Values => Ok(value),
                    _ => self.heap.alloc_tuple(&[key, value]).map_err(heap_fault),
                };
            }
            _ => work.index,
        };
        self.heap
            .sequence_get(receiver, position)
            .map_err(heap_fault)
    }
}
