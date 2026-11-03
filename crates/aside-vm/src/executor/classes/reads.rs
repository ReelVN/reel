//! Class point reads retain validated metadata only during synchronous planning.
use super::*;
use crate::heap::ObjectRead;
mod context;
pub(in crate::executor) use context::ClassReadContext;

#[cfg(test)]
mod oracle;

/// Logical owners for one admission only; no borrowed storage crosses a yield.
pub(in crate::executor) struct ClassRead {
    receiver: Value,
    member: Value,
    cell: Option<Value>,
    work: u64,
}

impl ClassRead {
    pub(in crate::executor) fn cell(&self) -> Option<Value> {
        self.cell
    }
    pub(in crate::executor) fn work(&self) -> u64 {
        self.work
    }
}

struct ReadTarget<'a> {
    receiver: Value,
    receiver_read: ObjectRead<'a>,
    member: Value,
    member_read: ObjectRead<'a>,
    work: u64,
}

impl Vm {
    fn class_read_context(&self) -> ClassReadContext<'_> {
        ClassReadContext {
            heap: &self.heap,
            class_member_hints: &self.class_member_hints,
            #[cfg(feature = "profiling")]
            profile: &self.step_costs,
            #[cfg(test)]
            member_hint_hits: &self.work_probe.member_hint_hits,
        }
    }

    pub(super) fn member_find(&self, class: Value, key: i64) -> Result<(Value, u32), HeapError> {
        let members = self.class_slot(class, ClassSlot::Members)?;
        let read = self.heap.read_object(members)?;
        self.class_read_context()
            .member_find_in(read, read.len(), key)
    }

    pub(super) fn member_target(
        &self,
        object: Value,
        name: SymbolId,
    ) -> Result<(Value, Value), FaultKind> {
        #[cfg(test)]
        if self.work_probe.checked_class_metadata {
            let (receiver, member, _) = self.checked_member_read_target(object, name, None)?;
            return Ok((receiver, member));
        }
        let object_read = self.heap.read_object(object).map_err(heap_fault)?;
        let context = self.class_read_context();
        let target = context.read_member_target(object, object_read, name, None)?;
        Ok((target.receiver, target.member))
    }

    pub(super) fn member_cell(&self, receiver: Value, member: Value) -> Result<Value, HeapError> {
        let member = self.heap.read_object(member)?;
        let index = member.field(MemberSlot::Offset as u32)?.as_int().unwrap() as u32;
        let receiver = self.heap.read_object(receiver)?;
        self.class_read_context()
            .member_cell_from(receiver, member, index)
    }

    pub(in crate::executor) fn class_read_plan(
        &self,
        object: Value,
        name: SymbolId,
        site: Option<(FunctionId, Pc)>,
    ) -> Result<(ClassRead, Option<crate::PropertyId>), FaultKind> {
        let read = self.heap.read_object(object).map_err(heap_fault)?;
        self.class_read_from(object, read, name, site)
    }

    pub(in crate::executor) fn class_read_from(
        &self,
        object: Value,
        read: ObjectRead<'_>,
        name: SymbolId,
        site: Option<(FunctionId, Pc)>,
    ) -> Result<(ClassRead, Option<crate::PropertyId>), FaultKind> {
        #[cfg(test)]
        if self.work_probe.checked_class_metadata {
            return self.checked_class_read_plan(object, name, site);
        }
        self.class_read_context().class_read_from(
            object,
            read,
            name,
            site,
            #[cfg(feature = "profiling")]
            false,
        )
    }

    #[cfg_attr(not(feature = "profiling"), inline(always))]
    pub(super) fn class_method_value(
        &mut self,
        receiver: Value,
        member: Value,
        effects: Effects,
    ) -> Result<Value, FaultKind> {
        #[cfg(feature = "profiling")]
        let span = self
            .step_costs
            .start_part(step_costs::Part::MethodScriptError);
        #[cfg(feature = "profiling")]
        let hits = self.script_binding_hits;
        let result = self.class_method_value_inner(receiver, member, effects);
        #[cfg(feature = "profiling")]
        self.step_costs.finish_part(
            span,
            Some(if result.is_err() {
                step_costs::Part::MethodScriptError
            } else if self.script_binding_hits != hits {
                step_costs::Part::MethodScriptHit
            } else {
                step_costs::Part::MethodScriptMiss
            }),
        );
        result
    }
    fn class_method_value_inner(
        &mut self,
        receiver: Value,
        member: Value,
        effects: Effects,
    ) -> Result<Value, FaultKind> {
        if effects.0 & Effects::ALLOCATE == 0 {
            return Err(FaultKind::Effect);
        }
        let member = self.heap.read_object(member).map_err(heap_fault)?;
        let owner = member.field(MemberSlot::Owner as u32).map_err(heap_fault)?;
        let index = member
            .field(MemberSlot::Index as u32)
            .map_err(heap_fault)?
            .as_int()
            .unwrap() as u32;
        let closures = self
            .class_slot(owner, ClassSlot::Closures)
            .map_err(heap_fault)?;
        let closure = self.heap.get_field(closures, index).map_err(heap_fault)?;
        let receiver =
            if self.heap.read_object(receiver).map_err(heap_fault)?.kind() == ObjectKind::Class {
                Value::NULL
            } else {
                receiver
            };
        self.bind_script(closure, receiver, owner)
            .map_err(heap_fault)
    }

    pub(super) fn class_cell_value(&mut self, cell: Value) -> Result<Value, FaultKind> {
        self.heap
            .observed_cell_value(cell)
            .map_err(heap_fault)?
            .ok_or(FaultKind::UninitializedField)
    }

    pub(in crate::executor) fn execute_class_read(
        &mut self,
        s: &mut ScriptFrame,
        target: ClassRead,
        dest: Reg,
        next: Pc,
    ) -> Result<bool, FaultKind> {
        let value = if let Some(cell) = target.cell {
            part_cost!(
                self.step_costs,
                ClassFieldValue,
                self.class_cell_value(cell)
            )?
        } else {
            self.class_method_value(target.receiver, target.member, s.effects)?
        };
        self.write(s.regs, dest, value)?;
        s.pc = next;
        Ok(true)
    }
}
