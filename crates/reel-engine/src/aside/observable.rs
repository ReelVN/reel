//! Shared read-only observable records and collections for author services.
use super::AdapterError;
use aside_vm::{Heap, PropertyId, Value};

pub(super) fn record(
    heap: &mut Heap,
    fields: &[(&str, Value)],
    payload: &[Value],
) -> Result<Value, AdapterError> {
    let table = heap.alloc_table()?;
    for (i, &(name, value)) in fields.iter().enumerate() {
        let key = heap.alloc_string(name)?;
        heap.table_store(table, i as u32, key, value)?;
    }
    let value = heap.alloc_host(table, payload, None)?;
    heap.observe_host_properties(value, 0, fields.len() as u32)?;
    Ok(value)
}
pub(super) fn array(heap: &mut Heap) -> Result<Value, AdapterError> {
    let value = heap.alloc_array(&[])?;
    heap.observe_readonly_collection(value)?;
    Ok(value)
}
pub(super) fn publish(
    heap: &mut Heap,
    host: Value,
    field: u32,
    value: Value,
) -> Result<(), AdapterError> {
    let property = PropertyId { host, field };
    if !heap.host_property(property)?.same_bits(value) {
        heap.publish_host_property(property, value)?;
    }
    Ok(())
}
