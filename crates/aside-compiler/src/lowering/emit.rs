use super::{Compilation, Error, ErrorKind, ir::*};
use aside_vm::bytecode as bc;
use std::mem::size_of;

fn source_run(
    output: &mut bc::FunctionDef,
    span: bc::SourceSpan,
    b: &mut super::budget::Budget,
) -> Result<(), Box<Error>> {
    if output
        .source_map
        .last()
        .is_none_or(|entry| entry.span != span)
    {
        b.reserve(&mut output.source_map, 1)?;
        output.source_map.push(bc::SourceMapEntry {
            pc: bc::Pc(output.code.len() as u32),
            span,
        });
    }
    Ok(())
}

pub(super) fn write(ir: &mut IrModule) -> Result<(), Box<Error>> {
    let b = &mut ir.budget;
    for mut f in ir.functions.drain(..) {
        b.span = f.span;
        let definition = &ir.package.functions[f.output.index()];
        direct_destinations(&mut f, definition, b)?;
        let mut pcs = b.filled(f.blocks.len(), None)?;
        let mut boundaries = Vec::new();
        b.reserve(&mut boundaries, f.layout.len() + 1)?;
        let mut count = 0u32;
        b.work(f.layout.len() as u64)?;
        for &block in &f.layout {
            let entry = pcs
                .get_mut(block.0 as usize)
                .filter(|pc| pc.is_none())
                .ok_or_else(|| b.error(ErrorKind::InvalidIr, "invalid/duplicate block layout"))?;
            *entry = Some(count);
            boundaries.push(count);
            let block = &f.blocks[block.0 as usize];
            if block.terminator.is_none() {
                return Err(b.error(ErrorKind::InvalidIr, "unterminated block"));
            }
            count = count
                .checked_add(block.operations.len() as u32 + 1)
                .ok_or_else(|| b.error(ErrorKind::Limit, "PC overflow"))?;
        }
        boundaries.push(count);
        let output = &mut ir.package.functions[f.output.index()];
        b.reserve(&mut output.code, count as usize)?;
        // Iterate layout by reference, then reclaim operations as each block is consumed.
        let mut blocks = f.blocks;
        for &id in &f.layout {
            let block = &mut blocks[id.0 as usize];
            for op in block.operations.drain(..) {
                b.span = op.span;
                b.work(1)?;
                source_run(output, source(ir.source, op.span), b)?;
                output.code.push(op.instruction);
            }
            let (term, span) = block.terminator.take().expect("checked terminator");
            let instruction = match term {
                Terminator::Jump(id) => {
                    bc::Instruction::new(bc::Opcode::Jump, target(&pcs, id, b)?, 0, 0)
                }
                Terminator::Leave(id) => {
                    bc::Instruction::new(bc::Opcode::Leave, target(&pcs, id, b)?, 0, 0)
                }
                Terminator::BranchBool { value, yes, no } => bc::Instruction::new(
                    bc::Opcode::BranchBool,
                    value.0,
                    target(&pcs, yes, b)?,
                    target(&pcs, no, b)?,
                ),
                Terminator::BranchNull {
                    value,
                    null,
                    value_block,
                } => bc::Instruction::new(
                    bc::Opcode::BranchNull,
                    value.0,
                    target(&pcs, null, b)?,
                    target(&pcs, value_block, b)?,
                ),
                Terminator::Return(value) => {
                    bc::Instruction::new(bc::Opcode::Return, value.0, 0, 0)
                }
                Terminator::Throw(value) => bc::Instruction::new(bc::Opcode::Throw, value.0, 0, 0),
                Terminator::MatchFail => bc::Instruction::new(bc::Opcode::MatchFail, 0, 0, 0),
                Terminator::Rethrow => bc::Instruction::new(bc::Opcode::Rethrow, 0, 0, 0),
                Terminator::EndFinally(region) => {
                    bc::Instruction::new(bc::Opcode::EndFinally, region.0, 0, 0)
                }
            };
            b.span = span;
            b.work(1)?;
            source_run(output, source(ir.source, span), b)?;
            output.code.push(instruction);
            let ops = std::mem::take(&mut block.operations);
            b.release(ops);
        }
        let regions_capacity = f.regions.capacity() * size_of::<Region>();
        for region in f.regions {
            let region = bc::ExceptionRegion {
                protected: range(&boundaries, region.protected, b)?,
                catch: region
                    .catch
                    .map(|(r, exception)| {
                        range(&boundaries, r, b).map(|range| bc::CatchRegion { range, exception })
                    })
                    .transpose()?,
                finally: region
                    .finally
                    .map(|r| range(&boundaries, r, b))
                    .transpose()?,
            };
            b.push(&mut output.regions, region)?;
        }
        b.release(pcs);
        b.release(boundaries);
        b.release(blocks);
        b.release(f.layout);
        b.release_bytes(regions_capacity);
    }
    b.release(std::mem::take(&mut ir.functions));
    Ok(())
}

