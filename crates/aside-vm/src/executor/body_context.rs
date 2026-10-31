//! Short Body capability created only for a classified member instruction.
use super::body_cursor::{Cursor, Exit};
use super::registers::Registers;
use super::*;

pub(super) struct Context<'a, 'b> {
    pub registers: &'a mut Registers<'b>,
    pub cursor: &'a mut Cursor,
    pub effects: Effects,
    pub members: &'a mut member_state::MemberState,
    pub prepared_read: &'a mut Option<script_body::PreparedRead>,
    #[cfg(feature = "profiling")]
    pub profile: &'a step_costs::State,
    #[cfg(feature = "execution-counts")]
    pub execution_counts: &'a execution_counts::State,
    #[cfg(test)]
    pub general_class_reads: bool,
    #[cfg(test)]
    pub hint_hits: &'a std::cell::Cell<u64>,
    #[cfg(test)]
    pub hot_class_reads: &'a std::cell::Cell<u64>,
}

impl Context<'_, '_> {
    pub(super) fn class_read(
        &mut self,
        [dest, source, symbol]: [u32; 3],
        next: Pc,
        meter: &mut Meter,
    ) -> Result<Exit, VmError> {
        #[cfg(test)]
        if self.general_class_reads {
            return Ok(Exit::General);
        }
        self.cursor.member_symbol = Some(SymbolId(symbol));
        let Some((target, property, reactive)) = step_cost!(
            self.profile,
            Cost,
            self.point(),
            self.prepare_class_read(source, SymbolId(symbol))
        ) else {
            return Ok(Exit::General);
        };
        execution_count!(self, MemberGetClassCosts, 1);
        if property.is_some() {
            // Freshness may need frame admission and a callback. End this view
            // before the original Body path decides; never repeat the lookup.
            *self.prepared_read = Some(script_body::PreparedRead::Class { target, property });
            return Ok(Exit::General);
        }
        #[cfg(feature = "execution-counts")]
        self.execution_counts.step(&ScriptPhase::Body);
        let work = target.work() * (1 + u64::from(reactive));
        if !meter.take(1, work, Values::allocation_bytes(3)) {
            return Ok(Exit::Budget);
        }
        self.cursor.instruction = None;
        instruction_admitted!(self, Opcode::MemberGet);
        execution_count!(self, BytecodeExecutions, 1);
        #[cfg(feature = "profiling")]
        let point = self.point();
        let members = &mut *self.members;
        let effects = self.effects;
        let value = step_cost!(
            self.profile,
            Execute,
            point,
            self.registers
                .with_heap(|heap| classes::ClassBindings::new(members).value(heap, target, effects))
        );
        let value = match value {
            Ok(value) => value,
            Err(kind) => return Ok(Exit::Fault(kind)),
        };
        if let Err(error) = self.registers.write(dest, value) {
            return Ok(Exit::Fault(heap_fault(error)));
        }
        // Body publishes its literal member symbol until the following logical
        // step, including when the instruction quota ends immediately here.
        self.cursor.pc = next;
        #[cfg(test)]
        self.hot_class_reads.set(self.hot_class_reads.get() + 1);
        Ok(Exit::Done)
    }

    fn prepare_class_read(
        &mut self,
        source: u32,
        symbol: SymbolId,
    ) -> Option<(classes::ClassRead, Option<crate::PropertyId>, bool)> {
        let object = self.registers.read(source).ok()?;
        let heap = self.registers.heap();
        let read = heap.read_object(object).ok()?;
        let info = read.info();
        // Other MemberGet kinds reuse this already-validated classification in
        // the immediate fallback instead of reading the root and handle twice.
        *self.prepared_read = Some(script_body::PreparedRead::Member { object, info });
        if !matches!(
            info.kind,
            ObjectKind::Class | ObjectKind::Instance | ObjectKind::Super
        ) {
            return None;
        }
        let reader = classes::ClassReader::new(heap, &self.members.class_hints);
        #[cfg(test)]
        let reader = reader.with_hint_hits(self.hint_hits);
        let (target, property) = reader
            .plan(
                object,
                read,
                symbol,
                Some((self.cursor.function, self.cursor.pc)),
            )
            .ok()?;
        *self.prepared_read = None;
        Some((target, property, heap.reactive_evaluating()))
    }
}

impl Context<'_, '_> {
    #[cfg(feature = "profiling")]
    fn point(&self) -> Option<step_costs::StepPoint> {
        Some(step_costs::StepPoint {
            function: self.cursor.function.0,
            pc: self.cursor.pc.0,
            native: None,
            mode: 0,
        })
    }
}
