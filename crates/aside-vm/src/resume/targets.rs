//! Static callable proofs use current verified instructions and immutable globals.
use super::*;
use crate::bytecode::{self as bc, InstructionSequence};

pub(crate) fn static_target(
    program: &bc::VerifiedProgram,
    f: &bc::RuntimeFunction,
    target: bc::CallTarget,
    pc: Option<Pc>,
    b: &mut Budget,
) -> Result<FunctionId, ResumeError> {
    match target {
        bc::CallTarget::Function(function) => Ok(function),
        bc::CallTarget::Native(native) => {
            for (i, function) in program.package().functions.iter().enumerate() {
                b.work(1)?;
                if function.kind == bc::FunctionKind::Native(native) {
                    return Ok(FunctionId(i as u32));
                }
            }
            Err(ResumeError::new("resume native function"))
        }
        bc::CallTarget::Register(reg) => register_target(
            program,
            f,
            reg,
            pc.ok_or_else(|| ResumeError::new("dynamic call in point preparation"))?,
            true,
            b,
        ),
    }
}

fn register_target(
    program: &bc::VerifiedProgram,
    f: &bc::RuntimeFunction,
    mut reg: Reg,
    pc: Pc,
    allow_global: bool,
    b: &mut Budget,
) -> Result<FunctionId, ResumeError> {
    let bytes = f
        .code
        .len()
        .checked_mul(std::mem::size_of::<(std::ops::Range<usize>, bc::Instruction)>())
        .ok_or_else(|| ResumeError::new("resume callable proof size"))?;
    b.bytes(bytes)?;
    let mut code = Vec::new();
    code.try_reserve_exact(f.code.len())
        .map_err(|_| ResumeError::new("resume callable proof allocation"))?;
    let mut last_entry = 0;
    for item in f.code.ranged_iter() {
        b.work(1)?;
        let (range, instruction) = item.map_err(|e| ResumeError::new(e.to_string()))?;
        let spec = bc::instruction_spec(instruction.opcode).unwrap();
        let targets: &[u32] = match spec.flow {
            bc::Flow::Jump | bc::Flow::Leave => &instruction.operands[..1],
            bc::Flow::Branch => &instruction.operands[1..3],
            _ => &[],
        };
        for &target in targets {
            if target as usize <= pc.index() {
                last_entry = last_entry.max(target as usize);
            }
        }
        if range.start < pc.index() {
            code.push((range, instruction));
        }
    }
    for (range, instruction) in code.into_iter().rev() {
        b.work(1)?;
        if range.start < last_entry {
            break;
        }
        let spec = bc::instruction_spec(instruction.opcode).unwrap();
        if spec.flow != bc::Flow::Next {
            break;
        }
        let op = spec.opcode;
        if op == bc::Opcode::Move && instruction.operands[0] == reg.0 {
            reg = Reg(instruction.operands[1]);
            continue;
        }
        if op == bc::Opcode::GlobalRead && instruction.operands[0] == reg.0 && allow_global {
            let bc::Descriptor::Global { module, global } =
                f.descriptors[instruction.operands[1] as usize]
            else {
                unreachable!()
            };
            return global_target(program, module, global, b);
        }
        if op == bc::Opcode::MakeClosure && instruction.operands[0] == reg.0 {
            let bc::Descriptor::Closure {
                function,
                ref captures,
            } = f.descriptors[instruction.operands[1] as usize]
            else {
                unreachable!()
            };
            if captures.is_empty() {
                return Ok(function);
            }
        }
        if op == bc::Opcode::BindRead && instruction.operands[0] == reg.0 {
            let binding = &f.bindings[instruction.operands[1] as usize];
            if !binding.mutable {
                reg = binding.storage.reg();
                continue;
            }
        }
        if op == bc::Opcode::BindInit
            && f.bindings[instruction.operands[0] as usize].storage.reg() == reg
        {
            reg = Reg(instruction.operands[1]);
            continue;
        }
        let mut written = false;
        spec.visit_registers_budgeted(
            f,
            &instruction,
            |n| {
                b.work(n)
                    .map_err(|e| bc::Error::new(bc::ErrorKind::Limit, e.detail))
            },
            |r, access| {
                if r == reg
                    && matches!(
                        access,
                        bc::Access::Write | bc::Access::ReadWrite | bc::Access::ConditionalWrite
                    )
                {
                    written = true;
                }
            },
        )
        .map_err(|e| ResumeError::new(e.to_string()))?;
        if written {
            break;
        }
    }
    Err(ResumeError::new(
        "dynamic callee has no straight-line static proof at the resume boundary",
    ))
}

fn global_target(
    program: &bc::VerifiedProgram,
    mut module: bc::ModuleId,
    mut global: bc::GlobalId,
    b: &mut Budget,
) -> Result<FunctionId, ResumeError> {
    let mut visited = std::collections::BTreeSet::new();
    let def = loop {
        b.work(1)?;
        b.bytes(std::mem::size_of::<(bc::ModuleId, bc::GlobalId)>())?;
        if !visited.insert((module, global)) {
            return Err(ResumeError::new("cyclic imported resume callee"));
        }
        let owner = &program.package().modules[module.index()];
        if let Some(def) = owner.globals.get(global.index()) {
            break def;
        }
        let import = &owner.imports[global.index() - owner.globals.len()];
        module = import.module;
        b.work(program.package().modules[module.index()].exports.len() as u64)?;
        global = program.package().modules[module.index()]
            .exports
            .iter()
            .find(|e| e.name == import.export)
            .unwrap()
            .global;
    };
    if def.mutable {
        return Err(ResumeError::new(
            "mutable global callee is not a stable resume target",
        ));
    }
    if let Some(function) = def.linked_function {
        return Ok(function);
    }
    let initializer = program.package().modules[module.index()]
        .initializer
        .ok_or_else(|| ResumeError::new("resume global has no initializer"))?;
    let mut producer = None;
    for (index, f) in program.package().functions.iter().enumerate() {
        for item in f.code.ranged_iter() {
            b.work(1)?;
            let (range, instruction) = item.map_err(|e| ResumeError::new(e.to_string()))?;
            if instruction.opcode != bc::Opcode::GlobalInit as u16 {
                continue;
            }
            let bc::Descriptor::Global {
                module: target_module,
                global: target_global,
            } = f.descriptors[instruction.operands[1] as usize]
            else {
                unreachable!()
            };
            if (target_module, target_global) != (module, global) {
                continue;
            }
            if index != initializer.index() || producer.is_some() {
                return Err(ResumeError::new(
                    "resume global requires one initializer-owned closure definition",
                ));
            }
            producer = Some(register_target(
                program,
                f,
                Reg(instruction.operands[0]),
                Pc(range.start as u32),
                false,
                b,
            )?);
        }
    }
    producer.ok_or_else(|| ResumeError::new("resume global has no proved closure initializer"))
}
