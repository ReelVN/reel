use super::{Compilation, Error, ErrorKind, ir::*};
use aside_vm::bytecode as bc;
use std::mem::size_of;

pub(super) fn emit(mut ir: IrModule) -> Result<Compilation, Error> {
    let b = &mut ir.budget;
    for f in ir.functions.drain(..) {
        b.span = f.span;
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
        b.reserve(&mut output.source_map, count as usize)?;
        // Iterate layout by reference, then reclaim operations as each block is consumed.
        let mut blocks = f.blocks;
        for &id in &f.layout {
            let block = &mut blocks[id.0 as usize];
            for op in block.operations.drain(..) {
                b.span = op.span;
                b.work(1)?;
                output.source_map.push(bc::SourceMapEntry {
                    pc: bc::Pc(output.code.len() as u32),
                    span: source(ir.source, op.span),
                });
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
                Terminator::Rethrow => bc::Instruction::new(bc::Opcode::Rethrow, 0, 0, 0),
                Terminator::EndFinally(region) => {
                    bc::Instruction::new(bc::Opcode::EndFinally, region.0, 0, 0)
                }
            };
            b.span = span;
            b.work(1)?;
            output.source_map.push(bc::SourceMapEntry {
                pc: bc::Pc(output.code.len() as u32),
                span: source(ir.source, span),
            });
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
    b.release(ir.functions);
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

fn target(pcs: &[Option<u32>], id: BlockId, b: &super::budget::Budget) -> Result<u32, Error> {
    pcs.get(id.0 as usize)
        .copied()
        .flatten()
        .ok_or_else(|| b.error(ErrorKind::InvalidIr, "edge to an unlaid block"))
}
fn range(
    boundaries: &[u32],
    r: std::ops::Range<u32>,
    b: &super::budget::Budget,
) -> Result<bc::PcRange, Error> {
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
