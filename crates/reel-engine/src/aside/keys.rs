//! Shared identity for author list keys and selection values.
use aside_vm::{Heap, HeapError, ObjectKind, Value, ValueTag};
use std::cmp::Ordering;

/// Scalars retain their type, strings compare by content and other objects by
/// identity. Opaque engine IDs and author-owned records need no surrogate key.
pub(super) fn compare(
    heap: &Heap,
    a: Value,
    b: Value,
    chunk: usize,
) -> Result<Ordering, HeapError> {
    if a.same_bits(b) {
        return Ok(Ordering::Equal);
    }
    let order = (a.tag() as u32).cmp(&(b.tag() as u32));
    if order != Ordering::Equal {
        return Ok(order);
    }
    match a.tag() {
        ValueTag::Null => return Ok(Ordering::Equal),
        ValueTag::Bool => return Ok(a.as_bool().cmp(&b.as_bool())),
        ValueTag::Int => return Ok(a.as_int().cmp(&b.as_int())),
        ValueTag::Float => {
            let (a, b) = (a.as_float().unwrap(), b.as_float().unwrap());
            return Ok(if a == b {
                Ordering::Equal
            } else {
                a.total_cmp(&b)
            });
        }
        ValueTag::Object => {
            let a_text = heap.info(a)?.kind == ObjectKind::String;
            let b_text = heap.info(b)?.kind == ObjectKind::String;
            if a_text != b_text {
                return Ok(a_text.cmp(&b_text));
            }
            if !a_text {
                return Ok(heap.handle(a)?.key.cmp(&heap.handle(b)?.key));
            }
        }
    }
    let (a_len, b_len) = (heap.info(a)?.len, heap.info(b)?.len);
    let mut offset = 0;
    let (mut left, mut right) = ([0; 256], [0; 256]);
    while offset < a_len.min(b_len) {
        let count = ((a_len.min(b_len) - offset) as usize)
            .min(chunk)
            .min(left.len());
        heap.read_bytes(a, offset, &mut left[..count])?;
        heap.read_bytes(b, offset, &mut right[..count])?;
        match left[..count].cmp(&right[..count]) {
            Ordering::Equal => offset += count as u32,
            order => return Ok(order),
        }
    }
    Ok(a_len.cmp(&b_len))
}
