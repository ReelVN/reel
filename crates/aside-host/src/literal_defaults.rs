//! Shared immutable values used directly by native parameter defaults.
use aside_vm::bytecode as bc;
use std::collections::HashMap;

pub(crate) fn intern<'a>(
    package: &mut bc::BytecodePackage,
    index: &mut HashMap<&'a bc::Const, bc::ConstId>,
    value: &'a bc::Const,
) -> bc::ConstId {
    *index.entry(value).or_insert_with(|| {
        let index = bc::ConstId(package.constants.len() as u32);
        package.constants.push(value.clone());
        index
    })
}
