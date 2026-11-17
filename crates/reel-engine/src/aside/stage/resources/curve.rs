//! The standard scalar curve is immutable data, sampled by the current library.
use super::*;
pub(super) fn header(heap: &Heap, value: Value) -> Option<LogicalReference> {
    aside_std::curve_key_count(heap, value)
        .ok()
        .map(|_| LogicalReference {
            payload: Vec::new(),
            kind: "std.math.curve".into(),
            version: 1,
            fields: 3,
        })
}
pub(super) fn field(
    heap: &Heap,
    value: Value,
    index: u32,
) -> Result<(std::borrow::Cow<'static, str>, Value), TypedDataError> {
    let names = ["root", "first", "count"];
    let name = names
        .get(index as usize)
        .ok_or(TypedDataError::Contract("invalid curve field"))?;
    Ok((
        (*name).into(),
        aside_std::curve_resource_fields(heap, value)?[index as usize],
    ))
}
pub(super) fn connect(
    heap: &mut Heap,
    value: Value,
    name: &str,
    field: Value,
) -> Result<(), TypedDataError> {
    let index = ["root", "first", "count"]
        .iter()
        .position(|n| *n == name)
        .ok_or(TypedDataError::Contract("unknown curve field"))?;
    Ok(aside_std::connect_curve_resource(
        heap,
        value,
        index as u32,
        field,
    )?)
}
