//! Resolved declaration markers become a separately versioned current-code sidecar.
use super::{Error, ErrorKind, FunctionMetadata, Metadata, budget::Budget};
use crate::{TextRange, hir as h, semantic::Intrinsic};
use aside_vm::{bytecode as bc, resume as r};
use bc::InstructionSequence;
use std::{collections::HashMap, mem::size_of};
mod declarations;
pub(super) use declarations::collect as collect_declarations;
pub(crate) use declarations::{SourceRoot, SourceState};

#[derive(Debug)]
pub(crate) struct SourceFlow {
    key: String,
    function: bc::FunctionId,
    source: bc::SourceId,
    span: TextRange,
    sites: Vec<SourceSite>,
}
#[derive(Debug)]
struct SourceSite {
    key: Option<String>,
    span: TextRange,
}

const AUTO_PREFIX: &str = "@";

fn marker(module: &h::AnalyzedModule, statement: h::NodeId) -> Option<(h::NodeId, Intrinsic)> {
    let h::Form::Statement { expression, .. } = module.hir().node(statement).form else {
        return None;
    };
    let intrinsic = module.facts()[expression.0 as usize].intrinsic?;
    matches!(intrinsic, Intrinsic::ResumeFlow | Intrinsic::ResumePoint)
        .then_some((expression, intrinsic))
}
fn key(module: &h::AnalyzedModule, call: h::NodeId, b: &mut Budget) -> Result<String, Box<Error>> {
    b.span = module.hir().node(call).span;
    let h::Form::Call {
        arguments: Some(arguments),
        children: None,
        ..
    } = module.hir().node(call).form
    else {
        return Err(b.error(
            ErrorKind::Unsupported,
            "resume declaration requires one literal string key",
        ));
    };
    let args: Vec<_> = module.hir().children(arguments).collect();
    if args.len() != 1 {
        return Err(b.error(
            ErrorKind::Unsupported,
            "resume declaration requires exactly one literal key",
        ));
    }
    let h::Form::Argument { name: None, value } = module.hir().node(args[0]).form else {
        return Err(b.error(ErrorKind::Unsupported, "resume key must be positional"));
    };
    literal(module, value, b)
}
fn literal(
    module: &h::AnalyzedModule,
    value: h::NodeId,
    b: &mut Budget,
) -> Result<String, Box<Error>> {
    let Some(literal) = crate::strings::StringLiteral::new(module.hir(), value) else {
        return Err(b.error(
            ErrorKind::Unsupported,
            "resume key must be a literal string",
        ));
    };
    let mut bytes = Vec::new();
    for part in literal.parts() {
        let crate::strings::StringPart::Text(text) = part else {
            return Err(b.error(
                ErrorKind::Unsupported,
                "resume key cannot contain interpolation",
            ));
        };
        b.reserve(&mut bytes, text.text.len())?;
        b.work(text.text.len() as u64)?;
        text.append_to(&mut bytes)
            .map_err(|_| b.error(ErrorKind::InvalidIr, "resume key escape"))?;
    }
    if bytes.is_empty() {
        return Err(b.error(ErrorKind::Unsupported, "resume key cannot be empty"));
    }
    Ok(String::from_utf8(bytes).expect("decoded UTF-8"))
}
fn target(module: &h::AnalyzedModule, statement: h::NodeId) -> Option<h::NodeId> {
    let node = match module.hir().node(statement).form {
        h::Form::Statement { expression, .. } => expression,
        h::Form::Binding {
            initializer: Some(value),
            ..
        } => value,
        _ => return None,
    };
    (matches!(
        module.hir().node(node).form,
        h::Form::Call {
            optional: false,
            chain: false,
            children: None,
            ..
        }
    ) || matches!(module.hir().node(node).form,h::Form::Unary{operator,..} if operator.kind==crate::syntax::SyntaxKind::Await))
    .then_some(node)
}
pub(super) fn call_nodes(
    module: &h::AnalyzedModule,
    b: &mut Budget,
) -> Result<Vec<bool>, Box<Error>> {
    let mut calls = b.filled(module.hir().nodes.len(), false)?;
    for function in module.functions() {
        if flow_marker(module, function).is_none() {
            continue;
        }
        let mut stack = vec![function.body];
        while let Some(node) = stack.pop() {
            b.work(1)?;
            if let Some(call) = target(module, node) {
                calls[call.0 as usize] = true;
            }
            stack.extend(flow_children(module, node));
        }
    }
    Ok(calls)
}

fn flow_marker(module: &h::AnalyzedModule, function: &h::Function) -> Option<h::NodeId> {
    let (marker, intrinsic) = marker(module, module.hir().children(function.body).next()?)?;
    (intrinsic == Intrinsic::ResumeFlow).then_some(marker)
}

