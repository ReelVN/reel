//! Internal native compositions use the same declared stage property defaults.
use super::*;
pub(in crate::aside) fn layout(heap: &mut Heap) -> Result<Vec<Value>, HeapError> {
    use aside_vm::bytecode::Const;
    NodeProperty::PROPERTIES
        .iter()
        .map(|p| {
            Ok(match p.parameter().default.as_ref().unwrap() {
                Const::Null => Value::NULL,
                Const::Bool(v) => Value::bool(*v),
                Const::Int(v) => Value::int(*v),
                Const::FloatBits(v) => Value::float(f64::from_bits(*v)),
                Const::Utf8(v) => heap.alloc_string(v)?,
                _ => return Err(HeapError::TypeMismatch),
            })
        })
        .collect()
}
