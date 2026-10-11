//! Independently recover the current standalone compiler's module-function
//! initialization prologue from verified B01, never from metadata or source.
use crate::{Budget, Error, ErrorKind, require};
use aside_vm::bytecode as bc;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn from_initializer(
    program: &bc::BytecodePackage,
    initializer: u32,
    budget: &mut Budget,
) -> Result<BTreeMap<u32, u32>, Error> {
    let init = &program.functions[initializer as usize];
    let module = &program.modules[init.module.index()];
    // Includes all PC operands and exception ranges, not just the prefix.
    budget.charge(
        init.code
            .len()
            .checked_mul(8)
            .and_then(|n| {
                init.regions
                    .len()
                    .checked_mul(8)
                    .and_then(|r| n.checked_add(r))
            })
            .ok_or_else(|| Error::new(ErrorKind::Limit, "initializer validation work overflow"))?,
    )?;
    let mut globals = BTreeMap::new();
    let mut functions = BTreeSet::new();
    let mut end = 0usize;
    while let Some(group) = init.code.get(end..end.saturating_add(3)) {
        let [make, bind, clear] = group else {
            unreachable!()
        };
        if make.opcode != bc::Opcode::MakeClosure as u16
            || bind.opcode != bc::Opcode::GlobalInit as u16
            || clear.opcode != bc::Opcode::Clear as u16
            || make.operands[0] != bind.operands[0]
            || make.operands[0] != clear.operands[0]
        {
            break;
        }
        let bc::Descriptor::Closure { function, captures } =
            &init.descriptors[make.operands[1] as usize]
        else {
            unreachable!("verified descriptor")
        };
        let bc::Descriptor::Global {
            module: owner,
            global,
        } = &init.descriptors[bind.operands[1] as usize]
        else {
            unreachable!("verified descriptor")
        };
        let output = &program.functions[function.index()];
        if *owner != init.module {
            break;
        }
        let definition = &module.globals[global.index()];
        // A root variable initialized with an anonymous closure can use the same
        // opcodes; it is not the named-function declaration prologue.
        if definition.mutable
            || definition.name != output.name
            || !captures.is_empty()
            || !output.captures.is_empty()
            || output.module != init.module
            || *function == bc::FunctionId(initializer)
        {
            break;
        }
        let log = (usize::BITS - (globals.len() + 1).leading_zeros()) as usize + 1;
        budget.charge(log * 4)?;
        require(
            globals.insert(global.0, function.0).is_none() && functions.insert(function.0),
            ErrorKind::Metadata,
            "duplicate initializer declaration binding",
        )?;
        end += 3;
    }
    for instruction in &init.code {
        let spec = bc::instruction_spec(instruction.opcode).expect("verified opcode");
        for (kind, value) in spec.operands.iter().zip(instruction.operands) {
            require(
                *kind != bc::OperandKind::Pc || value as usize >= end,
                ErrorKind::Metadata,
                "control flow enters initializer declaration prologue",
            )?;
        }
    }
    for region in &init.regions {
        require(
            region.protected.start.0 as usize >= end
                && region
                    .catch
                    .as_ref()
                    .is_none_or(|c| c.range.start.0 as usize >= end)
                && region.finally.is_none_or(|f| f.start.0 as usize >= end),
            ErrorKind::Metadata,
            "exception region enters initializer declaration prologue",
        )?;
    }
    let log = (usize::BITS - globals.len().max(1).leading_zeros()) as usize + 1;
    for (function_id, function) in program.functions.iter().enumerate() {
        budget.charge(
            function
                .code
                .len()
                .checked_mul(8 + log * 2)
                .ok_or_else(|| {
                    Error::new(ErrorKind::Limit, "declaration write scan work overflow")
                })?,
        )?;
        for (pc, instruction) in function.code.iter().enumerate() {
            if instruction.opcode != bc::Opcode::GlobalInit as u16
                && instruction.opcode != bc::Opcode::GlobalWrite as u16
            {
                continue;
            }
            let bc::Descriptor::Global {
                module: owner,
                global,
            } = &function.descriptors[instruction.operands[1] as usize]
            else {
                unreachable!("verified descriptor")
            };
            if *owner == init.module && globals.contains_key(&global.0) {
                require(
                    function_id == initializer as usize
                        && pc < end
                        && pc % 3 == 1
                        && instruction.opcode == bc::Opcode::GlobalInit as u16,
                    ErrorKind::Metadata,
                    "module declaration global is initialized/written outside its declaration prologue",
                )?;
            }
        }
    }
    Ok(globals)
}
