//! Recover named module functions from verified link-time bindings.
use crate::{Budget, Error, ErrorKind, require};
use aside_vm::bytecode as bc;
use std::collections::BTreeMap;

pub(crate) fn from_bindings(
    program: &bc::BytecodePackage<impl bc::InstructionSequence>,
    module_id: u32,
    budget: &mut Budget,
) -> Result<BTreeMap<u32, u32>, Error> {
    let module = &program.modules[module_id as usize];
    let mut declarations = BTreeMap::new();
    for (index, global) in module.globals.iter().enumerate() {
        budget.charge(1)?;
        let Some(function) = global.linked_function else {
            continue;
        };
        let definition = &program.functions[function.index()];
        if definition.kind != bc::FunctionKind::Plain {
            continue;
        }
        require(
            definition.module.0 == module_id
                && definition.name == global.name
                && definition.captures.is_empty()
                && !global.mutable,
            ErrorKind::Metadata,
            "invalid linked declaration binding",
        )?;
        declarations.insert(index as u32, function.0);
    }
    Ok(declarations)
}
