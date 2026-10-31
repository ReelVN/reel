//! One value-resolution path for cold frames and borrowed hot Body contexts.
use super::*;

pub(in crate::executor) struct ClassBindings<'a> {
    state: &'a mut member_state::MemberState,
}

impl<'a> ClassBindings<'a> {
    pub(in crate::executor) fn new(state: &'a mut member_state::MemberState) -> Self {
        Self { state }
    }

    pub(in crate::executor) fn bind_script(
        &mut self,
        heap: &mut Heap,
        closure: Value,
        receiver: Value,
        owner: Value,
    ) -> Result<Value, HeapError> {
        let key = method_bindings::MethodKey::script(
            closure.object_parts().unwrap().1,
            receiver.object_parts().map_or(0, |(_, key)| key),
            owner.object_parts().unwrap().1,
        );
        if let Some(callable) = self.state.bindings.get(key) {
            let value = Value::object(heap.id(), callable);
            if heap.info(value).is_ok() {
                self.state.script_hits += 1;
                return Ok(value);
            }
        }
        self.state.script_misses += 1;
        let value = heap.alloc_record(ObjectKind::BoundScript, &[closure, receiver, owner])?;
        self.state
            .bindings
            .remember(key, value.object_parts().unwrap().1);
        Ok(value)
    }

    pub(in crate::executor) fn method_value(
        &mut self,
        heap: &mut Heap,
        receiver: Value,
        member: Value,
        effects: Effects,
    ) -> Result<Value, FaultKind> {
        if effects.0 & Effects::ALLOCATE == 0 {
            return Err(FaultKind::Effect);
        }
        let member = heap.read_object(member).map_err(heap_fault)?;
        let owner = member.field(MemberSlot::Owner as u32).map_err(heap_fault)?;
        let index = member
            .field(MemberSlot::Index as u32)
            .map_err(heap_fault)?
            .as_int()
            .unwrap() as u32;
        let closures = heap
            .get_field(owner, ClassSlot::Closures as u32)
            .map_err(heap_fault)?;
        let closure = heap.get_field(closures, index).map_err(heap_fault)?;
        let receiver = if heap.info(receiver).map_err(heap_fault)?.kind == ObjectKind::Class {
            Value::NULL
        } else {
            receiver
        };
        self.bind_script(heap, closure, receiver, owner)
            .map_err(heap_fault)
    }

    pub(in crate::executor) fn value(
        &mut self,
        heap: &mut Heap,
        target: ClassRead,
        effects: Effects,
    ) -> Result<Value, FaultKind> {
        if let Some(cell) = target.cell {
            cell_value(heap, cell)
        } else {
            self.method_value(heap, target.receiver, target.member, effects)
        }
    }
}

pub(super) fn cell_value(heap: &mut Heap, cell: Value) -> Result<Value, FaultKind> {
    heap.observed_cell_value(cell)
        .map_err(heap_fault)?
        .ok_or(FaultKind::UninitializedField)
}
