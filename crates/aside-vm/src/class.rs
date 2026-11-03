//! One traced layout shared by execution, model copying and property access.
use crate::{
    Heap, HeapError, ObjectKind, Value,
    bytecode::{SymbolId, TypeId},
};

#[derive(Clone, Copy)]
#[repr(u32)]
pub(crate) enum ClassSlot {
    Base,
    Function,
    Descriptor,
    Members,
    Fields,
    Statics,
    Closures,
    Types,
    State,
}
#[derive(Clone, Copy)]
#[repr(u32)]
pub(crate) enum InstanceSlot {
    Class,
    Fields,
    State,
    Initializers,
}
#[derive(Clone, Copy)]
#[repr(u32)]
pub(crate) enum MemberSlot {
    Key,
    Owner,
    Index,
    Offset,
    Method,
}

/// A borrowed data-field name and its existing language cell.
pub struct InstanceField<'a> {
    pub name: &'a str,
    pub symbol: SymbolId,
    pub cell: Value,
}
#[derive(Clone, Copy, PartialEq)]
#[repr(i64)]
pub(crate) enum State {
    Constructing,
    Ready,
    Failed,
}

impl Heap {
    /// Count data fields, including inherited fields, of a completed instance.
    pub fn instance_field_count(&self, instance: Value) -> Result<u32, HeapError> {
        let (_, fields) = self.instance_data(instance)?;
        Ok(self.read_object(fields)?.len())
    }

    /// Read the fixed field layout without allocating or invoking script methods.
    /// The caller keeps the instance rooted while retaining its field cells.
    pub fn instance_field(
        &self,
        instance: Value,
        index: u32,
    ) -> Result<InstanceField<'_>, HeapError> {
        let (class, fields) = self.instance_data(instance)?;
        let layout = self.get_field(class, ClassSlot::Fields as u32)?;
        let member = self.get_field(layout, index)?;
        let key = self
            .get_field(member, MemberSlot::Key as u32)?
            .as_int()
            .unwrap();
        let symbol = SymbolId((key / 2) as u32);
        let code = self.code.as_ref().ok_or(HeapError::WrongKind)?;
        Ok(InstanceField {
            name: &code.package().symbols[symbol.index()],
            symbol,
            cell: self.get_field(fields, index)?,
        })
    }

    pub(crate) fn instance_data(&self, instance: Value) -> Result<(Value, Value), HeapError> {
        let instance_read = self.read_object(instance)?;
        if instance_read.kind() != ObjectKind::Instance {
            return Err(HeapError::WrongKind);
        }
        match instance_read.field(InstanceSlot::State as u32)?.as_int() {
            Some(state) if state == State::Ready as i64 => {}
            Some(state) if state == State::Failed as i64 => {
                return Err(HeapError::ConstructionFailed);
            }
            _ => return Err(HeapError::UninitializedCell),
        }
        Ok((
            instance_read.field(InstanceSlot::Class as u32)?,
            instance_read.field(InstanceSlot::Fields as u32)?,
        ))
    }
}

pub(crate) fn accepts_nominal(heap: &Heap, value: Value, ty: TypeId) -> Result<bool, HeapError> {
    if value.tag() != crate::ValueTag::Object {
        return Ok(false);
    }
    match heap.read_object(value)?.kind() {
        ObjectKind::Host => return heap.accepts_host_nominal(value, ty),
        ObjectKind::Instance => {}
        _ => return Ok(false),
    }
    if heap
        .code
        .as_ref()
        .is_some_and(|code| code.checks.host_types.contains(ty))
    {
        return Ok(false);
    }
    let class = heap.get_field(value, InstanceSlot::Class as u32)?;
    let types = heap.get_field(class, ClassSlot::Types as u32)?;
    let bits = heap.get_field(types, ty.0 / 64)?.as_int().unwrap();
    Ok(bits & (1i64 << (ty.0 % 64)) != 0)
}