fn flow_children(module: &h::AnalyzedModule, node: h::NodeId) -> impl Iterator<Item = h::NodeId> {
    module.hir().children(node).filter(|&child| {
        !matches!(
            module.hir().node(child).kind,
            h::HirKind::Function | h::HirKind::Closure | h::HirKind::Class
        )
    })
}
/// Inspect resolved intrinsic identities; ordinary functions with the same spelling stay ordinary.
pub(crate) fn collect(
    module: &h::AnalyzedModule,
    functions: &[FunctionMetadata],
    source: bc::SourceId,
    b: &mut Budget,
) -> Result<Vec<SourceFlow>, Box<Error>> {
    let mut result = Vec::new();
    let mut seen = vec![false; module.hir().nodes.len()];
    for (index, function) in module.functions().iter().enumerate() {
        let Some(flow_marker) = flow_marker(module, function) else {
            continue;
        };
        let Some(output) = functions[index].output else {
            continue;
        };
        let mut flow = SourceFlow {
            key: key(module, flow_marker, b)?,
            function: output,
            source,
            span: module.hir().node(function.node).span,
            sites: Vec::new(),
        };
        seen[flow_marker.0 as usize] = true;
        let mut stack = vec![function.body];
        while let Some(block) = stack.pop() {
            b.work(1)?;
            let children: Vec<_> = module.hir().children(block).collect();
            let mut marked = None;
            for (position, &child) in children.iter().enumerate() {
                if let Some((declaration, intrinsic)) = marker(module, child) {
                    if declaration == flow_marker {
                        continue;
                    }
                    b.span = module.hir().node(declaration).span;
                    if intrinsic == Intrinsic::ResumeFlow {
                        return Err(b.error(
                            ErrorKind::Unsupported,
                            "flow must be the first statement of its function",
                        ));
                    }
                    let call = children.get(position + 1).and_then(|&next| target(module, next)).ok_or_else(|| b.error(ErrorKind::Unsupported, "point must precede one complete call or await statement, or binding initializer"))?;
                    let key = key(module, declaration, b)?;
                    if key.starts_with(AUTO_PREFIX) {
                        return Err(b.error(
                            ErrorKind::Unsupported,
                            "point key uses the reserved @ prefix",
                        ));
                    }
                    let site = SourceSite {
                        key: Some(key),
                        span: module.hir().node(call).span,
                    };
                    marked = Some(call);
                    seen[declaration.0 as usize] = true;
                    b.push(&mut flow.sites, site)?;
                } else if let Some(call) = target(module, child)
                    && marked != Some(call)
                {
                    b.push(
                        &mut flow.sites,
                        SourceSite {
                            key: None,
                            span: module.hir().node(call).span,
                        },
                    )?;
                }
            }
            stack.extend(flow_children(module, block));
        }
        b.work(
            flow.sites.len() as u64
                * (usize::BITS - flow.sites.len().max(1).leading_zeros()) as u64,
        )?;
        flow.sites.sort_unstable_by_key(|site| site.span.start());
        b.push(&mut result, flow)?;
    }
    for (index, facts) in module.facts().iter().enumerate() {
        if matches!(
            facts.intrinsic,
            Some(Intrinsic::ResumeFlow | Intrinsic::ResumePoint)
        ) && !seen[index]
        {
            b.span = module.hir().nodes[index].span;
            return Err(b.error(ErrorKind::Unsupported, "resume declaration is outside its declared ordinary flow or is not a standalone statement"));
        }
    }
    Ok(result)
}

