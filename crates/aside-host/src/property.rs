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
    /// Precise author type when the runtime boundary uses a broader builtin type.
    /// This metadata shares the property's declaration and never changes admission.
    pub doc_type: Option<fn() -> aside_compiler::semantic::TypeHint>,
}

/// A property's registered type/default and its declaration documentation.
pub struct PropertyDocumentation {
    pub property: &'static Property,
    pub markdown: &'static str,
}
impl Property {
    /// Read the source type, retaining the independently registered runtime validator.
    pub fn source_type(&self) -> aside_compiler::semantic::TypeHint {
        self.doc_type
            .map_or_else(|| crate::hint(&self.ty), |ty| ty())
    }
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
    /// Property names in the same stable order as `PROPERTIES`.
    const NAMES: &'static [&'static str];
    /// Return the declaration's Markdown for hover and completion documentation.
    fn documentation(index: usize) -> &'static str;
    /// Host write entry point. Override to validate relationships between fields.
    fn validate_host(heap: &Heap, _host: Value, index: u32, value: Value) -> Result<(), HeapError> {
        Self::validate(heap, index, value)
    }
    fn validate(heap: &Heap, index: u32, value: Value) -> Result<(), HeapError> {
        Self::PROPERTIES
            .get(index as usize)
            .ok_or(HeapError::OutOfBounds)?
            .validate(heap, value)
    }
}
