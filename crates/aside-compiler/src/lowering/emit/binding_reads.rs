//! Borrow initialized frame-local values instead of rooting short-lived copies.
use super::*;
mod initialization;

pub(super) fn eliminate(
    function: &mut Function,
    definition: &bc::FunctionDef,
    budget: &mut super::super::budget::Budget,
) -> Result<(), Box<Error>> {
    let mut empty = budget.filled(definition.register_count as usize, false)?;
    let mut initialized = budget.filled(definition.bindings.len(), false)?;
    let mut bindings = budget.filled(definition.register_count as usize, None)?;
    for (index, binding) in definition.bindings.iter().enumerate() {
        budget.work(1)?;
        bindings[binding.storage.reg().index()] = Some(index);
    }
    let entries = initialization::entries(function, definition, &bindings, budget)?;
    // A backedge to entry must not inherit fresh-frame initialization facts.
    let entry = function.layout.first().copied();
    let mut fresh = definition.kind == bc::FunctionKind::Plain;
    for block in &function.blocks {
        budget.work(1)?;
        fresh &= !match block.terminator.as_ref().map(|(term, _)| term) {
            Some(Terminator::Jump(target) | Terminator::Leave(target)) => Some(*target) == entry,
            Some(Terminator::BranchBool { yes, no, .. }) => {
                Some(*yes) == entry || Some(*no) == entry
            }
            Some(Terminator::BranchNull {
                null, value_block, ..
            }) => Some(*null) == entry || Some(*value_block) == entry,
            _ => false,
        };
    }
    for (id, block) in function.blocks.iter_mut().enumerate() {
        budget.work((empty.len() + initialized.len()) as u64)?;
        empty.fill(fresh && entry == Some(BlockId(id as u32)));
        initialized.copy_from_slice(
            &entries[id * definition.bindings.len()..(id + 1) * definition.bindings.len()],
        );
        for (index, binding) in definition.bindings.iter().enumerate() {
            if initialized[index] || matches!(binding.storage, bc::BindingStorage::Cell(_)) {
                empty[binding.storage.reg().index()] = false;
            }
        }
        let mut removed = budget.filled(block.operations.len(), false)?;
        let (mut read, mut write) = (0, 0);
        while read < block.operations.len() {
            budget.work(1)?;
            budget.span = block.operations[read].span;
            if removed[read] {
                read += 1;
                continue;
            }
            let instruction = block.operations[read].instruction;
            // A new plain activation owns empty value registers. Cells are
            // created during preparation; parameters and other block entries
            // retain their existing roots. Later writes update this fact below.
            if instruction.opcode == bc::Opcode::Clear as u16
                && empty[instruction.operands[0] as usize]
            {
                read += 1;
                continue;
            }
            if let Some(replacement) = replacement(
                &block.operations[read..],
                definition,
                &bindings,
                &initialized,
                budget,
            )? {
                let Replacement {
                    temporary,
                    consumer,
                    clear,
                    instruction: consumer_instruction,
                } = replacement;
                block.operations[read + consumer].instruction = consumer_instruction;
                removed[read + clear] = true;
                if !empty[temporary.index()] {
                    // The old BindRead overwrote this root before its consumer.
                    // Retain that release point even when its contents are unknown.
                    let clear = bc::Instruction::new(bc::Opcode::Clear, temporary.0, 0, 0);
                    block.operations[write] = Operation {
                        instruction: clear,
                        span: block.operations[read].span,
                    };
                    state(
                        clear,
                        definition,
                        &bindings,
                        &mut empty,
                        &mut initialized,
                        budget,
                    )?;
                    write += 1;
                }
                read += 1;
            } else {
                let operation = &block.operations[read];
                state(
                    operation.instruction,
                    definition,
                    &bindings,
                    &mut empty,
                    &mut initialized,
                    budget,
                )?;
                block.operations[write] = Operation {
                    instruction: operation.instruction,
                    span: operation.span,
                };
                write += 1;
                read += 1;
            }
        }
        block.operations.truncate(write);
        budget.release(removed);
    }
    budget.release(empty);
    budget.release(initialized);
    budget.release(bindings);
    budget.release(entries);
    Ok(())
}

struct Replacement {
    temporary: bc::Reg,
    consumer: usize,
    clear: usize,
    instruction: bc::Instruction,
}

