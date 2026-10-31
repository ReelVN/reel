//! Class point reads retain validated metadata only during synchronous planning.
use super::*;
use crate::heap::ObjectRead;

mod lookup;
#[cfg(test)]
mod oracle;
pub(in crate::executor) use lookup::ClassReader;

/// Logical owners for one admission only; no borrowed storage crosses a yield.
pub(in crate::executor) struct ClassRead {
    pub(super) receiver: Value,
    pub(super) member: Value,
    pub(super) cell: Option<Value>,
    pub(super) work: u64,
}

impl ClassRead {
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
    fn class_reader(&self) -> ClassReader<'_> {
        let reader = ClassReader::new(&self.heap, &self.member_state.class_hints);
        #[cfg(test)]
        let reader = reader.with_hint_hits(&self.work_probe.member_hint_hits);
        reader
    }
    pub(super) fn member_find(&self, class: Value, key: i64) -> Result<(Value, u32), HeapError> {
        self.class_reader().member_find(class, key)
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
        self.class_reader().target(object, name)
    }
    pub(super) fn member_cell(&self, receiver: Value, member: Value) -> Result<Value, HeapError> {
        self.class_reader().member_cell(receiver, member)
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
        self.class_reader().plan(object, read, name, site)
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

    pub(super) fn class_method_value(
        &mut self,
        receiver: Value,
        member: Value,
        effects: Effects,
    ) -> Result<Value, FaultKind> {
        ClassBindings::new(&mut self.member_state).method_value(
            &mut self.heap,
            receiver,
            member,
            effects,
        )
    }

    pub(super) fn class_cell_value(&mut self, cell: Value) -> Result<Value, FaultKind> {
        bindings::cell_value(&mut self.heap, cell)
    }

    pub(in crate::executor) fn execute_class_read(
        &mut self,
        s: &mut ScriptFrame,
        target: ClassRead,
        dest: Reg,
        next: Pc,
    ) -> Result<bool, FaultKind> {
        let value =
            ClassBindings::new(&mut self.member_state).value(&mut self.heap, target, s.effects)?;
        self.write(s.regs, dest, value)?;
        s.pc = next;
        Ok(true)
    }
}
