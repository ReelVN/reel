//! Missing entries evaluate a factory and then recheck the original key.
use super::*;

impl Vm {
    pub(in crate::executor) fn entry_factory_step(
        &mut self,
        s: &mut ScriptFrame,
        call: BuiltinCall,
        waiting: bool,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        if waiting {
            if !meter.take(0, 3, 0) {
                return Ok(false);
            }
            let table = self.value(s.env, 1)?;
            let key = self.value(s.env, 2)?;
            if let Err(kind) = self.start_table(
                s,
                table,
                key,
                TableAction::EntryOrInsert { call, filled: true },
            ) {
                self.collection_fault(s, kind)?;
            }
            return Ok(true);
        }
        let callback = self.builtin_argument(s, call, 1)?;
        let Ok(function) = self.closure_function(callback) else {
            if !meter.take(0, 3, 0) {
                return Ok(false);
            }
            self.collection_fault(s, FaultKind::Type)?;
            return Ok(true);
        };
        self.push_builtin_callback(
            s,
            function,
            callback,
            0,
            ScriptPhase::EntryFactory {
                call,
                waiting: true,
            },
            meter,
        )
    }

    pub(in crate::executor) fn finish_table_entry(
        &mut self,
        s: &mut ScriptFrame,
        call: BuiltinCall,
        value: Value,
    ) -> Result<(), VmError> {
        s.table_work = None;
        self.builtin_finish(s, call, Ok(value))?;
        self.clear(s.env, 3)
    }
}
