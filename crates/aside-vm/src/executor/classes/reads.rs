//! Class point reads retain validated metadata only during synchronous planning.
use super::*;
use crate::heap::ObjectRead;

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
    pub(super) fn member_find(&self, class: Value, key: i64) -> Result<(Value, u32), HeapError> {
        let members = self.class_slot(class, ClassSlot::Members)?;
        let read = self.heap.read_object(members)?;
        self.member_find_in(read, read.info().len, key)
    }

    fn member_find_in(
        &self,
        members: ObjectRead<'_>,
        len: u32,
        key: i64,
    ) -> Result<(Value, u32), HeapError> {
        let mut lo = 0;
        let mut hi = len;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let member = members.field(mid)?;
            if self
                .heap
                .get_field(member, MemberSlot::Key as u32)?
                .as_int()
                .unwrap()
                < key
            {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let found = if lo < len {
            let member = members.field(lo)?;
            (self
                .heap
                .get_field(member, MemberSlot::Key as u32)?
                .as_int()
                == Some(key))
            .then_some(member)
        } else {
            None
        };
        Ok((found.unwrap_or(Value::NULL), lo))
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
        let target = self.read_member_target(object, object_read, name, None)?;
        Ok((target.receiver, target.member))
    }

    fn read_member_target<'a>(
        &'a self,
        object: Value,
        object_read: ObjectRead<'a>,
        name: SymbolId,
        site: Option<(FunctionId, Pc)>,
    ) -> Result<ReadTarget<'a>, FaultKind> {
        let (receiver, receiver_read, class, is_static) = match object_read.kind() {
            ObjectKind::Instance => (
                object,
                object_read,
                object_read
                    .field(InstanceSlot::Class as u32)
                    .map_err(heap_fault)?,
                false,
            ),
            ObjectKind::Class => (object, object_read, object, true),
            ObjectKind::Super => {
                let receiver = object_read.field(0).map_err(heap_fault)?;
                let class = object_read.field(1).map_err(heap_fault)?;
                let read = self.heap.read_object(receiver).map_err(heap_fault)?;
                (receiver, read, class, false)
            }
            _ => return Err(FaultKind::Type),
        };
        let state = match receiver_read.kind() {
            ObjectKind::Class => ClassSlot::State as u32,
            ObjectKind::Instance => InstanceSlot::State as u32,
            _ => return Err(FaultKind::Type),
        };
        if receiver_read.field(state).map_err(heap_fault)?.as_int() == Some(State::Failed as i64) {
            return Err(FaultKind::ConstructionFailed);
        }
        let class_read = if class.same_bits(receiver) {
            receiver_read
        } else {
            self.heap.read_object(class).map_err(heap_fault)?
        };
        let members = class_read
            .field(ClassSlot::Members as u32)
            .map_err(heap_fault)?;
        let members_read = self.heap.read_object(members).map_err(heap_fault)?;
        let info = members_read.info();
        let key = i64::from(name.0) * 2 + i64::from(is_static);
        let cached = site.and_then(|(function, pc)| {
            let (entry, true) =
                self.class_member_hints
                    .get(function, pc, members, info.content_version)?
            else {
                return None;
            };
            if entry >= info.len {
                return None;
            }
            let member = members_read.field(entry).ok()?;
            let read = self.heap.read_object(member).ok()?;
            (read.field(MemberSlot::Key as u32).ok()?.as_int() == Some(key))
                .then_some((member, read))
        });
        let (member, member_read) = if let Some(member) = cached {
            #[cfg(test)]
            self.work_probe
                .member_hint_hits
                .set(self.work_probe.member_hint_hits.get() + 1);
            member
        } else {
            let (member, entry) = self
                .member_find_in(members_read, info.len, key)
                .map_err(heap_fault)?;
            if member.same_bits(Value::NULL) {
                return Err(FaultKind::MissingKey);
            }
            if let Some((function, pc)) = site {
                self.class_member_hints.remember(
                    function,
                    pc,
                    entry,
                    members,
                    info.content_version,
                );
            }
            (member, self.heap.read_object(member).map_err(heap_fault)?)
        };
        Ok(ReadTarget {
            receiver,
            receiver_read,
            member,
            member_read,
            work: 12 + u64::from(info.len.checked_ilog2().unwrap_or(0)) * 3,
        })
    }

    pub(super) fn member_cell(&self, receiver: Value, member: Value) -> Result<Value, HeapError> {
        let member = self.heap.read_object(member)?;
        let index = member.field(MemberSlot::Offset as u32)?.as_int().unwrap() as u32;
        let receiver = self.heap.read_object(receiver)?;
        self.member_cell_from(receiver, member, index)
    }

    fn member_cell_from(
        &self,
        receiver: ObjectRead<'_>,
        member: ObjectRead<'_>,
        index: u32,
    ) -> Result<Value, HeapError> {
        let fields = if receiver.kind() == ObjectKind::Class {
            let owner = member.field(MemberSlot::Owner as u32)?;
            self.class_slot(owner, ClassSlot::Statics)?
        } else {
            receiver.field(InstanceSlot::Fields as u32)?
        };
        self.heap.get_field(fields, index)
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
        let target = self.read_member_target(object, read, name, site)?;
        if target
            .member_read
            .field(MemberSlot::Method as u32)
            .map_err(heap_fault)?
            .as_bool()
            == Some(true)
        {
            return Ok((
                ClassRead {
                    receiver: target.receiver,
                    member: target.member,
                    cell: None,
                    work: target.work,
                },
                None,
            ));
        }
        let index = target
            .member_read
            .field(MemberSlot::Offset as u32)
            .map_err(heap_fault)?
            .as_int()
            .unwrap() as u32;
        let cell = self
            .member_cell_from(target.receiver_read, target.member_read, index)
            .map_err(heap_fault)?;
        let property = (self.heap.reactive_evaluating()
            && self.heap.is_model_cell(cell).map_err(heap_fault)?)
        .then_some(crate::PropertyId {
            host: cell,
            field: 0,
        });
        Ok((
            ClassRead {
                receiver: target.receiver,
                member: target.member,
                cell: Some(cell),
                work: target.work,
            },
            property,
        ))
    }

    pub(super) fn class_method_value(
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
        let receiver = if self.heap.info(receiver).map_err(heap_fault)?.kind == ObjectKind::Class {
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
            self.class_cell_value(cell)?
        } else {
            self.class_method_value(target.receiver, target.member, s.effects)?
        };
        self.write(s.regs, dest, value)?;
        s.pc = next;
        Ok(true)
    }
}
