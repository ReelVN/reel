//! Current class provenance and static persistent declarations share the verified bytecode.
use super::*;
use crate::bytecode as bc;
use bc::InstructionSequence;
use std::collections::BTreeMap;
use std::mem::size_of;

#[derive(Clone, Copy)]
enum Origin {
    Global(ModuleId, GlobalId),
    Class(DescriptorId, Option<(ModuleId, GlobalId)>),
}
#[derive(Clone)]
pub(super) struct ClassDefinition {
    pub locator: ClassLocator,
    pub base: Option<(ModuleId, GlobalId)>,
}
pub(super) fn definition(
    program: &bc::VerifiedProgram,
    module: ModuleId,
    global: GlobalId,
    b: &mut Budget,
) -> Result<ClassDefinition, ResumeError> {
    let p = program.package();
    let m = p
        .modules
        .get(module.index())
        .ok_or_else(|| ResumeError::new("State class module is missing"))?;
    let binding = m
        .globals
        .get(global.index())
        .ok_or_else(|| ResumeError::new("State requires an owned module class binding"))?;
    if binding.mutable || binding.linked_function.is_some() {
        return Err(ResumeError::new(
            "State requires an immutable module class declaration",
        ));
    }
    let function = m
        .initializer
        .ok_or_else(|| ResumeError::new("State class module has no initializer"))?;
    let f = &p.functions[function.index()];
    let mut registers = BTreeMap::<u32, Origin>::new();
    let mut scratch = 0usize;
    let mut result = None;
    for item in f.code.ranged_iter() {
        b.work(1)?;
        let (range, i) = item.map_err(|e| ResumeError::new(e.to_string()))?;
        let spec = bc::instruction_spec(i.opcode).unwrap();
        let [a, d, _] = i.operands;
        let origin = match spec.opcode {
            bc::Opcode::GlobalRead => {
                let bc::Descriptor::Global { module, global } = f.descriptors[d as usize] else {
                    unreachable!()
                };
                Some(Origin::Global(module, global))
            }
            bc::Opcode::Move => registers.get(&d).copied(),
            bc::Opcode::MakeClass => {
                let bc::Descriptor::Class(desc) = &f.descriptors[d as usize] else {
                    unreachable!()
                };
                match desc.base {
                    None => Some(Origin::Class(DescriptorId(d), None)),
                    Some(reg) => match registers.get(&reg.0) {
                        Some(Origin::Global(m, g)) => {
                            Some(Origin::Class(DescriptorId(d), Some((*m, *g))))
                        }
                        _ => None,
                    },
                }
            }
            _ => None,
        };
        if spec.opcode == bc::Opcode::GlobalInit {
            let bc::Descriptor::Global {
                module: target_module,
                global: target_global,
            } = f.descriptors[d as usize]
            else {
                unreachable!()
            };
            if target_module == module && target_global == global {
                if result.is_some() {
                    return Err(ResumeError::new(
                        "State class binding has multiple initializers",
                    ));
                }
                let Some(Origin::Class(descriptor, base)) = registers.get(&a).copied() else {
                    return Err(ResumeError::new(
                        "State target is not a static named class declaration",
                    )
                    .at(function, Pc(range.start as u32)));
                };
                let bc::Descriptor::Class(desc) = &f.descriptors[descriptor.index()] else {
                    unreachable!()
                };
                let nominal = desc
                    .nominal
                    .ok_or_else(|| ResumeError::new("State class has no nominal type"))?;
                if !matches!(p.types[nominal.index()],bc::TypeDef::Named{module:m,name,..} if m==module&&name==binding.name)
                {
                    return Err(ResumeError::new(
                        "State target is not its named module class",
                    ));
                }
                result = Some(ClassDefinition {
                    locator: ClassLocator {
                        module,
                        global,
                        function,
                        descriptor,
                        nominal,
                    },
                    base,
                });
            }
        }
        for (operand, value) in spec.operands.into_iter().zip(i.operands) {
            if matches!(
                operand,
                bc::OperandKind::WriteReg | bc::OperandKind::ReadWriteReg
            ) {
                registers.remove(&value);
            }
        }
        if let Some(origin) = origin {
            let bytes = ((registers.len() + 1) / 5 + 1)
                * (128 + 11 * (size_of::<u32>() + size_of::<Origin>()));
            if bytes > scratch {
                b.bytes(bytes - scratch)?;
                scratch = bytes;
            }
            registers.insert(a, origin);
        }
        if matches!(
            spec.flow,
            bc::Flow::Jump | bc::Flow::Branch | bc::Flow::Leave
        ) {
            registers.clear();
        }
    }
    drop(registers);
    b.bytes -= scratch;
    result.ok_or_else(|| ResumeError::new("State class initializer was not found"))
}
/// Locate a static named class through its current immutable module global.
pub fn class_locator(
    program: &bc::VerifiedProgram,
    module: ModuleId,
    global: GlobalId,
) -> Result<ClassLocator, ResumeError> {
    Ok(definition(
        program,
        module,
        global,
        &mut Budget::new(ResumeLimits::default()),
    )?
    .locator)
}
