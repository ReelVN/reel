//! Version 1 stores the generator position in the integer `state` edge.
//! The current standard library supplies the restored stream's methods.
use super::*;

pub(super) const KIND: &str = "std.random";

pub(super) fn recognizes(heap: &Heap, value: Value) -> bool {
    aside_std::random_resource_state(heap, value).is_ok()
}

pub(super) fn header(heap: &Heap, value: Value) -> Option<LogicalReference> {
    recognizes(heap, value).then(|| LogicalReference {
        kind: KIND.into(),
        version: 1,
        fields: 1,
        payload: Vec::new(),
    })
}

pub(super) fn field(
    state: i64,
    index: u32,
) -> Result<(std::borrow::Cow<'static, str>, Value), TypedDataError> {
    if index != 0 {
        return Err(TypedDataError::Contract("invalid random stream field"));
    }
    Ok(("state".into(), Value::int(state)))
}

pub(super) fn allocate(
    heap: &mut Heap,
    reference: &LogicalReference,
) -> Result<Value, TypedDataError> {
    if reference.fields != 1 || !reference.payload.is_empty() {
        return Err(TypedDataError::Contract("invalid random stream descriptor"));
    }
    Ok(aside_std::allocate_random_resource(heap)?)
}

pub(super) fn connect(
    heap: &mut Heap,
    stream: Value,
    name: &str,
    value: Value,
) -> Result<(), TypedDataError> {
    if name != "state" {
        return Err(TypedDataError::Contract("unknown random stream field"));
    }
    let state = value.as_int().ok_or(TypedDataError::Contract(
        "random stream state must be an integer",
    ))?;
    Ok(aside_std::connect_random_resource(heap, stream, state)?)
}
