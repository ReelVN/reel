//! Static callable proofs use current verified instructions and immutable globals.
use super::*;
use crate::bytecode::{self as bc, InstructionSequence};

pub(crate) fn static_target(
    program: &bc::VerifiedProgram,
    f: &bc::RuntimeFunction,
    target: bc::CallTarget,
    pc: Pc,
    entry: Option<usize>,
    host_types: &[crate::HostTypeSupport],
    b: &mut Budget,
) -> Result<FunctionId, ResumeError> {
    match target {
        bc::CallTarget::Function(function) => Ok(function),
        bc::CallTarget::Native(native) => native_target(program, native, b),
        bc::CallTarget::Register(reg) => {
            let start = match entry {
                Some(start) => start,
                None => last_entry(f, pc, b)?,
            };
            register_target(program, f, reg, start..pc.index(), true, host_types, b)
        }
    }
}

fn register_target(
    program: &bc::VerifiedProgram,
    f: &bc::RuntimeFunction,
    mut reg: Reg,
    block: std::ops::Range<usize>,
    allow_global: bool,
    host_types: &[crate::HostTypeSupport],
    b: &mut Budget,
) -> Result<FunctionId, ResumeError> {
    let mut end = block.end;
    while let Some(start) = f.code.previous_pc(end) {
        b.work(1)?;
        if start < block.start {
            break;
        }
        end = start;
        let instruction = f
            .code
            .instruction(start)
            .map_err(|e| ResumeError::new(e.to_string()))?
            .unwrap();
        let spec = bc::instruction_spec(instruction.opcode).unwrap();
        if spec.flow != bc::Flow::Next {
            break;
        }
        let op = spec.opcode;
        if op == bc::Opcode::CheckType
            && instruction.operands[0] == reg.0
            && let Some(function) =
                host_callable_target(program, TypeId(instruction.operands[1]), host_types, b)?
        {
            return Ok(function);
        }
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
            return global_target(program, module, global, host_types, b);
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
        if op == bc::Opcode::MemberGet && instruction.operands[0] == reg.0 {
            let receiver = receiver_type(f, Reg(instruction.operands[1]), Pc(start as u32), b)?;
            return method_target(
                program,
                receiver,
                SymbolId(instruction.operands[2]),
                host_types,
                b,
            );
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

fn last_entry(f: &bc::RuntimeFunction, pc: Pc, b: &mut Budget) -> Result<usize, ResumeError> {
    let mut entry = 0;
    for instruction in f.code.iter() {
        b.work(1)?;
        let instruction = instruction.map_err(|e| ResumeError::new(e.to_string()))?;
        let spec = bc::instruction_spec(instruction.opcode).unwrap();
        let targets = match spec.flow {
            bc::Flow::Jump | bc::Flow::Leave => &instruction.operands[..1],
            bc::Flow::Branch => &instruction.operands[1..3],
            _ => &[],
        };
        for &target in targets {
            let target = target as usize;
            if target <= pc.index() {
                entry = entry.max(target);
            }
        }
    }
    Ok(entry)
}

pub(super) fn global_target(
    program: &bc::VerifiedProgram,
    mut module: bc::ModuleId,
    mut global: bc::GlobalId,
    host_types: &[crate::HostTypeSupport],
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
            let entry = last_entry(f, Pc(range.start as u32), b)?;
            producer = Some(register_target(
                program,
                f,
                Reg(instruction.operands[0]),
                entry..range.start,
                false,
                host_types,
                b,
            )?);
        }
    }
    producer.ok_or_else(|| ResumeError::new("resume global has no proved closure initializer"))
}

fn receiver_type(
    f: &bc::RuntimeFunction,
    mut reg: Reg,
    mut pc: Pc,
    b: &mut Budget,
) -> Result<TypeId, ResumeError> {
    'resolve: loop {
        let mut end = pc.index();
        while let Some(start) = f.code.previous_pc(end) {
            b.work(1)?;
            end = start;
            let instruction = f
                .code
                .instruction(start)
                .map_err(|e| ResumeError::new(e.to_string()))?
                .unwrap();
            let spec = bc::instruction_spec(instruction.opcode).unwrap();
            if spec.opcode == bc::Opcode::CheckType && instruction.operands[0] == reg.0 {
                return Ok(TypeId(instruction.operands[1]));
            }
            let mut writes = false;
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
                            bc::Access::Write
                                | bc::Access::ReadWrite
                                | bc::Access::ConditionalWrite
                        )
                    {
                        writes = true;
                    }
                },
            )
            .map_err(|e| ResumeError::new(e.to_string()))?;
            if !writes {
                continue;
            }
            match spec.opcode {
                bc::Opcode::BindRead => return Ok(f.bindings[instruction.operands[1] as usize].ty),
                bc::Opcode::Move => {
                    reg = Reg(instruction.operands[1]);
                    pc = Pc(start as u32);
                    continue 'resolve;
                }
                bc::Opcode::BindInit => return Ok(f.bindings[instruction.operands[0] as usize].ty),
                _ => {
                    return Err(ResumeError::new(
                        "resume method receiver requires a typed business binding",
                    ));
                }
            }
        }
        return f
            .bindings
            .iter()
            .find(|binding| {
                binding.storage.reg() == reg
                    && matches!(
                        binding.initialization,
                        bc::BindingInitialization::Parameter(_)
                    )
            })
            .map(|binding| binding.ty)
            .ok_or_else(|| {
                ResumeError::new("resume method receiver requires a typed business binding")
            });
    }
}