pub(crate) fn build(
    program: &bc::VerifiedProgram,
    modules: &[&Metadata],
    host_types: &[bc::HostTypeSupport],
    b: &mut Budget,
) -> Result<r::ResumeTable, Box<Error>> {
    let mut sources: Vec<_> = modules
        .iter()
        .flat_map(|metadata| metadata.resume_source.iter())
        .collect();
    sources.sort_unstable_by_key(|flow| flow.function.index());
    let flow_lookup_work = u64::from(sources.len().max(1).ilog2()) + 1;
    let mut table = r::ResumeTable::default();
    declarations::build(program, modules, &mut table, b)?;
    for source in &sources {
        b.span = source.span;
        b.source = Some(source.source);
        let mut keys = Vec::new();
        b.reserve(&mut keys, source.sites.len())?;
        keys.extend(source.sites.iter().filter(|site| site.key.is_some()));
        b.work(keys.len() as u64 * (usize::BITS - keys.len().max(1).leading_zeros()) as u64)?;
        keys.sort_unstable_by(|a, b| a.key.cmp(&b.key));
        if let Some(pair) = keys.windows(2).find(|pair| pair[0].key == pair[1].key) {
            b.span = pair[1].span;
            return Err(b.error(
                ErrorKind::Unsupported,
                "point keys must be unique within a flow",
            ));
        }
        b.release(keys);
        let mut flow = r::ResumeFlow {
            key: r::FlowKey(b.string(&source.key)?),
            function: source.function,
            layouts: Vec::new(),
            points: Vec::new(),
            returns: Vec::new(),
        };
        if source.sites.is_empty() {
            b.push(&mut table.flows, flow)?;
            continue;
        }
        let analysis = r::FlowAnalysis::new(
            program,
            source.function,
            r::ResumeLimits {
                work: b.limits.max_work.saturating_sub(b.stats.work_units),
                allocation_bytes: b
                    .limits
                    .max_capacity_bytes
                    .saturating_sub(b.stats.capacity_bytes),
            },
        )
        .map_err(|error| b.error(ErrorKind::Unsupported, error.to_string()))?;
        b.work(analysis.work_units)?;
        b.capacity(analysis.capacity_bytes)?;
        let definition = &program.package().functions[source.function.index()];
        let code = definition
            .code
            .ranged_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| b.error(ErrorKind::Bytecode, error.to_string()))?;
        let mut calls = Vec::new();
        for (index, (range, instruction)) in code.iter().enumerate() {
            b.work(1)?;
            if matches!(
                bc::instruction_spec(instruction.opcode).unwrap().opcode,
                bc::Opcode::Call | bc::Opcode::CallNoWait | bc::Opcode::Await
            ) && let Some(span) = program.source_at(source.function, bc::Pc(range.start as u32))
                && span.file == source.source
            {
                b.push(&mut calls, ((span.start, span.end), index))?;
            }
        }
        let lookup_work = u64::from(calls.len().max(1).ilog2()) + 1;
        b.work(calls.len() as u64 * lookup_work)?;
        calls.sort_unstable_by_key(|&(span, _)| span);
        let mut layout_ids = HashMap::new();
        let mut layout_cache_bytes = 0;
        let mut shape = blake3::Hasher::new();
        let mut automatic = Vec::new();
        for site in &source.sites {
            b.span = site.span;
            let start: u32 = site.span.start().into();
            let end: u32 = site.span.end().into();
            let span = (start, end);
            b.work(lookup_work)?;
            let found = calls.partition_point(|&(candidate, _)| candidate < span);
            if calls
                .get(found)
                .is_none_or(|&(candidate, _)| candidate != span)
                || calls
                    .get(found + 1)
                    .is_some_and(|&(candidate, _)| candidate == span)
            {
                if site.key.is_none() {
                    continue;
                }
                return Err(b.error(
                    ErrorKind::Unsupported,
                    "resume statement must lower to one ordinary call or await",
                ));
            }
            let call_index = calls[found].1;
            let call = bc::Pc(code[call_index].0.start as u32);
            let (target, target_work) = analysis.target_candidate(
                call,
                host_types,
                r::ResumeLimits {
                    work: b.limits.max_work.saturating_sub(b.stats.work_units),
                    allocation_bytes: b
                        .limits
                        .max_capacity_bytes
                        .saturating_sub(b.stats.capacity_bytes),
                },
            );
            b.work(target_work)?;
            let target = match target {
                Ok(target) => target,
                Err(error) if site.key.is_some() => {
                    return Err(b.error(ErrorKind::Unsupported, error.to_string()));
                }
                Err(_) => continue,
            };
            b.work(flow_lookup_work)?;
            let callee = sources
                .binary_search_by_key(&target.index(), |flow| flow.function.index())
                .ok()
                .map(|index| sources[index]);
            let point = callee.is_none();
            let target = &program.package().functions[target.index()];
            let identity = match (callee, target.kind) {
                (Some(flow), _) => flow.key.as_str(),
                (None, bc::FunctionKind::Native(native))
                    if program.package().signatures[target.signature.index()]
                        .effects
                        .0
                        & bc::Effects::WAIT
                        != 0 =>
                {
                    let symbol = program.package().natives[native.index()].symbol;
                    program.package().symbols[symbol.index()].as_str()
                }
                _ if site.key.is_none() => continue,
                _ => {
                    return Err(b.error(ErrorKind::Unsupported,
                        "point must call a statically known native waiting operation or declared flow"));
                }
            };
            let mut prepare_index = call_index;
            if point {
                while prepare_index > 0 {
                    let span = program.source_at(
                        source.function,
                        bc::Pc(code[prepare_index - 1].0.start as u32),
                    );
                    if !span.is_some_and(|span| {
                        span.file == source.source && start <= span.start && span.end <= end
                    }) {
                        break;
                    }
                    prepare_index -= 1;
                }
            }
            let prepare = bc::Pc(code[prepare_index].0.start as u32);
            let limits = r::ResumeLimits {
                work: b.limits.max_work.saturating_sub(b.stats.work_units),
                allocation_bytes: b
                    .limits
                    .max_capacity_bytes
                    .saturating_sub(b.stats.capacity_bytes),
            };
            let (layout, work) =
                analysis.site_candidate(point.then_some(prepare), call, host_types, limits);
            b.work(work)?;
            let layout = match layout {
                Ok(layout) => layout,
                Err(error) if site.key.is_some() => {
                    return Err(b.error(ErrorKind::Unsupported, error.to_string()));
                }
                Err(_) => continue,
            };
            let key = match &site.key {
                Some(key) => b.string(key)?,
                None => {
                    let index = if point {
                        flow.points.len()
                    } else {
                        flow.returns.len()
                    };
                    b.push(&mut automatic, (point, index))?;
                    String::new()
                }
            };
            b.work(1 + identity.len() as u64 + key.len() as u64)?;
            shape.update(&[u8::from(point)]);
            for component in [identity, key.as_str()] {
                shape.update(&(component.len() as u64).to_le_bytes());
                shape.update(component.as_bytes());
            }
            let ids = [layout.live, layout.cells, layout.initialized_dead];
            b.work(1 + ids.iter().map(|ids| ids.len() as u64).sum::<u64>())?;
            let current = if let Some(&index) = layout_ids.get(&ids) {
                index
            } else {
                let mut shared = r::ResumeLayout::default();
                b.reserve(&mut shared.bindings, ids[0].len())?;
                for &binding in &ids[0] {
                    let name = definition.bindings[binding.index()].name;
                    let key = r::BindingKey(b.string(&program.package().symbols[name.index()])?);
                    b.push(&mut shared.bindings, r::ResumeBinding { key, binding })?;
                }
                b.reserve(&mut shared.cells, ids[1].len())?;
                shared.cells.extend_from_slice(&ids[1]);
                b.reserve(&mut shared.initialized_dead, ids[2].len())?;
                shared.initialized_dead.extend_from_slice(&ids[2]);
                let index = u32::try_from(flow.layouts.len())
                    .map_err(|_| b.error(ErrorKind::Limit, "resume layout index exceeds u32"))?;
                b.push(&mut flow.layouts, shared)?;
                let bytes = 4 * size_of::<([Vec<bc::BindingId>; 3], u32)>()
                    + ids
                        .iter()
                        .map(|ids| ids.capacity() * size_of::<bc::BindingId>())
                        .sum::<usize>();
                b.capacity(bytes)?;
                layout_cache_bytes += bytes;
                layout_ids.try_reserve(1).map_err(|_| {
                    b.error(
                        ErrorKind::Allocation,
                        "resume layout index allocation failed",
                    )
                })?;
                layout_ids.insert(ids, index);
                index
            };
            if point {
                let point = r::ResumePoint {
                    key: r::PointKey(key),
                    prepare,
                    call,
                    after_call: layout.after_call,
                    result: layout.result,
                    layout: current,
                };
                b.push(&mut flow.points, point)?;
            } else {
                let callee = callee.unwrap();
                let ret = r::ResumeReturn {
                    key: r::ReturnKey(key),
                    callee: callee.function,
                    call,
                    after_call: layout.after_call,
                    result: layout.result,
                    layout: current,
                };
                b.push(&mut flow.returns, ret)?;
            }
        }
        if !automatic.is_empty() {
            let mut names = shape.finalize_xof();
            for &(point, index) in &automatic {
                let mut bytes = [0; 8];
                names.fill(&mut bytes);
                b.work(bytes.len() as u64)?;
                let key = b.string(&format!("{AUTO_PREFIX}{:016x}", u64::from_le_bytes(bytes)))?;
                if point {
                    flow.points[index].key = r::PointKey(key);
                } else {
                    flow.returns[index].key = r::ReturnKey(key);
                }
            }
        }
        b.release(automatic);
        drop(layout_ids);
        b.release_bytes(layout_cache_bytes);
        b.release(calls);
        b.release_bytes(analysis.capacity_bytes);
        b.push(&mut table.flows, flow)?;
    }
    if !table.is_empty() {
        program
            .verify_resume(table.clone(), host_types, r::ResumeLimits::default())
            .map_err(|error| {
                if let (Some(function), Some(pc)) = (error.function, error.pc) {
                    if let Some(span) = program.source_at(function, pc) {
                        b.source = Some(span.file);
                        b.span = TextRange::new(span.start.into(), span.end.into());
                    }
                }
                b.error(ErrorKind::Unsupported, error.to_string())
            })?;
    }
    b.source = None;
    Ok(table)
}
