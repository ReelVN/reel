//! Empty own fields prove an own-field miss before shared-method lookup.
//! Completion never retains this proof across a budget yield, callback or GC.
use super::*;

impl Vm {
    /// Complete the existing key-validation and empty-table scan together when
    /// their entire budget and clock batch fit. The ordinary weak binding and
    /// subsequent native binder retain callable identity and error ordering.
    pub(super) fn empty_host_method(
        &mut self,
        frame: &mut ScriptFrame,
        dest: Reg,
        object: Reg,
        symbol: SymbolId,
        key: Value,
        meter: &mut Meter,
    ) -> Result<bool, FaultKind> {
        if !meter.admits_short_steps(2) || !self.active.equality_cursors.data.is_empty() {
            return Ok(false);
        }
        let host = self.value(frame.regs, object.0).map_err(vm_fault)?;
        // Failed speculative decoding is handled by the original cursor after
        // its original admission. No author-visible error moves earlier.
        let Ok(lookup) = self.heap.host_method_work(host) else {
            return Ok(false);
        };
        let lookup = 1 + lookup;
        if meter.budget.work - meter.usage.work < 1 + lookup {
            return Ok(false);
        }
        let Ok(Some(function)) = self.shared_host_method(host, key, Some(symbol)) else {
            return Ok(false);
        };
        let (binding_work, allocation) = self.host_binding_cost(function);
        let scan = lookup + binding_work;
        if meter.budget.work - meter.usage.work < 1 + scan
            || meter.budget.allocation_bytes - meter.usage.allocation_bytes < allocation
        {
            return Ok(false);
        }
        // These are the same two logical steps and charges as TableValidate
        // followed by GetHost's empty TableScan. A literal symbol's constant is
        // already a valid immutable String; neither step invokes author code.
        assert!(meter.next_short_step());
        assert!(meter.take(0, 1, 0));
        assert!(meter.next_short_step());
        assert!(meter.take(0, scan, allocation));
        let bound = self.host_binding_value(host, function, frame.effects)?;
        self.put(frame.regs, dest.0, bound).map_err(vm_fault)?;
        self.clear(frame.env, 1).map_err(vm_fault)?;
        self.clear(frame.env, 2).map_err(vm_fault)?;
        frame.pc = frame.instruction_end;
        frame.phase = ScriptPhase::Body;
        execution_count!(self, NamedHostMethods, 1);
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
