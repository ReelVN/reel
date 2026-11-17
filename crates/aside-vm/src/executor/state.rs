//! Trusted current global reads for automatic State registration.
use super::*;
use crate::class::{ClassSlot, State};
use crate::typed_data::SchemaError;

impl Vm {
    /// Read one current class through an already verified State declaration.
    /// Module globals keep the value live until the registry roots it.
    pub fn state_class(
        &self,
        table: &crate::resume::VerifiedResumeTable,
        index: u32,
    ) -> Result<Value, SchemaError> {
        if self.program.snapshot_identity() != table.program.snapshot_identity() {
            return Err(SchemaError::Contract(
                "State declarations belong to another current program",
            ));
        }
        let state = table
            .table()
            .states
            .get(index as usize)
            .ok_or(SchemaError::Contract("unknown State declaration"))?;
        let fail = |reason| SchemaError::State {
            key: state.key.clone(),
            reason,
        };
        let binding = self
            .program
            .global_binding(state.class.module, state.class.global)
            .map_err(|_| fail("current class module binding is missing"))?;
        let cell = self
            .slots
            .read_vm(at(self.globals, binding.offset))
            .map_err(|_| fail("current class module has not been initialized"))?;
        let class = self
            .heap
            .cell_read(cell)
            .map_err(|_| fail("current class module has not completed initialization"))?;
        if self.heap.info(class)?.kind != ObjectKind::Class {
            return Err(fail("module binding does not contain its current class"));
        }
        if self
            .heap
            .get_field(class, ClassSlot::State as u32)?
            .as_int()
            != Some(State::Ready as i64)
        {
            return Err(fail("current class is not ready"));
        }
        if self
            .heap
            .get_field(class, ClassSlot::Function as u32)?
            .as_int()
            != Some(state.class.function.0 as i64)
            || self
                .heap
                .get_field(class, ClassSlot::Descriptor as u32)?
                .as_int()
                != Some(state.class.descriptor.0 as i64)
        {
            return Err(fail("module class differs from its verified declaration"));
        }
        Ok(class)
    }
}