/// Lowering creates private temporary registers before copying values into
/// argument/collection windows. Write directly into the destination when the
/// source temporary is already empty, so removing its Clear cannot extend a
/// GC root's lifetime. Stay inside one block, before PC and source-map emission.
fn direct_destinations(
    function: &mut Function,
    definition: &bc::FunctionDef,
    b: &mut super::budget::Budget,
) -> Result<(), Box<Error>> {
    let mut empty = b.filled(definition.register_count as usize, false)?;
    for block in &mut function.blocks {
        b.work(empty.len() as u64)?;
        empty.fill(false);
        let mut read = 0;
        let mut write = 0;
        while read < block.operations.len() {
            b.work(1)?;
            let mut instruction = block.operations[read].instruction;
            let span = block.operations[read].span;
            b.span = span;
            let mut consumed = 1;
            if matches!(
                bc::instruction_spec(instruction.opcode)
                    .expect("lowered opcode")
                    .opcode,
                bc::Opcode::LoadConst
                    | bc::Opcode::BindRead
                    | bc::Opcode::CaptureRead
                    | bc::Opcode::GlobalRead
            ) && empty[instruction.operands[0] as usize]
                && let Some(pair) = block.operations.get(read + 1..read + 3)
            {
                let copy = pair[0].instruction;
                let clear = pair[1].instruction;
                let source = instruction.operands[0];
                if copy.opcode == bc::Opcode::Move as u16
                    && clear.opcode == bc::Opcode::Clear as u16
                    && copy.operands[1] == source
                    && clear.operands[0] == source
                    && copy.operands[0] != source
                {
                    instruction.operands[0] = copy.operands[0];
                    consumed = 3;
                }
            }
            let spec = bc::instruction_spec(instruction.opcode).expect("lowered opcode");
            spec.visit_registers_budgeted(
                definition,
                &instruction,
                |units| {
                    b.work(units).map_err(|error| bc::Error {
                        kind: bc::ErrorKind::Limit,
                        offset: None,
                        function: None,
                        pc: None,
                        source: None,
                        detail: error.detail,
                    })
                },
                |register, access| {
                    if matches!(
                        access,
                        bc::Access::Write | bc::Access::ReadWrite | bc::Access::ConditionalWrite
                    ) {
                        empty[register.index()] = instruction.opcode == bc::Opcode::Clear as u16;
                    }
                },
            )
            .map_err(|error| {
                b.error(
                    if error.kind == bc::ErrorKind::Limit {
                        ErrorKind::Limit
                    } else {
                        ErrorKind::InvalidIr
                    },
                    error.to_string(),
                )
            })?;
            block.operations[write] = Operation { instruction, span };
            write += 1;
            read += consumed;
        }
        block.operations.truncate(write);
    }
    b.release(empty);
    Ok(())
}

pub(super) fn emit(mut ir: IrModule) -> Result<Compilation, Box<Error>> {
    write(&mut ir)?;
    super::constants::intern(&mut ir.package, &mut ir.budget)?;
    let b = &mut ir.budget;
    b.span = ir.metadata.source_span;
    let mut validation_limits = ir.options.bytecode_limits;
    validation_limits.max_allocation_bytes = validation_limits.max_allocation_bytes.min(
        b.limits
            .max_capacity_bytes
            .saturating_sub(ir.metadata.capacity_bytes),
    );
    let program = bc::verify(ir.package, validation_limits).map_err(|e| {
        let mut error = b.error(ErrorKind::Bytecode, e.to_string());
        if let Some(source) = e.source {
            error.span = crate::TextRange::new(source.start.into(), source.end.into());
        }
        error
    })?;
    let stats = program.stats();
    // The shared verifier reports the final package and derived cache capacities.
    // Replace conservative seed-clone admission with actual retained allocation;
    // metadata owns only new Vec/String storage, while descriptors retain Arcs.
    b.stats.capacity_bytes = stats.owned_capacity_bytes
        - std::mem::size_of::<bc::BytecodePackage>()
        + ir.metadata.capacity_bytes;
    b.stats.capacity_peak_bytes = b
        .stats
        .capacity_peak_bytes
        .max(b.stats.capacity_bytes - stats.derived_capacity_bytes + stats.validation_scratch_peak);
    Ok(Compilation {
        program,
        stats: b.stats,
        metadata: ir.metadata,
        entry: ir.entry,
    })
}
fn source(file: bc::SourceId, span: crate::TextRange) -> bc::SourceSpan {
    bc::SourceSpan {
        file,
        start: span.start().into(),
        end: span.end().into(),
    }
}

