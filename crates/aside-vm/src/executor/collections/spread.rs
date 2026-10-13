//! Shallow captures resume without allocating per-element state. The source
//! and destination versions cover mutations by other tasks between slices.
use super::*;

#[derive(Clone, Copy)]
pub(in crate::executor) struct SequenceSpread {
    pub arguments: bool,
    pub index: u32,
    pub len: u32,
    pub version: u64,
    pub destination_version: u64,
}

#[derive(Clone, Copy)]
pub(in crate::executor) enum TableFinish {
    Instruction,
    Build {
        dest: Reg,
        descriptor: DescriptorId,
        field: u32,
    },
}

#[derive(Clone, Copy)]
pub(in crate::executor) struct TableSpread {
    pub source: Reg,
    pub index: u32,
    pub len: u32,
    pub version: u64,
    pub destination_version: u64,
    pub prefix_len: u32,
    finish: TableFinish,
}

impl Vm {
    pub(in crate::executor) fn spread_step(
        &mut self,
        s: &mut ScriptFrame,
        mut work: SequenceSpread,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        let target = self.value(s.env, 1)?;
        let source = self.value(s.env, 2)?;
        let destination = self.heap.info(target)?;
        let changed = destination.content_version != work.destination_version
            || self.heap.info(source)?.content_version != work.version;
        let extra = if !changed
            && work.index < work.len
            && (destination.len as usize).is_multiple_of(VALUE_CHUNK_LEN)
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
        } else if work.index == work.len {
            self.clear(s.env, 1)?;
            self.clear(s.env, 2)?;
            s.pc.0 += 1;
            s.phase = ScriptPhase::Body;
        } else {
            let value = self.heap.sequence_get(source, work.index)?;
            let result = if work.arguments {
                self.heap.packed_pair_push(target, Value::int(-1), value)
            } else {
                self.heap.array_push(target, value)
            };
            match result {
                Ok(()) => {
                    work.index += 1;
                    work.destination_version = self.heap.info(target)?.content_version;
                    // Self-spread copies the original length exactly once.
                    if source.same_bits(target) {
                        work.version = work.destination_version;
                    }
                    s.phase = ScriptPhase::Spread(work);
                }
                Err(error) => self.collection_fault(s, heap_fault(error))?,
            }
        }
        Ok(true)
    }

    pub(in crate::executor) fn start_table_spread(
        &mut self,
        s: &mut ScriptFrame,
        table: Value,
        source: Reg,
        finish: TableFinish,
    ) -> Result<(), FaultKind> {
        let value = self.read(*s, source)?;
        if !self.is_text(table, ObjectKind::Table) || !self.is_text(value, ObjectKind::Table) {
            return Err(FaultKind::Type);
        }
        let info = self.heap.info(value).map_err(heap_fault)?;
        let destination = self.heap.info(table).map_err(heap_fault)?;
        self.put(s.env, 1, table).map_err(vm_fault)?;
        s.phase = ScriptPhase::TableSpread(TableSpread {
            source,
            index: 0,
            len: if value.same_bits(table) { 0 } else { info.len },
            version: info.content_version,
            destination_version: destination.content_version,
            prefix_len: destination.len,
            finish,
        });
        Ok(())
    }

    pub(in crate::executor) fn table_spread_step(
        &mut self,
        s: &mut ScriptFrame,
        mut work: TableSpread,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        let target = self.value(s.env, 1)?;
        let source = self.value(s.regs, work.source.0)?;
        let destination = self.heap.info(target)?;
        let changed = destination.content_version != work.destination_version
            || self.heap.info(source)?.content_version != work.version;
        let extra = if !changed
            && work.prefix_len == 0
            && work.index < work.len
            && (destination.len as usize * 2).is_multiple_of(VALUE_CHUNK_LEN)
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
        } else if work.index == work.len {
            self.clear(s.env, 2)?;
            match work.finish {
                TableFinish::Instruction => {
                    self.clear(s.env, 1)?;
                    s.pc.0 += 1;
                    s.phase = ScriptPhase::Body;
                }
                TableFinish::Build {
                    dest,
                    descriptor,
                    field,
                } => {
                    self.next_table_field(s, dest, descriptor, field)?;
                }
            }
        } else {
            let (key, value) = self.heap.table_entry(source, work.index)?;
            if work.prefix_len == 0 {
                // A table already contains unique, valid keys. Copy into an
                // empty destination without scanning its growing prefix.
                match self.heap.table_store(target, work.index, key, value) {
                    Ok(()) => {
                        work.index += 1;
                        work.destination_version = self.heap.info(target)?.content_version;
                        s.phase = ScriptPhase::TableSpread(work);
                    }
                    Err(error) => self.collection_fault(s, heap_fault(error))?,
                }
            } else {
                self.put(s.env, 2, key)?;
                s.table_work = Some(TableWork {
                    action: TableAction::Spread(work),
                    entry: 0,
                    version: destination.content_version,
                    matched: false,
                });
                s.phase = ScriptPhase::TableScan;
            }
        }
        Ok(true)
    }
}
