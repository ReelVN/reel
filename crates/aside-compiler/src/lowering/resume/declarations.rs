//! Module declaration syntax becomes current-code locators in the shared ResumeTable.
use super::*;
use crate::lowering::{
    GlobalIdentity,
    project::{GlobalTarget, ProjectLinks},
};
use std::mem::size_of;

#[derive(Debug)]
pub(crate) struct SourceState {
    target: GlobalTarget,
    key: String,
    fields: Vec<(String, String)>,
    span: TextRange,
}
#[derive(Debug)]
pub(crate) struct SourceRoot {
    target: GlobalTarget,
    key: String,
    span: TextRange,
}
fn target(
    m: &h::AnalyzedModule,
    node: h::NodeId,
    module: bc::ModuleId,
    globals: &[GlobalIdentity],
    project: Option<&ProjectLinks>,
    b: &mut Budget,
) -> Result<GlobalTarget, Box<Error>> {
    b.span = m.hir().node(node).span;
    if !matches!(
        m.hir().node(node).form,
        h::Form::Name { .. }
            | h::Form::Member {
                optional: false,
                chain: false,
                ..
            }
    ) {
        return Err(b.error(
            ErrorKind::Unsupported,
            "State and Root targets require static module declarations",
        ));
    }
    if let Some(target) = project.and_then(|p| p.node(node)) {
        return Ok(target);
    }
    if let Some(symbol) = m.facts()[node.0 as usize].symbol {
        if let Some(target) = project.and_then(|p| p.symbol(symbol)) {
            return Ok(target);
        }
        if let Ok(index) = globals.binary_search_by_key(&symbol.0, |g| g.source.0) {
            return Ok(GlobalTarget {
                module,
                global: globals[index].global,
            });
        }
    }
    Err(b.error(
        ErrorKind::Unsupported,
        "State or Root target has no static module binding",
    ))
}
fn arguments(
    m: &h::AnalyzedModule,
    node: h::NodeId,
    third: Option<&str>,
    b: &mut Budget,
) -> Result<(h::NodeId, h::NodeId, Option<h::NodeId>), Box<Error>> {
    let h::Form::Call {
        arguments: Some(args),
        children: None,
        optional: false,
        chain: false,
        ..
    } = m.hir().node(node).form
    else {
        return Err(b.error(
            ErrorKind::Unsupported,
            "State and Root declarations require a direct call",
        ));
    };
    let mut slots = [None; 3];
    let mut positional = 0;
    for arg in m.hir().children(args) {
        let h::Form::Argument { name, value } = m.hir().node(arg).form else {
            return Err(b.error(
                ErrorKind::Unsupported,
                "State and Root declarations do not accept spread arguments",
            ));
        };
        let index = if let Some(name) = name {
            match m.hir().name(name.name.unwrap()) {
                "target" => 0,
                "key" => 1,
                name if Some(name) == third => 2,
                _ => {
                    return Err(b.error(
                        ErrorKind::Unsupported,
                        "unknown State or Root declaration argument",
                    ));
                }
            }
        } else {
            let index = positional;
            positional += 1;
            index
        };
        if index >= if third.is_some() { 3 } else { 2 } || slots[index].replace(value).is_some() {
            return Err(b.error(
                ErrorKind::Unsupported,
                "duplicate State or Root declaration argument",
            ));
        }
    }
    Ok((
        slots[0]
            .ok_or_else(|| b.error(ErrorKind::Unsupported, "State or Root target is required"))?,
        slots[1].ok_or_else(|| {
            b.error(
                ErrorKind::Unsupported,
                "stable State or Root key is required",
            )
        })?,
        slots[2],
    ))
}
pub(crate) fn collect(
    m: &h::AnalyzedModule,
    module: bc::ModuleId,
    globals: &[GlobalIdentity],
    project: Option<&ProjectLinks>,
    b: &mut Budget,
) -> Result<(Vec<SourceState>, Option<SourceRoot>), Box<Error>> {
    let mut states = Vec::new();
    let mut root = None;
    let mut seen = std::collections::BTreeSet::new();
    for statement in m.hir().children(h::NodeId(0)) {
        let h::Form::Statement { expression, .. } = m.hir().node(statement).form else {
            continue;
        };
        let Some(intrinsic @ (Intrinsic::ResumeState | Intrinsic::ResumeRoot)) =
            m.facts()[expression.0 as usize].intrinsic
        else {
            continue;
        };
        b.span = m.hir().node(expression).span;
        b.work(1)?;
        seen.insert(expression.0);
        let (value, key, third) = arguments(
            m,
            expression,
            if intrinsic == Intrinsic::ResumeState {
                Some("fields")
            } else {
                None
            },
            b,
        )?;
        let destination = target(m, value, module, globals, project, b)?;
        let key = literal(m, key, b)?;
        if intrinsic == Intrinsic::ResumeState {
            let mut fields = Vec::new();
            if let Some(value) = third {
                if m.hir().node(value).kind != h::HirKind::Object {
                    return Err(b.error(
                        ErrorKind::Unsupported,
                        "State fields must be a literal field-key map",
                    ));
                }
                for field in m.hir().children(value) {
                    let h::Form::Field { key, value } = m.hir().node(field).form else {
                        return Err(b.error(
                            ErrorKind::Unsupported,
                            "State field-key maps do not accept spread",
                        ));
                    };
                    let name = match key {
                        h::FieldKey::Name(token) => b.string(m.hir().name(token.name.unwrap()))?,
                        h::FieldKey::Expression(value) => literal(m, value, b)?,
                    };
                    let key = literal(m, value, b)?;
                    b.push(&mut fields, (name, key))?;
                }
            }
            b.push(
                &mut states,
                SourceState {
                    target: destination,
                    key,
                    fields,
                    span: m.hir().node(expression).span,
                },
            )?;
        } else {
            if root.is_some() {
                return Err(b.error(ErrorKind::Unsupported, "a module can declare only one Root"));
            }
            root = Some(SourceRoot {
                target: destination,
                key,
                span: m.hir().node(expression).span,
            });
        }
    }
    for (index, facts) in m.facts().iter().enumerate() {
        if matches!(
            facts.intrinsic,
            Some(Intrinsic::ResumeState | Intrinsic::ResumeRoot)
        ) && !seen.contains(&(index as u32))
        {
            b.span = m.hir().nodes[index].span;
            return Err(b.error(
                ErrorKind::Unsupported,
                "State and Root declarations must be standalone module statements",
            ));
        }
    }
    Ok((states, root))
}
pub(super) fn build(
    program: &bc::VerifiedProgram,
    modules: &[&Metadata],
    table: &mut r::ResumeTable,
    b: &mut Budget,
) -> Result<(), Box<Error>> {
    let p = program.package();
    for source in modules.iter().flat_map(|m| m.state_source.iter()) {
        b.span = source.span;
        let class = r::class_locator(program, source.target.module, source.target.global)
            .map_err(|e| b.error(ErrorKind::Unsupported, e.to_string()))?;
        let bc::Descriptor::Class(definition) =
            &p.functions[class.function.index()].descriptors[class.descriptor.index()]
        else {
            unreachable!()
        };
        b.work(definition.fields.len() as u64)?;
        let scratch =
            definition.fields.len() * (4 * (size_of::<&str>() + size_of::<bc::SymbolId>() + 64));
        b.capacity(scratch)?;
        let names = definition
            .fields
            .iter()
            .filter(|f| !f.is_static)
            .map(|f| (p.symbols[f.name.index()].as_str(), f.name))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut fields = Vec::new();
        for (name, key) in &source.fields {
            let field = *names.get(name.as_str()).ok_or_else(|| {
                b.error(
                    ErrorKind::Unsupported,
                    "State field override does not name a current field",
                )
            })?;
            let key = b.string(key)?;
            b.push(&mut fields, r::FieldKeyOverride { field, key })?;
        }
        drop(names);
        b.release_bytes(scratch);
        let key = aside_vm::typed_data::TypeKey {
            module: String::new(),
            name: b.string(&source.key)?,
        };
        b.push(&mut table.states, r::StateDecl { class, key, fields })?;
    }
    for source in modules.iter().filter_map(|m| m.root_source.as_ref()) {
        b.span = source.span;
        if table.root.is_some() {
            return Err(b.error(
                ErrorKind::Unsupported,
                "a project can declare only one Root",
            ));
        }
        let function =
            r::static_global_function(program, source.target.module, source.target.global)
                .map_err(|e| {
                    b.error(
                        ErrorKind::Unsupported,
                        format!("Root requires a static module function: {e}"),
                    )
                })?;
        let signature = &p.signatures[p.functions[function.index()].signature.index()];
        let [parameter] = signature.parameters.as_slice() else {
            return Err(b.error(
                ErrorKind::Unsupported,
                "Root requires one State parameter with a default value",
            ));
        };
        let state = table
            .states
            .iter()
            .find(|state| state.class.nominal == parameter.ty)
            .ok_or_else(|| {
                b.error(
                    ErrorKind::Unsupported,
                    "Root parameter type has no state declaration; use one direct State type",
                )
            })?;
        table.root = Some(r::RootDecl {
            key: r::RootKey(b.string(&source.key)?),
            function,
            state: state.key.clone(),
        });
    }
    Ok(())
}
