use super::{AdapterError, text};
use aside_vm::{Heap, ObjectKind, Value};
use reel_core::text::Text;
const TAG: i64 = -1100;
pub(super) fn source(heap: &Heap, value: Value) -> Result<(Value, bool), AdapterError> {
    if heap.info(value)?.kind == ObjectKind::String {
        return Ok((value, false));
    }
    if heap.host_payload_len(value)? != 3 || heap.host_payload(value, 0)?.as_int() != Some(TAG) {
        return Err(AdapterError::Input("expected text or rich text"));
    }
    Ok((heap.host_payload(value, 1)?, true))
}
pub(super) fn decode(source: String, rich: bool) -> Result<Text, AdapterError> {
    if rich {
        Text::parse(source).map_err(AdapterError::Markup)
    } else {
        Ok(source.into())
    }
}
pub(super) fn read(heap: &Heap, value: Value, chunk: usize) -> Result<Text, AdapterError> {
    let (value, rich) = source(heap, value)?;
    decode(text::read_string(heap, value, chunk)?, rich)
}
pub(super) fn len(heap: &Heap, value: Value) -> Result<usize, AdapterError> {
    let (_, rich) = source(heap, value)?;
    Ok(heap
        .info(if rich {
            heap.host_payload(value, 2)?
        } else {
            value
        })?
        .len as usize)
}
pub(super) fn create(heap: &mut Heap, source: Value, chunk: usize) -> Result<Value, AdapterError> {
    let parsed = decode(text::read_string(heap, source, chunk)?, true)?;
    let plain = text::alloc_string(heap, &parsed, chunk)?;
    let fields = heap.alloc_table()?;
    let key = heap.alloc_string("plain")?;
    heap.table_store(fields, 0, key, plain)?;
    let key = heap.alloc_string("markup")?;
    heap.table_store(fields, 1, key, source)?;
    Ok(heap.alloc_immutable_host(fields, &[Value::int(TAG), source, plain])?)
}
pub(super) fn alloc(heap: &mut Heap, value: &Text, chunk: usize) -> Result<Value, AdapterError> {
    if let Some(markup) = value.markup() {
        let source = text::alloc_string(heap, markup, chunk)?;
        create(heap, source, chunk)
    } else {
        text::alloc_string(heap, value, chunk)
    }
}
