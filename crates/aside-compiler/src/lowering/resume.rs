//! Resolved declaration markers become a separately versioned current-code sidecar.
use super::{Error, ErrorKind, FunctionMetadata, Metadata, budget::Budget};
use crate::{TextRange, hir as h, semantic::Intrinsic};
use aside_vm::{bytecode as bc, resume as r};
use bc::InstructionSequence;

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
    key: String,
    point: bool,
    span: TextRange,
}

fn marker(module: &h::AnalyzedModule, statement: h::NodeId) -> Option<(h::NodeId, Intrinsic)> {
    let h::Form::Statement { expression, .. } = module.hir().node(statement).form else {
        return None;
    };
    let intrinsic = module.facts()[expression.0 as usize].intrinsic?;
    matches!(
        intrinsic,
        Intrinsic::ResumeFlow | Intrinsic::ResumePoint | Intrinsic::ResumeReturn
    )
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
    matches!(
        module.hir().node(node).form,
        h::Form::Call {
            optional: false,
            chain: false,
            children: None,
            ..
        }
    )
    .then_some(node)
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
        let children: Vec<_> = module.hir().children(function.body).collect();
        let Some(&first) = children.first() else {
            continue;
        };
        let Some((flow_marker, Intrinsic::ResumeFlow)) = marker(module, first) else {
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
                    let call = children.get(position + 1).and_then(|&next| target(module, next)).ok_or_else(|| b.error(ErrorKind::Unsupported, "resume point/return must precede one complete call statement or call initializer"))?;
                    let site = SourceSite {
                        key: key(module, declaration, b)?,
                        point: intrinsic == Intrinsic::ResumePoint,
                        span: module.hir().node(call).span,
                    };
                    seen[declaration.0 as usize] = true;
                    b.push(&mut flow.sites, site)?;
                }
                if !matches!(
                    module.hir().node(child).kind,
                    h::HirKind::Function | h::HirKind::Closure | h::HirKind::Class
                ) {
                    stack.push(child);
                }
            }
        }
        b.push(&mut result, flow)?;
    }
    for (index, facts) in module.facts().iter().enumerate() {
        if matches!(
            facts.intrinsic,
            Some(Intrinsic::ResumeFlow | Intrinsic::ResumePoint | Intrinsic::ResumeReturn)
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
    b: &mut Budget,
) -> Result<r::ResumeTable, Box<Error>> {
    let sources: Vec<_> = modules
        .iter()
        .flat_map(|metadata| metadata.resume_source.iter())
        .collect();
    let mut table = r::ResumeTable {
        version: 1,
        flows: Vec::new(),
    };
    for source in &sources {
        b.span = source.span;
        let mut flow = r::ResumeFlow {
            key: r::FlowKey(b.string(&source.key)?),
            function: source.function,
            points: Vec::new(),
            returns: Vec::new(),
        };
        let definition = &program.package().functions[source.function.index()];
        let code = definition
            .code
            .ranged_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| b.error(ErrorKind::Bytecode, error.to_string()))?;
        for site in &source.sites {
            b.span = site.span;
            let start: u32 = site.span.start().into();
            let end: u32 = site.span.end().into();
            let calls: Vec<_> = code
                .iter()
                .enumerate()
                .filter(|(_, (range, instruction))| {
                    matches!(
                        bc::instruction_spec(instruction.opcode).unwrap().opcode,
                        bc::Opcode::Call | bc::Opcode::CallNoWait
                    ) && program
                        .source_at(source.function, bc::Pc(range.start as u32))
                        .is_some_and(|span| {
                            span.file == source.source && span.start == start && span.end == end
                        })
                })
                .collect();
            if calls.len() != 1 {
                return Err(b.error(
                    ErrorKind::Unsupported,
                    "resume statement must lower to one ordinary static call",
                ));
            }
            let (call_index, (range, _)) = calls[0];
            let call = bc::Pc(range.start as u32);
            let mut prepare_index = call_index;
            if site.point {
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
            let layout = r::analyze_site(
                program,
                source.function,
                site.point.then_some(prepare),
                call,
                limits,
            )
            .map_err(|error| b.error(ErrorKind::Unsupported, error.to_string()))?;
            b.work(layout.work_units)?;
            b.capacity(layout.capacity_bytes)?;
            b.release_bytes(layout.capacity_bytes);
            let mut bindings = Vec::new();
            for binding in layout.live {
                let key = b.string(
                    &program.package().symbols[definition.bindings[binding.index()].name.index()],
                )?;
                b.push(
                    &mut bindings,
                    r::ResumeBinding {
                        key: r::BindingKey(key),
                        binding,
                    },
                )?;
            }
            let current = r::ResumeLayout {
                bindings,
                cells: layout.cells,
            };
            if site.point {
                let point = r::ResumePoint {
                    key: r::PointKey(b.string(&site.key)?),
                    prepare,
                    call,
                    after_call: layout.after_call,
                    result: layout.result,
                    layout: current,
                };
                b.push(&mut flow.points, point)?;
            } else {
                let target = r::static_call_target(program, source.function, call)
                    .map_err(|error| b.error(ErrorKind::Unsupported, error.to_string()))?;
                let callee = sources
                    .iter()
                    .find(|flow| flow.function == target)
                    .ok_or_else(|| {
                        b.error(
                            ErrorKind::Unsupported,
                            "return point must call another declared flow",
                        )
                    })?;
                let ret = r::ResumeReturn {
                    key: r::ReturnKey(b.string(&site.key)?),
                    callee: r::FlowKey(b.string(&callee.key)?),
                    call,
                    after_call: layout.after_call,
                    result: layout.result,
                    layout: current,
                };
                b.push(&mut flow.returns, ret)?;
            }
        }
        b.push(&mut table.flows, flow)?;
    }
    if !table.flows.is_empty() {
        program
            .verify_resume(table.clone(), r::ResumeLimits::default())
            .map_err(|error| {
                if let (Some(function), Some(pc)) = (error.function, error.pc) {
                    if let Some(span) = program.source_at(function, pc) {
                        b.span = TextRange::new(span.start.into(), span.end.into());
                    }
                }
                b.error(ErrorKind::Unsupported, error.to_string())
            })?;
    }
    Ok(table)
}