fn host_callable_target(
    program: &bc::VerifiedProgram,
    ty: TypeId,
    host_types: &[crate::HostTypeSupport],
    b: &mut Budget,
) -> Result<Option<FunctionId>, ResumeError> {
    host_nominal_target(program, ty, host_types, false, b)
}
pub(super) fn await_target(
    program: &bc::VerifiedProgram,
    f: &bc::RuntimeFunction,
    reg: Reg,
    pc: Pc,
    host_types: &[crate::HostTypeSupport],
    b: &mut Budget,
) -> Result<FunctionId, ResumeError> {
    let ty = receiver_type(f, reg, pc, b)?;
    host_nominal_target(program, ty, host_types, true, b)?.ok_or_else(|| {
        ResumeError::new(
            "await point requires a nominal value with an independently registered await adapter",
        )
    })
}
fn host_nominal_target(
    program: &bc::VerifiedProgram,
    mut ty: TypeId,
    host_types: &[crate::HostTypeSupport],
    awaiting: bool,
    b: &mut Budget,
) -> Result<Option<FunctionId>, ResumeError> {
    let p = program.package();
    if let bc::TypeDef::Union(types) = &p.types[ty.index()] {
        b.work(types.len() as u64)?;
        let mut values = types
            .iter()
            .copied()
            .filter(|id| p.types[id.index()] != bc::TypeDef::Builtin(bc::BuiltinType::Null));
        let Some(current) = values.next().filter(|_| values.next().is_none()) else {
            return Ok(None);
        };
        ty = current;
    }
    let bc::TypeDef::Named { module, name, .. } = &p.types[ty.index()] else {
        return Ok(None);
    };
    let module = &p.symbols[p.modules[module.index()].path.index()];
    let name = &p.symbols[name.index()];
    for host in host_types {
        b.work((host.name.len() + module.len() + name.len() + 2) as u64)?;
        if host.name.rsplit_once("::") != Some((module.as_str(), name.as_str())) {
            continue;
        }
        let Some(call) = (if awaiting {
            host.await_adapter.as_ref()
        } else {
            host.call.as_ref()
        }) else {
            return Ok(None);
        };
        for (index, native) in p.natives.iter().enumerate() {
            b.work(1)?;
            if p.symbols[native.symbol.index()] == call.native
                && native.abi_version == call.abi_version
            {
                return native_target(program, bc::NativeId(index as u32), b).map(Some);
            }
        }
        return Err(ResumeError::new("registered callable native is missing"));
    }
    Ok(None)
}