fn replacement(
    operations: &[Operation],
    definition: &bc::FunctionDef,
    bindings: &[Option<usize>],
    initialized: &[bool],
    budget: &mut super::super::budget::Budget,
) -> Result<Option<Replacement>, Box<Error>> {
    let read = operations[0].instruction;
    if read.opcode != bc::Opcode::BindRead as u16 {
        return Ok(None);
    }
    let temporary = bc::Reg(read.operands[0]);
    let binding = &definition.bindings[read.operands[1] as usize];
    let bc::BindingStorage::Value(source) = binding.storage else {
        return Ok(None);
    };
    if !initialized[read.operands[1] as usize] || bindings[temporary.index()].is_some() {
        return Ok(None);
    }
    let mut consumer = None;
    for (index, operation) in operations.iter().enumerate().skip(1) {
        budget.work(1)?;
        let instruction = operation.instruction;
        if instruction.opcode == bc::Opcode::Clear as u16 && instruction.operands[0] == temporary.0
        {
            return Ok(consumer.map(|(consumer, instruction)| Replacement {
                temporary,
                consumer,
                clear: index,
                instruction,
            }));
        }
        let spec = bc::instruction_spec(instruction.opcode).expect("lowered opcode");
        let direct_reads = instruction
            .operands
            .iter()
            .zip(spec.operands)
            .filter(|(operand, kind)| *kind == bc::OperandKind::ReadReg && **operand == temporary.0)
            .count();
        let (mut reads, mut writes) = (0, false);
        visit(definition, &instruction, budget, |reg, access| {
            if access == bc::Access::Read && reg == temporary {
                reads += 1;
            }
            if access != bc::Access::Read && (reg == temporary || reg == source) {
                writes = true;
            }
        })?;
        // A mutable Value slot belongs to this activation, unlike a shared
        // Cell. Its value can be borrowed until an instruction writes the slot.
        // Descriptor windows keep their original argument capture and roots.
        if reads != direct_reads || writes || (reads != 0 && consumer.is_some()) {
            return Ok(None);
        }
        if direct_reads != 0 {
            let mut replacement = instruction;
            for (operand, kind) in replacement.operands.iter_mut().zip(spec.operands) {
                if kind == bc::OperandKind::ReadReg && *operand == temporary.0 {
                    *operand = source.0;
                }
            }
            consumer = Some((index, replacement));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests;

fn state(
    instruction: bc::Instruction,
    definition: &bc::FunctionDef,
    bindings: &[Option<usize>],
    empty: &mut [bool],
    initialized: &mut [bool],
    budget: &mut super::super::budget::Budget,
) -> Result<(), Box<Error>> {
    writes(
        definition,
        &instruction,
        bindings,
        budget,
        |reg, binding| {
            empty[reg.index()] = instruction.opcode == bc::Opcode::Clear as u16;
            if let Some(binding) = binding {
                initialized[binding] = false;
            }
        },
    )?;
    if let Some(binding) = initialized_by(instruction) {
        initialized[binding] = true;
    }
    Ok(())
}

fn initialized_by(instruction: bc::Instruction) -> Option<usize> {
    match instruction.opcode {
        opcode
            if opcode == bc::Opcode::BindInit as u16 || opcode == bc::Opcode::BindWrite as u16 =>
        {
            Some(instruction.operands[0] as usize)
        }
        opcode if opcode == bc::Opcode::BindRead as u16 => Some(instruction.operands[1] as usize),
        _ => None,
    }
}

fn writes(
    definition: &bc::FunctionDef,
    instruction: &bc::Instruction,
    bindings: &[Option<usize>],
    budget: &mut super::super::budget::Budget,
    mut writer: impl FnMut(bc::Reg, Option<usize>),
) -> Result<(), Box<Error>> {
    visit(definition, instruction, budget, |reg, access| {
        if access != bc::Access::Read {
            writer(reg, bindings[reg.index()]);
        }
    })
}

fn visit(
    definition: &bc::FunctionDef,
    instruction: &bc::Instruction,
    budget: &mut super::super::budget::Budget,
    visitor: impl FnMut(bc::Reg, bc::Access),
) -> Result<(), Box<Error>> {
    bc::instruction_spec(instruction.opcode)
        .expect("lowered opcode")
        .visit_registers_budgeted(
            definition,
            instruction,
            |units| {
                budget.work(units).map_err(|error| bc::Error {
                    kind: bc::ErrorKind::Limit,
                    offset: None,
                    function: None,
                    pc: None,
                    source: None,
                    detail: error.detail,
                })
            },
            visitor,
        )
        .map_err(|error| {
            budget.error(
                if error.kind == bc::ErrorKind::Limit {
                    ErrorKind::Limit
                } else {
                    ErrorKind::InvalidIr
                },
                error.to_string(),
            )
        })
}
