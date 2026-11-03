//! A trusted literal position completes its original three short table phases.
use super::*;

impl Vm {
    pub(in crate::executor) fn direct_named_read(
        &mut self,
        frame: &mut ScriptFrame,
        plan: &NamedRead,
        dest: Reg,
        object: Reg,
        symbol: SymbolId,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        #[cfg(any(test, feature = "vm-validation-oracle"))]
        if self.legacy_named_read_completion {
            return Ok(false);
        }
        if plan.len == 0
            || !meter.admits_short_steps(3)
            || meter.budget.work - meter.usage.work < 4
            || !self.active.equality_cursors.data.is_empty()
            || self.program.package().symbols[symbol.index()].len() > member_hints::MAX_KEY_BYTES
        {
            return Ok(false);
        }
        let Some((entry, true)) =
            self.member_hints
                .get(frame.code.function, frame.pc, plan.table, plan.version)
        else {
            return Ok(false);
        };
        // The plan and full-generation hint were both checked in this same
        // synchronous instruction. No callback, collection or mutation of the
        // table's structure can intervene before its current value is read.
        if entry >= plan.len {
            return Ok(false);
        }
        let host = if plan.host {
            let Ok(host) = self.value(frame.regs, object.0) else {
                return Ok(false);
            };
            Some(host)
        } else {
            None
        };
        let property = if let Some(host) = host {
            Some(crate::PropertyId { host, field: entry })
        } else if self.heap.is_model_table(plan.table).unwrap_or(false) {
            let Ok(field) = self.heap.model_field(plan.table, entry) else {
                return Ok(false);
            };
            Some(crate::PropertyId {
                host: plan.table,
                field,
            })
        } else {
            None
        };
        if let Some(property) = property {
            // Freshness walks and their possible script callbacks retain the
            // original resumable cursor. This prefix only classifies the source.
            if !matches!(self.heap.reactive_derived_source(property), Ok(None)) {
                return Ok(false);
            }
        }
        // Original TableValidate, trusted TableScan and matched commit grants.
        // Keep their separate clock steps, including the instruction's prior
        // clock boundary, and leave insufficient grants to the ordinary cursor.
        for work in [1, 2, 1] {
            assert!(meter.next_short_step());
            assert!(meter.take(0, work, 0));
        }
        #[cfg(feature = "execution-counts")]
        {
            self.execution_counts.step(&ScriptPhase::TableValidate);
            self.execution_counts.step(&ScriptPhase::TableScan);
            self.execution_counts.step(&ScriptPhase::TableScan);
        }
        execution_count!(self, NamedTableStarts, 1);
        execution_count!(self, NamedTableSteps, 3);
        execution_count!(self, NamedTableHintTrusted, 1);
        #[cfg(test)]
        {
            self.work_probe
                .direct_named_reads
                .set(self.work_probe.direct_named_reads.get() + 1);
            self.work_probe
                .member_hint_hits
                .set(self.work_probe.member_hint_hits.get() + 1);
        }
        self.member_hints.remember(
            frame.code.function,
            frame.pc,
            entry,
            plan.table,
            plan.version,
        );
        let result = (|| -> Result<(), VmError> {
            // Key validation normally leaves both work slots uninitialized.
            let roots = self.active.equality_roots.unwrap();
            self.clear(roots, 0)?;
            self.clear(roots, 1)?;
            let Some(value) = self.read_matched_member(frame, plan.table, entry, host)? else {
                return Ok(());
            };
            self.put(frame.regs, dest.0, value)?;
            self.clear(frame.env, 1)?;
            self.clear(frame.env, 2)?;
            frame.pc = frame.instruction_end;
            frame.phase = ScriptPhase::Body;
            Ok(())
        })();
        if let Err(error) = result {
            // A hard observation/publication failure keeps the same already-
            // admitted matched cursor as ordinary TableScan, including its roots.
            let key = self.value(self.constants, symbol.0)?;
            self.put(frame.env, 1, plan.table)?;
            self.put(frame.env, 2, key)?;
            frame.table_work = Some(TableWork::matched_read(
                if plan.host {
                    collections::TableAction::GetHost { object, dest }
                } else {
                    collections::TableAction::MemberGet { dest, symbol }
                },
                entry,
                plan.version,
            ));
            frame.phase = ScriptPhase::TableScan;
            self.publish_script_frame(self.active.frames.data.len() - 1, frame);
            return Err(error);
        }
        Ok(true)
    }

    /// Matched ordinary and direct reads share liveness, observation and value
    /// selection. A retired Host enters the source instruction's ordinary fault.
    pub(in crate::executor) fn read_matched_member(
        &mut self,
        frame: &mut ScriptFrame,
        table: Value,
        entry: u32,
        host: Option<Value>,
    ) -> Result<Option<Value>, VmError> {
        if let Some(host) = host {
            let value = match self
                .heap
                .host_property(crate::PropertyId { host, field: entry })
            {
                Ok(value) => value,
                Err(error) => {
                    self.collection_fault(frame, heap_fault(error))?;
                    return Ok(None);
                }
            };
            self.heap.observe_read(host, Some(entry))?;
            Ok(Some(value))
        } else {
            self.heap
                .observe_read(table, Some(self.heap.model_field(table, entry)?))?;
            Ok(Some(self.heap.table_entry(table, entry)?.1))
        }
    }
}
