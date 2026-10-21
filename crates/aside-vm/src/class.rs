//! One traced layout shared by execution, model copying and property access.
use crate::{Heap, HeapError, ObjectKind, Value, bytecode::TypeId};

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
#[derive(Clone, Copy, PartialEq)]
#[repr(i64)]
pub(crate) enum State {
    Constructing,
    Ready,
    Failed,
}

impl Heap {
    pub(crate) fn instance_data(&self, instance: Value) -> Result<(Value, Value), HeapError> {
        if self.info(instance)?.kind != ObjectKind::Instance {
            return Err(HeapError::WrongKind);
        }
        match self
            .get_field(instance, InstanceSlot::State as u32)?
            .as_int()
        {
            Some(state) if state == State::Ready as i64 => {}
            Some(state) if state == State::Failed as i64 => {
                return Err(HeapError::ConstructionFailed);
            }
            _ => return Err(HeapError::UninitializedCell),
        }
        Ok((
            self.get_field(instance, InstanceSlot::Class as u32)?,
            self.get_field(instance, InstanceSlot::Fields as u32)?,
        ))
    }
}

pub(crate) fn accepts_nominal(heap: &Heap, value: Value, ty: TypeId) -> Result<bool, HeapError> {
    if value.tag() != crate::ValueTag::Object {
        return Ok(false);
    }
    match heap.info(value)?.kind {
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
