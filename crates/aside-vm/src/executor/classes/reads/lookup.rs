//! Shared immutable class lookup inside one synchronous execution context.
use super::*;

pub(in crate::executor) struct ClassReader<'a> {
    heap: &'a Heap,
    hints: &'a member_hints::MemberHints<{ member_hints::CLASS_SLOTS }>,
    #[cfg(test)]
    hint_hits: Option<&'a std::cell::Cell<u64>>,
}

impl<'a> ClassReader<'a> {
    pub(in crate::executor) fn new(
        heap: &'a Heap,
        hints: &'a member_hints::MemberHints<{ member_hints::CLASS_SLOTS }>,
    ) -> Self {
        Self {
            heap,
            hints,
            #[cfg(test)]
            hint_hits: None,
        }
    }
    #[cfg(test)]
    pub(in crate::executor) fn with_hint_hits(mut self, hits: &'a std::cell::Cell<u64>) -> Self {
        self.hint_hits = Some(hits);
        self
    }
    fn class_slot(&self, class: Value, slot: ClassSlot) -> Result<Value, HeapError> {
        self.heap.get_field(class, slot as u32)
    }
    pub(in crate::executor) fn member_find(
        &self,
        class: Value,
        key: i64,
    ) -> Result<(Value, u32), HeapError> {
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
    pub(in crate::executor) fn member_cell(
        &self,
        receiver: Value,
        member: Value,
    ) -> Result<Value, HeapError> {
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
    fn read_member_target<'b>(
        &'b self,
        object: Value,
        object_read: ObjectRead<'b>,
        name: SymbolId,
        site: Option<(FunctionId, Pc)>,
    ) -> Result<ReadTarget<'b>, FaultKind> {
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
            let (entry, true) = self
                .hints
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
            if let Some(hits) = self.hint_hits {
                hits.set(hits.get() + 1);
            }
            member
        } else {
            let (member, entry) = self
                .member_find_in(members_read, info.len, key)
                .map_err(heap_fault)?;
            if member.same_bits(Value::NULL) {
                return Err(FaultKind::MissingKey);
            }
            if let Some((function, pc)) = site {
                self.hints
                    .remember(function, pc, entry, members, info.content_version);
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
    pub(in crate::executor) fn plan(
        &self,
        object: Value,
        read: ObjectRead<'_>,
        name: SymbolId,
        site: Option<(FunctionId, Pc)>,
    ) -> Result<(ClassRead, Option<crate::PropertyId>), FaultKind> {
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
    pub(in crate::executor) fn target(
        &self,
        object: Value,
        name: SymbolId,
    ) -> Result<(Value, Value), FaultKind> {
        let object_read = self.heap.read_object(object).map_err(heap_fault)?;
        let target = self.read_member_target(object, object_read, name, None)?;
        Ok((target.receiver, target.member))
    }
}
