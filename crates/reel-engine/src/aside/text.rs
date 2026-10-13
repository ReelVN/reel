use aside_vm::{Heap, ObjectKind, Value};

use super::AdapterError;

/// Copy an immutable VM string without exceeding Heap's per-call input limit.
/// Decode only after the complete byte sequence has arrived: a read boundary
/// may bisect a UTF-8 scalar. `read_limit` comes from the same VmLimits as load.
pub(super) fn read_string(
    heap: &Heap,
    value: Value,
    read_limit: usize,
) -> Result<String, AdapterError> {
    let info = heap.info(value)?;
    if info.kind != ObjectKind::String {
        return Err(AdapterError::Input("expected immutable string"));
    }
    let len = info.len as usize;
    if read_limit == 0 {
        return Err(AdapterError::Input("string read chunk must be nonzero"));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(len)
        .map_err(|_| AdapterError::Allocation)?;
    bytes.resize(len, 0);
    let mut offset = 0;
    while offset < len {
        let end = len.min(offset.saturating_add(read_limit));
        let count = heap.read_bytes(value, offset as u32, &mut bytes[offset..end])?;
        if count == 0 || count > end - offset {
            return Err(AdapterError::Input("short string read"));
        }
        offset += count;
    }
    String::from_utf8(bytes).map_err(|_| AdapterError::Input("invalid UTF-8 string"))
}