fn target(pcs: &[Option<u32>], id: BlockId, b: &super::budget::Budget) -> Result<u32, Box<Error>> {
    pcs.get(id.0 as usize)
        .copied()
        .flatten()
        .ok_or_else(|| b.error(ErrorKind::InvalidIr, "edge to an unlaid block"))
}
fn range(
    boundaries: &[u32],
    r: std::ops::Range<u32>,
    b: &super::budget::Budget,
) -> Result<bc::PcRange, Box<Error>> {
    match (
        boundaries.get(r.start as usize).copied(),
        boundaries.get(r.end as usize).copied(),
    ) {
        (Some(start), Some(end)) if start < end => Ok(bc::PcRange {
            start: bc::Pc(start),
            end: bc::Pc(end),
        }),
        _ => Err(b.error(ErrorKind::InvalidIr, "empty/invalid region layout range")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_destinations_require_an_empty_temporary_in_the_same_block() {
        use bc::Opcode::{Clear, LoadConst, Move};
        for (code, expected) in [
            (
                vec![
                    (Clear, 0, 0),
                    (LoadConst, 0, 1),
                    (Move, 1, 0),
                    (Clear, 0, 0),
                ],
                vec![(Clear, 0, 0), (LoadConst, 1, 1)],
            ),
            (
                vec![(LoadConst, 0, 1), (Move, 1, 0), (Clear, 0, 0)],
                vec![(LoadConst, 0, 1), (Move, 1, 0), (Clear, 0, 0)],
            ),
            (
                vec![
                    (Clear, 0, 0),
                    (LoadConst, 0, 1),
                    (Move, 0, 0),
                    (Clear, 0, 0),
                ],
                vec![
                    (Clear, 0, 0),
                    (LoadConst, 0, 1),
                    (Move, 0, 0),
                    (Clear, 0, 0),
                ],
            ),
        ] {
            let span = crate::TextRange::new(1.into(), 2.into());
            let mut definition = bc::fixtures::helper_native().functions.remove(0);
            definition.register_count = 2;
            let mut function = Function {
                output: bc::FunctionId(0),
                origin: Origin::Module,
                span,
                blocks: vec![Block {
                    operations: code
                        .into_iter()
                        .map(|(op, a, b)| Operation {
                            instruction: bc::Instruction::new(op, a, b, 0),
                            span,
                        })
                        .collect(),
                    terminator: Some((Terminator::Return(bc::Reg(1)), span)),
                }],
                layout: vec![BlockId(0)],
                regions: vec![],
            };
            // An earlier block clearing the same register must not establish
            // availability across an edge or an exception-handler entry.
            function.blocks.insert(
                0,
                Block {
                    operations: vec![Operation {
                        instruction: bc::Instruction::new(Clear, 0, 0, 0),
                        span,
                    }],
                    terminator: Some((Terminator::Jump(BlockId(1)), span)),
                },
            );
            let mut budget = super::super::budget::Budget::new(Default::default());
            direct_destinations(&mut function, &definition, &mut budget).unwrap();
            assert_eq!(
                function.blocks[1]
                    .operations
                    .iter()
                    .map(|operation| operation.instruction)
                    .collect::<Vec<_>>(),
                expected
                    .into_iter()
                    .map(|(op, a, b)| bc::Instruction::new(op, a, b, 0))
                    .collect::<Vec<_>>()
            );
            assert!(
                function.blocks[1]
                    .operations
                    .iter()
                    .all(|operation| operation.span == span)
            );
        }
    }
}
