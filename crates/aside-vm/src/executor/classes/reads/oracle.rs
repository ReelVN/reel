//! Original per-access class metadata resolution, compiled only for comparison.
use super::*;

impl Vm {
    pub(super) fn checked_member_find_in(
        &self,
        members: Value,
        len: u32,
        key: i64,
    ) -> Result<(Value, u32), HeapError> {
        let mut lo = 0;
        let mut hi = len;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let member = self.heap.get_field(members, mid)?;
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
            let member = self.heap.get_field(members, lo)?;
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
    pub(super) fn checked_member_read_target(
        &self,
        object: Value,
        name: SymbolId,
        site: Option<(FunctionId, Pc)>,
    ) -> Result<(Value, Value, u64), FaultKind> {
        let (receiver, class, is_static) = match self.heap.info(object).map_err(heap_fault)?.kind {
            ObjectKind::Instance => (
                object,
                self.instance_slot(object, InstanceSlot::Class)
                    .map_err(heap_fault)?,
                false,
            ),
            ObjectKind::Class => (object, object, true),
            ObjectKind::Super => (
                self.heap.get_field(object, 0).map_err(heap_fault)?,
                self.heap.get_field(object, 1).map_err(heap_fault)?,
                false,
            ),
            _ => return Err(FaultKind::Type),
        };
        self.class_alive(receiver)?;
        let members = self
            .class_slot(class, ClassSlot::Members)
            .map_err(heap_fault)?;
        let info = self.heap.info(members).map_err(heap_fault)?;
        let key = i64::from(name.0) * 2 + i64::from(is_static);
        let cached = site.and_then(|(function, pc)| {
            let (entry, true) =
                self.member_state
                    .class_hints
                    .get(function, pc, members, info.content_version)?
            else {
                return None;
            };
            if entry >= info.len {
                return None;
            }
            // Resolve the current full member generation. A class/instance
            // switch at the same PC changes the static bit even for one name.
            let member = self.heap.get_field(members, entry).ok()?;
            (self
                .heap
                .get_field(member, MemberSlot::Key as u32)
                .ok()?
                .as_int()
                == Some(key))
            .then_some(member)
        });
        let member = if let Some(member) = cached {
            #[cfg(test)]
            self.work_probe
                .member_hint_hits
                .set(self.work_probe.member_hint_hits.get() + 1);
            member
        } else {
            let (member, entry) = self
                .checked_member_find_in(members, info.len, key)
                .map_err(heap_fault)?;
            if !member.same_bits(Value::NULL)
                && let Some((function, pc)) = site
            {
                self.member_state.class_hints.remember(
                    function,
                    pc,
                    entry,
                    members,
                    info.content_version,
                );
            }
            member
        };
        if member.same_bits(Value::NULL) {
            return Err(FaultKind::MissingKey);
        }
        Ok((
            receiver,
            member,
            12 + u64::from(info.len.checked_ilog2().unwrap_or(0)) * 3,
        ))
    }
    pub(super) fn checked_member_cell(
        &self,
        receiver: Value,
        member: Value,
    ) -> Result<Value, HeapError> {
        let index = self
            .heap
            .get_field(member, MemberSlot::Offset as u32)?
            .as_int()
            .unwrap() as u32;
        let fields = if self.heap.info(receiver)?.kind == ObjectKind::Class {
            let owner = self.heap.get_field(member, MemberSlot::Owner as u32)?;
            self.class_slot(owner, ClassSlot::Statics)?
        } else {
            self.instance_slot(receiver, InstanceSlot::Fields)?
        };
        self.heap.get_field(fields, index)
    }
    pub(super) fn checked_class_read_plan(
        &self,
        object: Value,
        name: SymbolId,
        site: Option<(FunctionId, Pc)>,
    ) -> Result<(ClassRead, Option<crate::PropertyId>), FaultKind> {
        let (receiver, member, work) = self.checked_member_read_target(object, name, site)?;
        if self
            .heap
            .get_field(member, MemberSlot::Method as u32)
            .map_err(heap_fault)?
            .as_bool()
            == Some(true)
        {
            return Ok((
                ClassRead {
                    receiver,
                    member,
                    cell: None,
                    work,
                },
                None,
            ));
        }
        let cell = self
            .checked_member_cell(receiver, member)
            .map_err(heap_fault)?;
        let property = (self.heap.reactive_evaluating()
            && self.heap.is_model_cell(cell).map_err(heap_fault)?)
        .then_some(crate::PropertyId {
            host: cell,
            field: 0,
        });
        Ok((
            ClassRead {
                receiver,
                member,
                cell: Some(cell),
                work,
            },
            property,
        ))
    }
}
