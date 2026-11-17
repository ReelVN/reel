//! Code-independent compiled curve edges for host logical-resource adapters.
use super::*;

/// Borrow the pure numeric key graph, first key and key count of a scalar curve.
pub fn curve_resource_fields(heap: &Heap, curve: Value) -> Result<[Value; 3], HeapError> {
    curve_key_count(heap, curve)?;
    Ok([
        heap.host_payload(curve, 1)?,
        heap.host_payload(curve, 2)?,
        heap.host_payload(curve, 3)?,
    ])
}
/// Allocate a current immutable curve identity before connecting its data edges.
pub fn allocate_curve_resource(heap: &mut Heap) -> Result<Value, HeapError> {
    let fields = heap.alloc_table()?;
    let curve = heap.alloc_immutable_host(
        fields,
        &[Value::int(TAG), Value::NULL, Value::NULL, Value::int(0)],
    )?;
    let ty = heap.host_type(crate::source::CURVE)?;
    heap.set_host_type(curve, ty)?;
    Ok(curve)
}
/// Connect a previously allocated curve's root, first-key or count edge.
pub fn connect_curve_resource(
    heap: &mut Heap,
    curve: Value,
    index: u32,
    value: Value,
) -> Result<(), HeapError> {
    if index > 2 || heap.host_payload(curve, 0)?.as_int() != Some(TAG) {
        return Err(HeapError::TypeMismatch);
    }
    if index == 2
        && !value
            .as_int()
            .is_some_and(|n| n > 0 && n <= i64::from(u32::MAX))
    {
        return Err(HeapError::TypeMismatch);
    }
    heap.set_host_payload(curve, index + 1, value)
}
fn node_at(heap: &Heap, curve: Value, index: u32) -> Result<(Value, u32, u32), HeapError> {
    let mut end = curve_key_count(heap, curve)?;
    if index >= end {
        return Err(HeapError::OutOfBounds);
    }
    let mut start = 0;
    let mut node = heap.host_payload(curve, 1)?;
    loop {
        if heap.sequence_len(node)? != 8 {
            return Err(HeapError::TypeMismatch);
        }
        let middle = start + (end - start) / 2;
        if index == middle {
            return Ok((node, start, end));
        }
        if index < middle {
            end = middle;
            node = heap.sequence_get(node, LEFT)?;
        } else {
            start = middle + 1;
            node = heap.sequence_get(node, RIGHT)?;
        }
    }
}
fn finite_key(heap: &Heap, node: Value, index: u32) -> Result<f64, HeapError> {
    heap.sequence_get(node, index)?
        .as_float()
        .filter(|v| v.is_finite())
        .ok_or(HeapError::TypeMismatch)
}
/// Validate one compiled key and its bounded balanced-tree links per call.
/// Complete validation precedes publication or sampling of restored data.
pub fn validate_curve_resource_step(
    heap: &Heap,
    curve: Value,
    cursor: &mut u32,
) -> Result<bool, HeapError> {
    let count = curve_key_count(heap, curve)?;
    if count == 0 {
        return Err(HeapError::TypeMismatch);
    }
    if *cursor == count {
        return Ok(true);
    }
    let index = *cursor;
    let (node, start, end) = node_at(heap, curve, index)?;
    for slot in [0, 1] {
        finite_key(heap, node, slot)?;
    }
    // Coefficients retain float64 arithmetic, including construction overflow.
    for slot in [3, 4, 5] {
        heap.sequence_get(node, slot)?
            .as_float()
            .ok_or(HeapError::TypeMismatch)?;
    }
    if !matches!(heap.sequence_get(node, 2)?.as_int(), Some(0..=2)) {
        return Err(HeapError::TypeMismatch);
    }
    if index == 0 {
        if !node.same_bits(heap.host_payload(curve, 2)?) {
            return Err(HeapError::TypeMismatch);
        }
    } else if finite_key(heap, node_at(heap, curve, index - 1)?.0, 0)? > finite_key(heap, node, 0)?
    {
        return Err(HeapError::TypeMismatch);
    }
    for (low, high, slot) in [(start, index, LEFT), (index + 1, end, RIGHT)] {
        let expected = if low < high {
            node_at(heap, curve, low + (high - low) / 2)?.0
        } else {
            Value::NULL
        };
        if !heap.sequence_get(node, slot)?.same_bits(expected) {
            return Err(HeapError::TypeMismatch);
        }
    }
    *cursor += 1;
    Ok(false)
}
