//! Read-only class resolution shared by the driver and borrowed Body execution.
use super::*;

pub(in crate::executor) struct ClassReadContext<'a> {
    pub(in crate::executor) heap: &'a Heap,
    pub(in crate::executor) class_member_hints:
        &'a member_hints::MemberHints<{ member_hints::CLASS_SLOTS }>,
    #[cfg(feature = "profiling")]
    pub(in crate::executor) profile: &'a step_costs::State,
    #[cfg(test)]
    pub(in crate::executor) member_hint_hits: &'a std::cell::Cell<u64>,
}

impl ClassReadContext<'_> {
    pub(super) fn member_find_in(
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

    pub(super) fn read_member_target<'a>(
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
            self.member_hint_hits.set(self.member_hint_hits.get() + 1);
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

    pub(super) fn member_cell_from(
        &self,
        receiver: ObjectRead<'_>,
        member: ObjectRead<'_>,
        index: u32,
    ) -> Result<Value, HeapError> {
        let fields = if receiver.kind() == ObjectKind::Class {
            let owner = member.field(MemberSlot::Owner as u32)?;
            self.heap.get_field(owner, ClassSlot::Statics as u32)?
        } else {
            receiver.field(InstanceSlot::Fields as u32)?
        };
        self.heap.get_field(fields, index)
    }

    #[cfg_attr(not(feature = "profiling"), inline(always))]
    pub(in crate::executor) fn class_read_from(
        &self,
        object: Value,
        read: ObjectRead<'_>,
        name: SymbolId,
        site: Option<(FunctionId, Pc)>,
        #[cfg(feature = "profiling")] speculative: bool,
    ) -> Result<(ClassRead, Option<crate::PropertyId>), FaultKind> {
        #[cfg(feature = "profiling")]
        let span = self.profile.start_part(if speculative {
            step_costs::Part::ClassSpecError
        } else {
            step_costs::Part::ClassFinalError
        });
        let result = self.class_read_inner(object, read, name, site);
        #[cfg(feature = "profiling")]
        self.profile.finish_part(
            span,
            Some(match (&result, speculative) {
                (Ok((target, _)), true) if target.cell.is_some() => {
                    step_costs::Part::ClassSpecField
                }
                (Ok(_), true) => step_costs::Part::ClassSpecMethod,
                (Ok((target, _)), false) if target.cell.is_some() => {
                    step_costs::Part::ClassFinalField
                }
                (Ok(_), false) => step_costs::Part::ClassFinalMethod,
                (Err(_), true) => step_costs::Part::ClassSpecError,
                (Err(_), false) => step_costs::Part::ClassFinalError,
            }),
        );
        result
    }
    fn class_read_inner(
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
}
