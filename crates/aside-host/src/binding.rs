use crate::NativeContract;
use aside_vm::{
    Heap, HeapError, Value, ValueTag,
    bytecode::BuiltinType,
    executor::{HostResponse, HostTicket, Vm, VmError},
};

#[derive(Debug)]
pub enum Error {
    Heap(HeapError),
    Vm(VmError),
    Allocation,
    Arguments,
}
impl From<HeapError> for Error {
    fn from(e: HeapError) -> Self {
        Self::Heap(e)
    }
}
impl From<VmError> for Error {
    fn from(e: VmError) -> Self {
        Self::Vm(e)
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

/// A borrowed native call context, never stored as a VM continuation.
pub struct CallContext<'a, H> {
    pub host: &'a mut H,
    pub vm: &'a mut Vm,
    pub ticket: HostTicket,
}

pub type NativeHandler<H, E> = fn(&mut H, &mut Vm, HostTicket) -> Result<(), E>;
pub struct NativeBinding<H = (), E = Error> {
    pub contract: NativeContract,
    pub invoke: NativeHandler<H, E>,
    /// Markdown extracted from the exported function's Rust documentation.
    /// Tooling reads this static text by canonical symbol; VM linkage stores only the contract.
    pub documentation: &'static str,
}

/// The registered signature and its source-authored Markdown, for editor tooling.
pub struct NativeDocumentation {
    pub contract: NativeContract,
    pub markdown: &'static str,
}

impl<H, E> NativeBinding<H, E> {
    pub fn into_documentation(self) -> NativeDocumentation {
        NativeDocumentation {
            contract: self.contract,
            markdown: self.documentation,
        }
    }
}

/// Owned conversions used at the native boundary. Value itself is a borrowed
/// VM identity: retain it with a root before storing it beyond this call.
pub trait FromValue: Sized {
    const TYPE: BuiltinType;
    fn from_value(heap: &Heap, value: Value) -> Result<Self, Error>;
}
pub trait IntoValue {
    const TYPE: BuiltinType;
    fn into_value(self, heap: &mut Heap) -> Result<Value, Error>;
}
impl FromValue for Value {
    const TYPE: BuiltinType = BuiltinType::Any;
    fn from_value(_: &Heap, value: Value) -> Result<Self, Error> {
        Ok(value)
    }
}
impl IntoValue for Value {
    const TYPE: BuiltinType = BuiltinType::Any;
    fn into_value(self, _: &mut Heap) -> Result<Value, Error> {
        Ok(self)
    }
}
macro_rules! scalar {
    ($ty:ty, $kind:ident, $get:ident, $make:ident) => {
        impl FromValue for $ty {
            const TYPE: BuiltinType = BuiltinType::$kind;
            fn from_value(_: &Heap, value: Value) -> Result<Self, Error> {
                value.$get().ok_or(HeapError::TypeMismatch.into())
            }
        }
        impl IntoValue for $ty {
            const TYPE: BuiltinType = BuiltinType::$kind;
            fn into_value(self, _: &mut Heap) -> Result<Value, Error> {
                Ok(Value::$make(self))
            }
        }
    };
}
scalar!(i64, Int, as_int, int);
scalar!(f64, Float, as_float, float);
scalar!(bool, Bool, as_bool, bool);
impl IntoValue for () {
    const TYPE: BuiltinType = BuiltinType::Null;
    fn into_value(self, _: &mut Heap) -> Result<Value, Error> {
        Ok(Value::NULL)
    }
}
impl<T: FromValue> FromValue for Option<T> {
    const TYPE: BuiltinType = BuiltinType::Any;
    fn from_value(heap: &Heap, value: Value) -> Result<Self, Error> {
        if value.tag() == ValueTag::Null {
            Ok(None)
        } else {
            T::from_value(heap, value).map(Some)
        }
    }
}
impl<T: IntoValue> IntoValue for Option<T> {
    const TYPE: BuiltinType = BuiltinType::Any;
    fn into_value(self, heap: &mut Heap) -> Result<Value, Error> {
        match self {
            Some(v) => v.into_value(heap),
            None => Ok(Value::NULL),
        }
    }
}
impl FromValue for String {
    const TYPE: BuiltinType = BuiltinType::String;
    fn from_value(heap: &Heap, value: Value) -> Result<Self, Error> {
        let info = heap.info(value)?;
        if info.kind != aside_vm::ObjectKind::String {
            return Err(HeapError::TypeMismatch.into());
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(info.len as usize)
            .map_err(|_| Error::Allocation)?;
        while bytes.len() < info.len as usize {
            bytes.extend_from_slice(heap.byte_segment(value, bytes.len() as u32)?);
        }
        String::from_utf8(bytes).map_err(|_| HeapError::TypeMismatch.into())
    }
}
impl IntoValue for String {
    const TYPE: BuiltinType = BuiltinType::String;
    fn into_value(self, heap: &mut Heap) -> Result<Value, Error> {
        Ok(heap.alloc_string(&self)?)
    }
}
impl IntoValue for &str {
    const TYPE: BuiltinType = BuiltinType::String;
    fn into_value(self, heap: &mut Heap) -> Result<Value, Error> {
        Ok(heap.alloc_string(self)?)
    }
}

/// Complete through the real native ticket; no script reentry occurs here.
pub fn respond<T: IntoValue>(vm: &mut Vm, ticket: HostTicket, value: T) -> Result<(), Error> {
    let value = value.into_value(vm.heap_mut())?;
    vm.respond(ticket, HostResponse::Ready(value))?;
    Ok(())
}

pub trait IntoConstant {
    fn into_constant(self) -> aside_vm::bytecode::Const;
}
macro_rules! constant {
    ($ty:ty, $variant:ident) => {
        impl IntoConstant for $ty {
            fn into_constant(self) -> aside_vm::bytecode::Const {
                aside_vm::bytecode::Const::$variant(self.into())
            }
        }
    };
}
constant!(i32, Int);
constant!(i64, Int);
constant!(bool, Bool);
constant!(&str, Utf8);
impl IntoConstant for f64 {
    fn into_constant(self) -> aside_vm::bytecode::Const {
        aside_vm::bytecode::Const::FloatBits(self.to_bits())
    }
}
pub fn constant(value: impl IntoConstant) -> aside_vm::bytecode::Const {
    value.into_constant()
}
