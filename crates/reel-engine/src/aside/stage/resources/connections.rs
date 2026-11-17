//! Validate completed constructor fields before reconnecting saved binding edges.
use super::*;

// All nodes and references remain rooted by TypedRestore's object array. The
// private node payload chains only reconstruction work and is cleared on finish.
#[derive(Default)]
pub(in crate::aside::stage) struct Restoring {
    head: Option<HeapHandle>,
    field: u32,
    binding: u32,
}
const NEXT: u32 = 0;
const BINDINGS: u32 = 1;
pub(super) fn binding_count(
    heap: &Heap,
    value: Value,
    shape: snapshot::Shape,
) -> Result<u32, TypedDataError> {
    let mut count = 0;
    for (field, _) in names(shape).enumerate() {
        if heap
            .property_binding(PropertyId {
                host: value,
                field: field as u32,
            })?
            .tag()
            != ValueTag::Null
        {
            count += 1;
        }
    }
    Ok(count)
}
pub(super) fn capture_binding(
    heap: &Heap,
    value: Value,
    shape: snapshot::Shape,
    mut index: u32,
) -> Result<(std::borrow::Cow<'static, str>, Value), TypedDataError> {
    for (field, name) in names(shape).enumerate() {
        let binding = heap.property_binding(PropertyId {
            host: value,
            field: field as u32,
        })?;
        if binding.tag() != ValueTag::Null {
            if index == 0 {
                return Ok((format!("binding.{name}").into(), binding));
            }
            index -= 1;
        }
    }
    Err(TypedDataError::Contract("invalid scene binding edge"))
}
pub(super) fn admit(
    heap: &mut Heap,
    stage: &mut StageBinding,
    value: Value,
) -> Result<(), AdapterError> {
    let next = stage
        .restoring_resources
        .head
        .map(|handle| heap.import_handle(handle))
        .transpose()?
        .unwrap_or(Value::NULL);
    let bindings = heap.alloc_array(&[])?;
    let pending = heap.alloc_array(&[next, bindings])?;
    heap.set_host_payload(value, 5, pending)?;
    stage.restoring_resources.head = Some(heap.handle(value)?);
    Ok(())
}
pub(super) fn connect_binding(
    heap: &mut Heap,
    value: Value,
    field: u32,
    reference: Value,
) -> Result<(), TypedDataError> {
    heap.property_ref(reference)?;
    let bindings = heap.sequence_get(heap.host_payload(value, 5)?, BINDINGS)?;
    heap.array_push(bindings, Value::int(i64::from(field)))?;
    heap.array_push(bindings, reference)?;
    Ok(())
}
pub(super) fn finish_step(
    heap: &mut Heap,
    stage: &mut StageBinding,
) -> Result<bool, TypedDataError> {
    let Some(handle) = stage.restoring_resources.head else {
        return Ok(true);
    };
    let value = heap.import_handle(handle)?;
    let id = stage.id(heap, value)?;
    let shape = stage.handles[&id].shape;
    let count = (shape.prefix() + NodeProperty::PROPERTIES.len()) as u32;
    if stage.restoring_resources.field < count {
        let field = stage.restoring_resources.field;
        let writable = schema::properties(shape)
            .get(field as usize)
            .is_none_or(|property| property.writable);
        if writable && !(shape == snapshot::Shape::Video && field == 0) {
            let property = PropertyId { host: value, field };
            heap.validate_property_write(property, heap.host_property(property)?)
                .map_err(|error| TypedDataError::Field {
                    path: format!(
                        "{}.{}",
                        schema::kind(shape).unwrap(),
                        names(shape).nth(field as usize).unwrap()
                    ),
                    reason: error.to_string(),
                })?;
        }
        stage.restoring_resources.field += 1;
        return Ok(false);
    }
    let pending = heap.host_payload(value, 5)?;
    let bindings = heap.sequence_get(pending, BINDINGS)?;
    let index = stage.restoring_resources.binding;
    if index < heap.sequence_len(bindings)? {
        let field = heap
            .sequence_get(bindings, index)?
            .as_int()
            .ok_or(HeapError::TypeMismatch)? as u32;
        heap.bind_host_property(
            PropertyId { host: value, field },
            heap.sequence_get(bindings, index + 1)?,
        )?;
        stage.restoring_resources.binding += 2;
        return Ok(false);
    }
    let next = heap.sequence_get(pending, NEXT)?;
    heap.set_host_payload(value, 5, Value::NULL)?;
    stage.restoring_resources.head = if next.tag() == ValueTag::Null {
        None
    } else {
        Some(heap.handle(next)?)
    };
    stage.restoring_resources.field = 0;
    stage.restoring_resources.binding = 0;
    Ok(false)
}
