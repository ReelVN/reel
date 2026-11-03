//! Must-initialization facts at block entries. Exceptional entry and cleanup
//! continuations start with prepared parameters; normal edges intersect facts.
use super::*;

pub(super) fn entries(
    function: &Function,
    definition: &bc::FunctionDef,
    bindings: &[Option<usize>],
    budget: &mut super::super::super::budget::Budget,
) -> Result<Vec<bool>, Box<Error>> {
    let width = definition.bindings.len();
    let len = function
        .blocks
        .len()
        .checked_mul(width)
        .ok_or_else(|| budget.error(ErrorKind::Limit, "binding flow capacity overflow"))?;
    let mut entries = budget.filled(len, true)?;
    let mut generated = budget.filled(len, false)?;
    let mut preserved = budget.filled(len, true)?;
    let mut roots = budget.filled(function.blocks.len(), false)?;
    let mut edges = Vec::new();
    if let Some(entry) = function.layout.first() {
        roots[entry.0 as usize] = true;
    }
    for region in &function.regions {
        budget.work(1)?;
        for entry in [
            region.catch.as_ref().map(|(range, _)| range.start),
            region.finally.as_ref().map(|range| range.start),
        ]
        .into_iter()
        .flatten()
        {
            roots[function.layout[entry as usize].0 as usize] = true;
        }
    }
    for (id, block) in function.blocks.iter().enumerate() {
        budget.work(1)?;
        let base = id * width;
        for operation in &block.operations {
            budget.span = operation.span;
            writes(
                definition,
                &operation.instruction,
                bindings,
                budget,
                |_, binding| {
                    if let Some(binding) = binding {
                        generated[base + binding] = false;
                        preserved[base + binding] = false;
                    }
                },
            )?;
            if let Some(binding) = initialized_by(operation.instruction) {
                generated[base + binding] = true;
            }
        }
        let mut edge = |target: BlockId| budget.push(&mut edges, (id, target.0 as usize));
        match block.terminator.as_ref().map(|(term, _)| term) {
            Some(Terminator::Jump(target)) => edge(*target)?,
            Some(Terminator::BranchBool { yes, no, .. }) => {
                edge(*yes)?;
                edge(*no)?;
            }
            Some(Terminator::BranchNull {
                null, value_block, ..
            }) => {
                edge(*null)?;
                edge(*value_block)?;
            }
            Some(Terminator::Leave(target)) => {
                // Leave may execute finally before reaching its target. That
                // target cannot inherit facts from before the cleanup body.
                roots[target.0 as usize] = true;
            }
            _ => {}
        }
    }
    for (id, root) in roots.iter().copied().enumerate() {
        if root {
            budget.work(width as u64)?;
            for (binding, definition) in definition.bindings.iter().enumerate() {
                entries[id * width + binding] = matches!(
                    definition.initialization,
                    bc::BindingInitialization::Parameter(_)
                );
            }
        }
    }
    // Start at the lattice top so initialized locals remain known around loop
    // backedges. Each update removes facts, and every edge visit is budgeted.
    let mut changed = true;
    while changed {
        changed = false;
        budget.work(1)?;
        for &(from, to) in &edges {
            budget.work(width as u64)?;
            for binding in 0..width {
                let source = from * width + binding;
                let destination = to * width + binding;
                let initialized = generated[source] || preserved[source] && entries[source];
                if entries[destination] && !initialized {
                    entries[destination] = false;
                    changed = true;
                }
            }
        }
    }
    budget.release(generated);
    budget.release(preserved);
    budget.release(roots);
    budget.release(edges);
    Ok(entries)
}
