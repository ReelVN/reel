//! Initialization-aware language cells share the storage/GC Cell object kind.
use crate::bytecode::{BuiltinType, ModuleId, TypeId};
use crate::payload::Payload;
use crate::{Heap, HeapError, ObjectKind, Value, ValueTag};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellContract {
    pub mutable: bool,
    pub ty: BuiltinType,
    pub type_id: TypeId,
    pub module: ModuleId,
    pub code_version: u64,
    pub module_version: u64,
}
impl CellContract {
    pub const fn untyped_mutable() -> Self {
        Self {
            mutable: true,
            ty: BuiltinType::Any,
            type_id: TypeId(0),
            module: ModuleId(0),
            code_version: 0,
            module_version: 0,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct CellInfo {
    pub initialized: bool,
    pub contract: CellContract,
}
pub(crate) struct CellState {
    pub value: Option<Value>,
    pub contract: CellContract,
}

impl Heap {
    pub(crate) fn accepts_builtin(&self, ty: BuiltinType, value: Value) -> Result<bool, HeapError> {
        use BuiltinType::*;
        let tag = value.tag();
        Ok(match ty {
            Any => {
                self.root_value_check(value)?;
                true
            }
            Null => tag == ValueTag::Null,
            Bool => tag == ValueTag::Bool,
            Int => tag == ValueTag::Int,
            Float => tag == ValueTag::Float,
            _ if tag != ValueTag::Object => false,
            _ => {
                let kind = self.info(value)?.kind;
                matches!(
                    (ty, kind),
                    (String, ObjectKind::String)
                        | (Bytes, ObjectKind::Bytes)
                        | (Array, ObjectKind::Array)
                        | (Table, ObjectKind::Table)
                        | (Tuple, ObjectKind::Tuple)
                        | (Class, ObjectKind::Class)
                        | (Instance, ObjectKind::Instance)
                        | (Closure, ObjectKind::Closure)
                        | (Closure, ObjectKind::BoundBuiltin)
                        | (Generator, ObjectKind::Generator)
                        | (Thread, ObjectKind::Thread)
                        | (Future, ObjectKind::Future)
                        | (WeakRef, ObjectKind::WeakRef)
                )
            }
        })
    }
    fn root_value_check(&self, value: Value) -> Result<(), HeapError> {
        if value.tag() == ValueTag::Object {
            self.info(value)?;
        }
        Ok(())
    }
    pub fn alloc_language_cell(&mut self, contract: CellContract) -> Result<Value, HeapError> {
        if self.objects.full() {
            return Err(HeapError::ObjectCapacity);
        }
        Ok(self.allocate(
            ObjectKind::Cell,
            Payload::Cell(CellState {
                value: None,
                contract,
            }),
            None,
            0,
        ))
    }
    pub fn cell_info(&self, cell: Value) -> Result<CellInfo, HeapError> {
        let Payload::Cell(state) = &self.object(self.key(cell)?)?.payload else {
            return Err(HeapError::WrongKind);
        };
        Ok(CellInfo {
            initialized: state.value.is_some(),
            contract: state.contract,
        })
    }
    pub fn cell_read(&self, cell: Value) -> Result<Value, HeapError> {
        self.cell_info(cell)?;
        self.get_field(cell, 0)
    }
    fn cell_publish(
        &mut self,
        cell: Value,
        value: Value,
        initialize: bool,
    ) -> Result<(), HeapError> {
        let key = self.key(cell)?;
        let Payload::Cell(state) = &self.object(key)?.payload else {
            return Err(HeapError::WrongKind);
        };
        if initialize && state.value.is_some() {
            return Err(HeapError::AlreadyInitialized);
        }
        if !initialize {
            if state.value.is_none() {
                return Err(HeapError::UninitializedCell);
            }
            if !state.contract.mutable {
                return Err(HeapError::ReadOnly);
            }
        }
        if !self.accepts_builtin(state.contract.ty, value)? {
            return Err(HeapError::TypeMismatch);
        }
        self.root_insertion_barrier(value)?;
        let obj = self.objects.get_mut(key).unwrap();
        let Payload::Cell(state) = &mut obj.payload else {
            unreachable!()
        };
        state.value = Some(value);
        obj.content_version = obj.content_version.wrapping_add(1);
        Ok(())
    }
    pub fn cell_initialize(&mut self, cell: Value, value: Value) -> Result<(), HeapError> {
        self.cell_publish(cell, value, true)
    }
    pub fn cell_write(&mut self, cell: Value, value: Value) -> Result<(), HeapError> {
        self.cell_publish(cell, value, false)
    }
}
