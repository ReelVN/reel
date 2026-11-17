//! Logical speakers retain their data and the current registered native call.
use super::*;
use crate::aside::data::{LogicalReference, TypedDataError};
use std::borrow::Cow;

pub(in crate::aside) fn character_resource_header(
    heap: &Heap,
    value: Value,
) -> Option<LogicalReference> {
    (heap.host_payload_len(value).ok()? == 3
        && heap.host_payload(value, 0).ok()?.as_int() == Some(CHARACTER_TAG))
    .then(|| LogicalReference {
        kind: "reel.character".into(),
        version: 1,
        fields: 2,
        payload: Vec::new(),
    })
}
pub(in crate::aside) fn character_resource_field(
    heap: &Heap,
    value: Value,
    index: u32,
) -> Result<(Cow<'static, str>, Value), TypedDataError> {
    let name = match index {
        0 => "id",
        1 => "name",
        _ => return Err(TypedDataError::Contract("invalid Character field")),
    };
    Ok((name.into(), heap.host_payload(value, index + 1)?))
}
pub(in crate::aside) fn allocate_character_resource(heap: &mut Heap) -> Result<Value, HeapError> {
    let fields = heap.alloc_table()?;
    for (index, name) in ["id", "name"].into_iter().enumerate() {
        let key = heap.alloc_string(name)?;
        heap.table_store(fields, index as u32, key, Value::NULL)?;
    }
    let value = heap.alloc_host(
        fields,
        &[Value::int(CHARACTER_TAG), Value::NULL, Value::NULL],
        Some(&IMMUTABLE),
    )?;
    heap.set_host_type(value, heap.host_type("reel::types::Character")?)?;
    Ok(value)
}
pub(in crate::aside) fn connect_character_resource(
    heap: &mut Heap,
    value: Value,
    name: &str,
    field: Value,
) -> Result<(), HeapError> {
    let index = match name {
        "id" => 0,
        "name" => 1,
        _ => return Err(HeapError::TypeMismatch),
    };
    let info = heap.info(field)?;
    if info.kind != ObjectKind::String || index == 0 && info.len == 0 {
        return Err(HeapError::TypeMismatch);
    }
    let fields = heap.host_fields(value)?;
    let key = heap.table_entry(fields, index)?.0;
    heap.table_store(fields, index, key, field)?;
    heap.set_host_payload(value, index + 1, field)?;
    Ok(())
}
pub(in crate::aside) fn finish_character_resource(
    heap: &Heap,
    value: Value,
) -> Result<(), TypedDataError> {
    for index in 0..2 {
        let info = heap.info(heap.host_payload(value, index + 1)?)?;
        if info.kind != ObjectKind::String || index == 0 && info.len == 0 {
            return Err(TypedDataError::Contract(
                "Character requires a string name and nonempty string id",
            ));
        }
    }
    Ok(())
}
