//! One proposal-name inspection or binding edge per source-resolution step.
use super::*;

pub(in crate::heap) enum SourceWalkProgress {
    Preparing {
        work: u64,
        required_work: u64,
    },
    Resolved {
        work: u64,
        source: PropertyId,
        writable: bool,
    },
}

#[derive(Clone, Copy)]
#[repr(u32)]
enum Slot {
    Host,
    Field,
    Writable,
    Edges,
    Proposal,
    Replacement,
    Matched,
    Ready,
    ForbiddenHost,
    ForbiddenField,
    Count,
}
pub(in crate::heap) const SOURCE_WORK: u64 = 32;

impl Heap {
    fn source_walk_get(&self, walk: Value, slot: Slot) -> Result<Value, HeapError> {
        self.sequence_get(walk, slot as u32)
    }
    fn source_walk_set(&mut self, walk: Value, slot: Slot, value: Value) -> Result<(), HeapError> {
        self.set_field(walk, slot as u32, value)
    }
    pub(in crate::heap) fn begin_property_source_walk(
        &mut self,
        property: PropertyId,
        forbidden: Option<PropertyId>,
    ) -> Result<Value, HeapError> {
        let walk = self.alloc_array(&[Value::NULL; Slot::Count as usize])?;
        self.reset_property_source_walk(walk, property, forbidden)?;
        Ok(walk)
    }
    pub(in crate::heap) fn reset_property_source_walk(
        &mut self,
        walk: Value,
        property: PropertyId,
        forbidden: Option<PropertyId>,
    ) -> Result<(), HeapError> {
        let object = self.object(self.key(property.host)?)?;
        let writable = if property.field < object.observable_fields {
            self.property_access(property)?
        } else {
            self.model_position(property)?;
            true
        };
        for (slot, value) in [
            (Slot::Host, property.host),
            (Slot::Field, Value::int(i64::from(property.field))),
            (Slot::Writable, Value::bool(writable)),
            (Slot::Edges, Value::int(0)),
            (Slot::Proposal, Value::int(0)),
            (Slot::Replacement, Value::NULL),
            (Slot::Matched, Value::bool(false)),
            (Slot::Ready, Value::bool(false)),
            (
                Slot::ForbiddenHost,
                forbidden.map_or(Value::NULL, |id| id.host),
            ),
            (
                Slot::ForbiddenField,
                Value::int(forbidden.map_or(0, |id| i64::from(id.field))),
            ),
        ] {
            self.source_walk_set(walk, slot, value)?;
        }
        Ok(())
    }
    pub(in crate::heap) fn step_property_source_walk(
        &mut self,
        walk: Value,
        writes: &dyn PropertyWrites,
        work_budget: u64,
    ) -> Result<SourceWalkProgress, HeapError> {
        let mut work = 0;
        loop {
            let source = PropertyId {
                host: self.source_walk_get(walk, Slot::Host)?,
                field: self
                    .source_walk_get(walk, Slot::Field)?
                    .as_int()
                    .ok_or(HeapError::TypeMismatch)? as u32,
            };
            let writable = self.source_walk_get(walk, Slot::Writable)?.as_bool() == Some(true);
            if self.source_walk_get(walk, Slot::Ready)?.as_bool() == Some(true) {
                return Ok(SourceWalkProgress::Resolved {
                    work,
                    source,
                    writable,
                });
            }
            if work_budget - work < SOURCE_WORK {
                return Ok(SourceWalkProgress::Preparing {
                    work,
                    required_work: SOURCE_WORK,
                });
            }
            work += SOURCE_WORK;
            if self
                .source_walk_get(walk, Slot::ForbiddenHost)?
                .same_bits(source.host)
                && self.source_walk_get(walk, Slot::ForbiddenField)?.as_int()
                    == Some(i64::from(source.field))
            {
                return Err(HeapError::BindingCycle);
            }
            let proposal = self
                .source_walk_get(walk, Slot::Proposal)?
                .as_int()
                .ok_or(HeapError::TypeMismatch)? as usize;
            if proposal < writes.len() {
                let (target, value) = writes.get(proposal).ok_or(HeapError::OutOfBounds)?;
                if target.same(source) {
                    if self.source_walk_get(walk, Slot::Matched)?.as_bool() == Some(true) {
                        return Err(HeapError::PropertyConflict);
                    }
                    self.source_walk_set(walk, Slot::Matched, Value::bool(true))?;
                    if self.is_property_ref(value) {
                        self.source_walk_set(walk, Slot::Replacement, value)?;
                    }
                }
                self.source_walk_set(walk, Slot::Proposal, Value::int((proposal + 1) as i64))?;
                continue;
            }
            let replacement = self.source_walk_get(walk, Slot::Replacement)?;
            let binding = if replacement.same_bits(Value::NULL) {
                self.property_binding(source)?
            } else {
                replacement
            };
            if binding.same_bits(Value::NULL) {
                self.source_walk_set(walk, Slot::Ready, Value::bool(true))?;
                return Ok(SourceWalkProgress::Resolved {
                    work,
                    source,
                    writable,
                });
            }
            if !replacement.same_bits(Value::NULL) {
                let edges = self
                    .source_walk_get(walk, Slot::Edges)?
                    .as_int()
                    .ok_or(HeapError::TypeMismatch)? as u64
                    + 1;
                if edges > writes.len() as u64 {
                    return Err(HeapError::BindingCycle);
                }
                self.source_walk_set(walk, Slot::Edges, Value::int(edges as i64))?;
            }
            let (next, permission) = self.property_ref(binding)?;
            for (slot, value) in [
                (Slot::Host, next.host),
                (Slot::Field, Value::int(i64::from(next.field))),
                (
                    Slot::Writable,
                    Value::bool(writable && permission && self.property_access(next)?),
                ),
                (Slot::Proposal, Value::int(0)),
                (Slot::Replacement, Value::NULL),
                (Slot::Matched, Value::bool(false)),
            ] {
                self.source_walk_set(walk, slot, value)?;
            }
        }
    }
}
