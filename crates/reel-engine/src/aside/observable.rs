//! Shared read-only observable records and collections for author services.
use super::AdapterError;

aside_host::native_documentation! {
    pub(super) fn documentation {
        /// Create observable fields and collections from the supplied defaults.
        /// Reads participate in dependency tracking; writes notify affected consumers.
        /// Use `bind(model.field)` for a writable property reference.
        "reel::model";
        /// Create a read-only computed property. The function tracks observable reads
        /// and recomputes when dependencies change. Evaluation permits reads and allocation;
        /// mutation and waiting are rejected.
        "reel::derive";
        /// Remove a writable property's live binding, retaining and returning its current
        /// resolved value. Pass a property reference such as `bind(node.x)`; a read-only
        /// reference returned by `derive` cannot be unbound through this operation.
        "reel::unbind";
    }
}
use aside_vm::{Heap, HeapError, HeapHandle, PropertyId, Value};
use std::collections::BTreeMap;

/// Static field names are shared by live records, without keeping retired
/// records or their schemas alive. Restore starts a fresh weak cache.
#[derive(Default)]
pub(super) struct SchemaKeys(BTreeMap<&'static str, HeapHandle>);

impl SchemaKeys {
    pub fn key(&mut self, heap: &mut Heap, name: &'static str) -> Result<Value, HeapError> {
        if let Some(&handle) = self.0.get(name)
            && let Ok(value) = heap.import_handle(handle)
        {
            return Ok(value);
        }
        let value = heap.alloc_string(name)?;
        self.0.insert(name, heap.handle(value)?);
        Ok(value)
    }

    pub fn record(
        &mut self,
        heap: &mut Heap,
        fields: &[(&'static str, Value)],
        payload: &[Value],
    ) -> Result<Value, AdapterError> {
        record_with_keys(heap, fields, payload, |heap, name| self.key(heap, name))
    }
}

pub(super) fn record(
    heap: &mut Heap,
    fields: &[(&'static str, Value)],
    payload: &[Value],
) -> Result<Value, AdapterError> {
    record_with_keys(heap, fields, payload, |heap, name| heap.alloc_string(name))
}

fn record_with_keys(
    heap: &mut Heap,
    fields: &[(&'static str, Value)],
    payload: &[Value],
    mut key: impl FnMut(&mut Heap, &'static str) -> Result<Value, HeapError>,
) -> Result<Value, AdapterError> {
    let table = heap.alloc_table()?;
    for (i, &(name, value)) in fields.iter().enumerate() {
        let key = key(heap, name)?;
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