fn method_target(
    program: &bc::VerifiedProgram,
    receiver: TypeId,
    member: SymbolId,
    host_types: &[crate::HostTypeSupport],
    b: &mut Budget,
) -> Result<FunctionId, ResumeError> {
    let p = program.package();
    let receiver = if let bc::TypeDef::Union(types) = &p.types[receiver.index()] {
        b.work(types.len() as u64)?;
        let mut values = types
            .iter()
            .copied()
            .filter(|id| p.types[id.index()] != bc::TypeDef::Builtin(bc::BuiltinType::Null));
        values
            .next()
            .filter(|_| values.next().is_none())
            .ok_or_else(|| ResumeError::new("resume method receiver requires one nominal type"))?
    } else {
        receiver
    };
    let bc::TypeDef::Named { module, name, .. } = &p.types[receiver.index()] else {
        return Err(ResumeError::new(
            "cannot prove the receiver type at this checkpoint. Annotate the receiver with its nominal type",
        ));
    };
    if let Some(function) = declared_method(program, *module, *name, member, b)? {
        return Ok(function);
    }
    let module = &p.symbols[p.modules[module.index()].path.index()];
    let name = &p.symbols[name.index()];
    let member = &p.symbols[member.index()];
    let mut target = None;
    for host in host_types {
        b.work((host.name.len() + module.len() + name.len() + 2) as u64)?;
        if host
            .name
            .strip_prefix(module)
            .is_none_or(|tail| tail.strip_prefix("::") != Some(name.as_str()))
        {
            continue;
        }
        for method in &host.methods {
            b.work((method.name.len() + member.len() + 1) as u64)?;
            if method.name != *member {
                continue;
            }
            if target.is_some() {
                return Err(ResumeError::new("ambiguous registered resume method"));
            }
            for (index, native) in p.natives.iter().enumerate() {
                b.work(1)?;
                if p.symbols[native.symbol.index()] == method.native
                    && native.abi_version == method.abi_version
                {
                    target = Some(bc::NativeId(index as u32));
                    break;
                }
            }
        }
    }
    let native = target.ok_or_else(|| {
        ResumeError::new("resume method has no proved class or registered native target")
    })?;
    native_target(program, native, b)
}

fn declared_method(
    program: &bc::VerifiedProgram,
    module: bc::ModuleId,
    name: SymbolId,
    member: SymbolId,
    b: &mut Budget,
) -> Result<Option<FunctionId>, ResumeError> {
    let p = program.package();
    let globals = &p.modules[module.index()].globals;
    b.work(globals.len() as u64)?;
    let Some(index) = globals.iter().position(|binding| binding.name == name) else {
        return Ok(None);
    };
    let class = super::declarations::definition(program, module, bc::GlobalId(index as u32), b)?;
    let definition = &p.functions[class.locator.function.index()];
    let bc::Descriptor::Class(descriptor) =
        &definition.descriptors[class.locator.descriptor.index()]
    else {
        unreachable!()
    };
    b.work(descriptor.methods.len() as u64)?;
    let Some(method) = descriptor
        .methods
        .iter()
        .find(|method| method.name == member && !method.is_static)
    else {
        return Ok(None);
    };
    if !method.captures.is_empty() {
        return Err(ResumeError::new(
            "resumable methods require ordinary lexical bindings",
        ));
    }
    let function = &p.functions[method.function.index()];
    for instruction in function.code.ranged_iter() {
        b.work(1)?;
        let (_, instruction) = instruction.map_err(|e| ResumeError::new(e.to_string()))?;
        if instruction.opcode == bc::Opcode::SuperRef as u16 {
            return Err(ResumeError::new(
                "resumable methods cannot depend on a super binding",
            ));
        }
    }
    Ok(Some(method.function))
}
fn native_target(
    program: &bc::VerifiedProgram,
    native: bc::NativeId,
    b: &mut Budget,
) -> Result<FunctionId, ResumeError> {
    b.work(1)?;
    program
        .native_function(native)
        .ok_or_else(|| ResumeError::new("resume native function"))
}
