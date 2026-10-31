//! Borrow initialized immutable bindings instead of rooting a short-lived copy.
use super::*;

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
        for (index, binding) in definition.bindings.iter().enumerate() {
            initialized[index] = matches!(
                binding.initialization,
                bc::BindingInitialization::Parameter(_)
            );
            if initialized[index] {
                empty[binding.storage.reg().index()] = false;
            }
        }
        let (mut read, mut write) = (0, 0);
        while read < block.operations.len() {
            budget.work(1)?;
            budget.span = block.operations[read].span;
            if let Some((temporary, consumer)) = replacement(
                &block.operations[read..],
                definition,
                &bindings,
                &initialized,
                budget,
            )? {
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
                let span = block.operations[read + 1].span;
                block.operations[write] = Operation {
                    instruction: consumer,
                    span,
                };
                state(
                    consumer,
                    definition,
                    &bindings,
                    &mut empty,
                    &mut initialized,
                    budget,
                )?;
                write += 1;
                read += 3;
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
    }
    budget.release(empty);
    budget.release(initialized);
    budget.release(bindings);
    Ok(())
}

fn replacement(
    operations: &[Operation],
    definition: &bc::FunctionDef,
    bindings: &[Option<usize>],
    initialized: &[bool],
    budget: &mut super::super::budget::Budget,
) -> Result<Option<(bc::Reg, bc::Instruction)>, Box<Error>> {
    let Some(operations) = operations.get(..3) else {
        return Ok(None);
    };
    let read = operations[0].instruction;
    if read.opcode != bc::Opcode::BindRead as u16 {
        return Ok(None);
    }
    let temporary = bc::Reg(read.operands[0]);
    let binding = &definition.bindings[read.operands[1] as usize];
    let bc::BindingStorage::Value(source) = binding.storage else {
        return Ok(None);
    };
    if binding.mutable
        || !initialized[read.operands[1] as usize]
        || bindings[temporary.index()].is_some()
        || operations[2].instruction.opcode != bc::Opcode::Clear as u16
        || operations[2].instruction.operands[0] != temporary.0
    {
        return Ok(None);
    }
    let mut consumer = operations[1].instruction;
    let spec = bc::instruction_spec(consumer.opcode).expect("lowered opcode");
    let (mut direct_reads, mut reads, mut writes) = (0, 0, false);
    for (operand, kind) in consumer.operands.iter().zip(spec.operands) {
        if kind == bc::OperandKind::ReadReg && *operand == temporary.0 {
            direct_reads += 1;
        }
    }
    visit(definition, &consumer, budget, |reg, access| {
        if access == bc::Access::Read && reg == temporary {
            reads += 1;
        }
        if access != bc::Access::Read && (reg == temporary || reg == source) {
            writes = true;
        }
    })?;
    // Descriptor windows and hidden binding operands retain their existing path.
    if direct_reads == 0 || reads != direct_reads || writes {
        return Ok(None);
    }
    for (operand, kind) in consumer.operands.iter_mut().zip(spec.operands) {
        if kind == bc::OperandKind::ReadReg && *operand == temporary.0 {
            *operand = source.0;
        }
    }
    Ok(Some((temporary, consumer)))
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
    visit(definition, &instruction, budget, |reg, access| {
        if access != bc::Access::Read {
            empty[reg.index()] = instruction.opcode == bc::Opcode::Clear as u16;
            if let Some(binding) = bindings[reg.index()] {
                initialized[binding] = false;
            }
        }
    })?;
    if instruction.opcode == bc::Opcode::BindInit as u16 {
        initialized[instruction.operands[0] as usize] = true;
    }
    Ok(())
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
