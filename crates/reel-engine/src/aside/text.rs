use aside_vm::{Heap, ObjectKind, Value};
use std::sync::Arc;

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

/// Reuse projected immutable text when the VM bytes still match it. Comparing
/// borrowed segments avoids a temporary string during unrelated node updates.
pub(super) fn read_shared_string(
    heap: &Heap,
    value: Value,
    read_limit: usize,
    previous: &Arc<str>,
) -> Result<Arc<str>, AdapterError> {
    let info = heap.info(value)?;
    if info.kind != ObjectKind::String {
        return Err(AdapterError::Input("expected immutable string"));
    }
    if read_limit == 0 {
        return Err(AdapterError::Input("string read chunk must be nonzero"));
    }
    if info.len as usize == previous.len() {
        let mut offset = 0;
        while offset < previous.len() {
            let bytes = heap.byte_segment(value, offset as u32)?;
            let count = bytes.len().min(read_limit);
            if count == 0 {
                return Err(AdapterError::Input("short string read"));
            }
            if bytes[..count] != previous.as_bytes()[offset..offset + count] {
                break;
            }
            offset += count;
        }
        if offset == previous.len() {
            return Ok(previous.clone());
        }
    }
    read_string(heap, value, read_limit).map(Arc::from)
}

/// Construct an immutable VM string through its existing chunked byte buffer.
pub(super) fn alloc_string(
    heap: &mut Heap,
    text: &str,
    chunk: usize,
) -> Result<Value, AdapterError> {
    let value = heap.alloc_byte_buffer(&[])?;
    for bytes in text.as_bytes().chunks(chunk) {
        heap.bytes_append(value, bytes)?;
    }
    heap.seal_string(value)?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aside_vm::{HeapLimits, SessionEpoch};

    #[test]
    fn shared_projection_reuses_equal_utf8_across_small_read_boundaries() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let previous: Arc<str> = Arc::from("明朝体, Noto Sans, 👋");
        let same = heap.alloc_string(&previous).unwrap();
        for limit in [1, 2, 3, 7, 4096] {
            let projected = read_shared_string(&heap, same, limit, &previous).unwrap();
            assert!(Arc::ptr_eq(&previous, &projected));
        }
        let changed = heap.alloc_string("明朝体, Noto Sans, 🌙").unwrap();
        let projected = read_shared_string(&heap, changed, 1, &previous).unwrap();
        assert_eq!(&*projected, "明朝体, Noto Sans, 🌙");
        assert!(!Arc::ptr_eq(&previous, &projected));
        let empty = heap.alloc_string("").unwrap();
        assert!(
            read_shared_string(&heap, empty, 1, &previous)
                .unwrap()
                .is_empty()
        );
    }
}
