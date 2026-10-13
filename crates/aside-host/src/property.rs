use crate::NativeParameter;
use aside_vm::{
    Heap, HeapError, Value,
    bytecode::{BuiltinType, Const},
};

pub type PropertyValidator = fn(&Heap, Value) -> Result<(), HeapError>;
pub struct Property {
    pub name: &'static str,
    pub ty: BuiltinType,
    pub default: fn() -> Const,
    pub writable: bool,
    pub animatable: bool,
    pub validator: Option<PropertyValidator>,
}
impl Property {
    pub fn parameter(&self) -> NativeParameter {
        NativeParameter {
            name: self.name.into(),
            ty: self.ty,
            default: Some((self.default)()),
        }
    }
    pub fn validate(&self, heap: &Heap, value: Value) -> Result<(), HeapError> {
        if !self.writable {
            return Err(HeapError::TypeMismatch);
        }
        if let Some(validate) = self.validator {
            return validate(heap, value);
        }
        if heap.accepts_builtin(self.ty, value)? {
            Ok(())
        } else {
            Err(HeapError::TypeMismatch)
        }
    }
}
pub trait PropertySchema {
    const PROPERTIES: &'static [Property];
    fn validate(heap: &Heap, index: u32, value: Value) -> Result<(), HeapError> {
        Self::PROPERTIES
            .get(index as usize)
            .ok_or(HeapError::OutOfBounds)?
            .validate(heap, value)
    }
}
