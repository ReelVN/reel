//! Shared immutable values used directly by native parameter defaults.
use aside_vm::bytecode as bc;

pub(crate) fn intern(package: &mut bc::BytecodePackage, value: &bc::Const) -> bc::ConstId {
    if let Some(index) = package
        .constants
        .iter()
        .position(|constant| constant == value)
    {
        bc::ConstId(index as u32)
    } else {
        let index = bc::ConstId(package.constants.len() as u32);
        package.constants.push(value.clone());
        index
    }
}
