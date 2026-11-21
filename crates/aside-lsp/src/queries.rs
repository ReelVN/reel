//! Editor queries consume compiler scopes, facts and canonical symbol references.
use crate::{
    position::Encoding,
    workspace::{HostIndex, Snapshot, Source, Workspace, import_path_at, reference_key},
};
use aside_compiler::hir::SymbolKind;
use aside_compiler::{
    SyntaxKind as K, TextRange, TextSize, editor::EditorModule, hir::*, semantic::*,
};
use lsp_types::*;
use std::{cmp::Reverse, collections::BTreeSet};

fn contains(span: TextRange, offset: usize) -> bool {
    u32::from(span.start()) as usize <= offset && offset <= u32::from(span.end()) as usize
}
fn scope_at(module: EditorModule<'_>, offset: usize) -> Option<ScopeId> {
    module
        .scopes()
        .iter()
        .enumerate()
        .filter(|(_, scope)| contains(scope.span, offset))
        .min_by_key(|(index, scope)| (u32::from(scope.span.len()), Reverse(*index)))
        .map(|(index, _)| ScopeId(index as u32))
}
fn visible(module: EditorModule<'_>, offset: usize) -> Vec<SymbolId> {
    let mut scope = scope_at(module, offset);
    let mut names = BTreeSet::new();
    let mut result = Vec::new();
    while let Some(id) = scope {
        let current = &module.scopes()[id.0 as usize];
        for &id in current.symbols.iter().rev() {
            let symbol = &module.symbols()[id.0 as usize];
            if symbol.initialization == Initialization::Declaration
                && u32::from(symbol.declaration.start()) as usize > offset
            {
                continue;
            }
            if names.insert(symbol.name) {
                result.push(id);
            }
        }
        scope = current.parent;
    }
    result
}
fn external(
    module: EditorModule<'_>,
    node: Option<NodeId>,
    symbol: Option<SymbolId>,
) -> Option<&ExternalSymbol> {
    if let Some(value) = node.and_then(|node| module.source_types().external(node)) {
        return Some(value);
    }
    let bindings = module.external_bindings();
    node.and_then(|node| bindings.iter().find(|binding| binding.node == Some(node)))
        .or_else(|| {
            symbol.and_then(|symbol| {
                bindings
                    .iter()
                    .find(|binding| binding.symbol == Some(symbol))
            })
        })
        .map(|binding| binding.descriptor.as_ref())
}
fn named_type(module: EditorModule<'_>, ty: TypeId) -> Option<&str> {
    match &module.types()[ty.0 as usize] {
        Type::Named(name) => Some(module.hir().name(*name)),
        Type::Union(types) => {
            let mut name = None;
            for &ty in types {
                if matches!(module.types()[ty.0 as usize], Type::Null) {
                    continue;
                }
                let candidate = named_type(module, ty)?;
                if name.is_some_and(|name| name != candidate) {
                    return None;
                }
                name = Some(candidate);
            }
            name
        }
        _ => None,
    }
}
pub(crate) fn hint_label(ty: &TypeHint) -> String {
    ty.to_string()
}
fn type_label(module: EditorModule<'_>, ty: TypeId) -> String {
    module.display_type(ty).to_string()
}
fn inferred_label(module: EditorModule<'_>, ty: &TypeHint) -> String {
    if let TypeHint::Named(name) = ty
        && let Some(local) = name
            .strip_prefix(module.canonical_path())
            .and_then(|name| name.strip_prefix("::"))
    {
        return local.into();
    }
    ty.to_string()
}

fn inferred_builtin_members(ty: &TypeHint) -> Option<&'static BuiltinTypeMembers> {
    if let TypeHint::Union(types) = ty {
        let mut selected: Option<&BuiltinTypeMembers> = None;
        for ty in types.iter().filter(|ty| **ty != TypeHint::Null) {
            let group = inferred_builtin_members(ty)?;
            if selected.is_some_and(|selected| !std::ptr::eq(selected, group)) {
                return None;
            }
            selected = Some(group);
        }
        return selected;
    }
    builtin_catalog().types.iter().find(|group| {
        group.receiver == *ty
            || matches!((ty, &group.receiver), (TypeHint::StringLiteral(_), TypeHint::String))
            || matches!((ty, &group.receiver), (TypeHint::Tuple(_), TypeHint::Named(name)) if name == "tuple")
    })
}

struct Signature {
    prefix: &'static str,
    label: String,
    name: String,
    result: String,
    parameters: Vec<(String, String)>,
    literal_parameters: Vec<(usize, Vec<String>)>,
    markdown: Option<String>,
    parameter_documentation: std::collections::BTreeMap<String, std::sync::Arc<str>>,
}

fn hint_literals(ty: &TypeHint) -> Option<Vec<String>> {
    match ty {
        TypeHint::StringLiteral(text) => Some(vec![format!("{text:?}")]),
        TypeHint::Null => Some(vec!["null".into()]),
        TypeHint::Union(types) => {
            let mut values = Vec::new();
            for ty in types {
                values.extend(hint_literals(ty)?);
            }
            Some(values)
        }
        _ => None,
    }
}

fn source_literals(module: EditorModule<'_>, ty: TypeId) -> Option<Vec<String>> {
    match &module.types()[ty.0 as usize] {
        Type::StringLiteral(text) => Some(vec![format!("{text:?}")]),
        Type::Null => Some(vec!["null".into()]),
        Type::Union(types) => {
            let mut values = Vec::new();
            for &ty in types {
                values.extend(source_literals(module, ty)?);
            }
            Some(values)
        }
        _ => None,
    }
}

fn literal_parameters(
    choices: impl Iterator<Item = Option<Vec<String>>>,
) -> Vec<(usize, Vec<String>)> {
    choices
        .enumerate()
        .filter_map(|(index, values)| {
            let mut values = values?;
            if !values.iter().any(|value| value.starts_with('"')) {
                return None;
            }
            values.sort_unstable();
            values.dedup();
            Some((index, values))
        })
        .collect()
}
fn host_signature(symbol: &ExternalSymbol, host: &HostIndex) -> Option<Signature> {
    let signature = symbol.signature.as_ref()?;
    let result = symbol
        .source_type
        .as_ref()
        .and_then(|info| info.call_result.as_ref())
        .map(|info| &info.ty)
        .filter(|ty| **ty != TypeHint::Any)
        .unwrap_or(&signature.result);
    let declaration = host.declaration(symbol);
    let parameters: Vec<_> = signature
        .parameters
        .iter()
        .map(|parameter| {
            let rest = if parameter.rest { "..." } else { "" };
            let default = declaration
                .and_then(|declaration| declaration.parameter_defaults.get(&parameter.name));
            let optional = if matches!(default, Some(crate::ParameterDefault::Omitted))
                || (default.is_none() && parameter.default)
            {
                "?"
            } else {
                ""
            };
            let value = match default {
                Some(crate::ParameterDefault::Value(value)) => format!(" = {value}"),
                _ => String::new(),
            };
            (
                parameter.name.clone(),
                format!(
                    "{rest}{}{optional}: {}{value}",
                    parameter.name,
                    hint_label(&parameter.ty)
                ),
            )
        })
        .collect();
    Some(Signature {
        prefix: "fn",
        literal_parameters: literal_parameters(
            signature
                .parameters
                .iter()
                .map(|parameter| hint_literals(&parameter.ty)),
        ),
        parameter_documentation: declaration
            .map(|declaration| declaration.parameter_documentation.clone())
            .unwrap_or_default(),
        name: symbol.name.clone(),
        result: hint_label(result),
        label: format!(
            "fn {}({}) -> {}",
            symbol.name,
            parameters
                .iter()
                .map(|(_, label)| label.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            hint_label(result)
        ),
        parameters,
        markdown: declaration
            .map(|declaration| declaration.markdown.clone())
            .filter(|text| !text.is_empty()),
    })
}
fn declaration_node(source: &Source, span: TextRange) -> Option<aside_compiler::SyntaxNode> {
    source
        .parsed
        .syntax()
        .descendants()
        .filter(|node| {
            matches!(
                node.kind(),
                K::FnDecl | K::LetDecl | K::ClassDecl | K::Param
            ) && node.text_range().contains_range(span)
        })
        .min_by_key(|node| u32::from(node.text_range().len()))
}
fn source_docs(source: &Source, span: TextRange) -> Option<String> {
    aside_compiler::documentation::declaration(&declaration_node(source, span)?)
        .map(|documentation| documentation.markdown)
}
fn local_signature(module: EditorModule<'_>, source: &Source, id: SymbolId) -> Option<Signature> {
    let symbol = &module.symbols()[id.0 as usize];
    value_signature(
        module,
        source,
        symbol.ty,
        module.hir().name(symbol.name),
        symbol.declaration,
    )
}
fn value_signature(
    module: EditorModule<'_>,
    source: &Source,
    ty: TypeId,
    name: &str,
    declaration: TextRange,
) -> Option<Signature> {
    if let Type::Union(types) = &module.types()[ty.0 as usize] {
        let mut candidates = types
            .iter()
            .copied()
            .filter(|id| !matches!(module.types()[id.0 as usize], Type::Null));
        let inner = candidates.next()?;
        if candidates.next().is_none() {
            return value_signature(module, source, inner, name, declaration);
        }
    }
    if let Type::Callable(parameters, result) = &module.types()[ty.0 as usize] {
        let literal_parameters =
            literal_parameters(parameters.iter().map(|&ty| source_literals(module, ty)));
        let parameters: Vec<_> = parameters
            .iter()
            .map(|&ty| (String::new(), type_label(module, ty)))
            .collect();
        let result = type_label(module, *result);
        return Some(Signature {
            prefix: "fn",
            literal_parameters,
            parameter_documentation: Default::default(),
            label: format!(
                "fn {name}({}) -> {result}",
                parameters
                    .iter()
                    .map(|(_, label)| label.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            name: name.into(),
            result,
            parameters,
            markdown: source_docs(source, declaration),
        });
    }
    let (function, bound) = match module.types()[ty.0 as usize] {
        Type::Function(function) => (function, false),
        Type::BoundMethod(function) => (function, true),
        _ => return None,
    };
    Some(function_signature(
        module,
        source,
        function,
        name,
        declaration,
        bound,
    ))
}
fn function_signature(
    module: EditorModule<'_>,
    source: &Source,
    function: FunctionId,
    name: &str,
    declaration: TextRange,
    bound: bool,
) -> Signature {
    let annotated = matches!(
        module
            .hir()
            .node(module.functions()[function.0 as usize].node)
            .form,
        Form::Function {
            result: Some(_),
            ..
        }
    );
    let result = (!annotated)
        .then(|| module.source_types().function_result(function))
        .flatten()
        .map(|info| match &info.shape {
            Some(SourceShape::Generator { result, .. }) => result,
            _ => info,
        })
        .filter(|info| info.ty != TypeHint::Any)
        .map(|info| inferred_label(module, &info.ty));
    let function = &module.functions()[function.0 as usize];
    let prefix = match function.kind {
        FunctionKind::Generator => "gen fn",
        FunctionKind::Async => "async fn",
        FunctionKind::Plain => "fn",
    };
    let result = result.unwrap_or_else(|| type_label(module, function.return_type));
    let parameters: Vec<_> = function
        .parameters
        .iter()
        .skip(usize::from(bound))
        .map(|parameter| {
            let symbol = &module.symbols()[parameter.symbol.0 as usize];
            let name = module.hir().name(symbol.name).to_owned();
            let rest = if parameter.rest { "..." } else { "" };
            let default = parameter
                .default
                .map(|node| format!(" = {}", module.hir().text(module.hir().node(node).span)))
                .unwrap_or_default();
            (
                name.clone(),
                format!(
                    "{rest}{name}: {}{default}",
                    type_label(module, parameter.annotation.unwrap_or(symbol.ty))
                ),
            )
        })
        .collect();
    Signature {
        prefix,
        literal_parameters: literal_parameters(
            function
                .parameters
                .iter()
                .skip(usize::from(bound))
                .map(|parameter| {
                    let ty = parameter
                        .annotation
                        .unwrap_or(module.symbols()[parameter.symbol.0 as usize].ty);
                    source_literals(module, ty)
                }),
        ),
        parameter_documentation: Default::default(),
        name: name.into(),
        result: result.clone(),
        label: format!(
            "{prefix} {name}({}) -> {}",
            parameters
                .iter()
                .map(|(_, label)| label.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            result
        ),
        parameters,
        markdown: source_docs(source, declaration),
    }
}
fn source_symbol<'a>(
    snapshot: &'a Snapshot,
    reference: &SymbolReference,
) -> Option<(EditorModule<'a>, &'a Source, SymbolId)> {
    let module = snapshot.project.module(&reference.module)?;
    let source = snapshot.sources.get(&reference.module)?;
    let id = *module.exports().iter().find(|&&id| {
        Some(module.hir().name(module.symbols()[id.0 as usize].name)) == reference.export.as_deref()
    })?;
    Some((module, source, id))
}
fn source_class<'a>(
    snapshot: &'a Snapshot,
    name: &str,
) -> Option<(EditorModule<'a>, &'a Source, NodeId)> {
    let (path, name) = name.rsplit_once("::")?;
    let module = snapshot.project.module(path)?;
    let source = snapshot.sources.get(path)?;
    let class =
        module
            .symbols()
            .iter()
            .find_map(|symbol| match module.types()[symbol.ty.0 as usize] {
                Type::Class(class) if module.hir().name(symbol.name) == name => Some(class),
                _ => None,
            })?;
    Some((module, source, class))
}
#[derive(Clone, Copy)]
struct ClassSource<'a> {
    module: EditorModule<'a>,
    source: &'a Source,
    class: NodeId,
}
// Source classes can inherit a local class or a canonical imported class. Keep
// each declaration's module so defaults, documentation and types use its source.
fn class_ancestors<'a>(
    snapshot: &'a Snapshot,
    start: ClassSource<'a>,
) -> impl Iterator<Item = ClassSource<'a>> + 'a {
    let mut next = Some(start);
    let mut seen = BTreeSet::new();
    std::iter::from_fn(move || {
        let current = next.take()?;
        if !seen.insert((current.module.canonical_path(), current.class.0)) {
            return None;
        }
        next = match current.module.hir().node(current.class).form {
            Form::Class {
                base: Some(base), ..
            } => {
                match current.module.types()[current.module.facts()[base.0 as usize].ty.0 as usize]
                {
                    Type::Class(class) => Some(ClassSource { class, ..current }),
                    Type::Named(name) => source_class(snapshot, current.module.hir().name(name))
                        .map(|(module, source, class)| ClassSource {
                            module,
                            source,
                            class,
                        }),
                    _ => None,
                }
            }
            _ => None,
        };
        Some(current)
    })
}
fn declared_class_members(
    module: EditorModule<'_>,
    class: NodeId,
    instance: bool,
) -> impl Iterator<Item = (HirToken, NodeId)> + '_ {
    module.hir().children(class).filter_map(move |member| {
        let (node, is_static) = match module.hir().node(member).form {
            Form::Modified {
                declaration,
                static_member,
                ..
            } => (declaration, static_member),
            _ => (member, false),
        };
        if is_static == instance {
            return None;
        }
        let name = match module.hir().node(node).form {
            Form::Function { name, .. } => name,
            Form::Binding { pattern, .. } => match module.hir().node(pattern).form {
                Form::Name { name } => Some(name),
                _ => None,
            },
            _ => None,
        }?;
        Some((name, node))
    })
}
fn source_member_reference<'a>(
    snapshot: &'a Snapshot,
    reference: &SymbolReference,
) -> Option<(EditorModule<'a>, &'a Source, NodeId, bool)> {
    let mut parts = reference.export.as_deref()?.split("::");
    let class = parts.next()?;
    let mode = parts.next()?;
    let name = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    if mode == "variant" {
        return enumerations::definition(snapshot, &reference.module, class, name);
    }
    if !matches!(mode, "instance" | "static") {
        return None;
    }
    let (module, source, class) =
        source_class(snapshot, &format!("{}::{class}", reference.module))?;
    let instance = mode == "instance";
    let node = class_members_from(module, class, instance)
        .into_iter()
        .find_map(|(token, node)| (module.hir().text(token.span) == name).then_some(node))?;
    Some((module, source, node, instance))
}
fn external_signature(
    symbol: &ExternalSymbol,
    snapshot: &Snapshot,
    host: &HostIndex,
) -> Option<Signature> {
    if let Some((module, source, node, bound)) = symbol
        .reference
        .as_deref()
        .and_then(|reference| source_member_reference(snapshot, reference))
        && module.hir().node(node).kind == HirKind::Function
        && let Some(function) = module.facts()[node.0 as usize].function
    {
        return Some(function_signature(
            module,
            source,
            function,
            &symbol.name,
            module.hir().node(node).span,
            bound,
        ));
    }
    if let Some((module, source, id)) = symbol
        .reference
        .as_deref()
        .and_then(|reference| source_symbol(snapshot, reference))
    {
        if let Some(signature) = local_signature(module, source, id) {
            return Some(signature);
        }
        if let Type::Class(class) = module.types()[module.symbols()[id.0 as usize].ty.0 as usize]
            && let Some(contract) = &symbol.signature
        {
            let mut signature = class_ancestors(
                snapshot,
                ClassSource {
                    module,
                    source,
                    class,
                },
            )
            .find_map(|parent| {
                let module = parent.module;
                let (_, node) = declared_class_members(module, parent.class, true)
                    .find(|(name, _)| module.hir().text(name.span) == "init")?;
                module.facts()[node.0 as usize].function.map(|function| {
                    function_signature(
                        module,
                        parent.source,
                        function,
                        &symbol.name,
                        module.hir().node(node).span,
                        true,
                    )
                })
            })
            .unwrap_or(Signature {
                prefix: "fn",
                literal_parameters: Vec::new(),
                parameter_documentation: Default::default(),
                label: String::new(),
                name: symbol.name.clone(),
                result: hint_label(&contract.result),
                parameters: Vec::new(),
                markdown: None,
            });
            signature.label = format!(
                "fn {}({}) -> {}",
                symbol.name,
                signature
                    .parameters
                    .iter()
                    .map(|(_, label)| label.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                hint_label(&contract.result)
            );
            signature.name = symbol.name.clone();
            signature.result = hint_label(&contract.result);
            if let Some(class_docs) = source_docs(source, module.hir().node(class).span) {
                signature.markdown = Some(match signature.markdown {
                    Some(constructor_docs) if constructor_docs != class_docs => {
                        format!("{class_docs}\n\n{constructor_docs}")
                    }
                    _ => class_docs,
                });
            }
            return Some(signature);
        }
    }
    host_signature(symbol, host)
}
fn external_docs(symbol: &ExternalSymbol, snapshot: &Snapshot, host: &HostIndex) -> Option<String> {
    let reference = symbol.reference.as_deref()?;
    if let Some((module, source, node, _)) = source_member_reference(snapshot, reference) {
        return source_docs(source, module.hir().node(node).span);
    }
    if let Some((module, source, id)) = source_symbol(snapshot, reference) {
        return source_docs(source, module.symbols()[id.0 as usize].declaration);
    }
    host.declaration(symbol)
        .map(|declaration| declaration.markdown.clone())
}
// Derived declarations replace inherited names; static and instance sets differ.
fn class_members(module: EditorModule<'_>, ty: TypeId) -> Vec<(HirToken, NodeId)> {
    let (class, instance) = match module.types()[ty.0 as usize] {
        Type::Class(class) => (class, false),
        Type::Instance(class) => (class, true),
        _ => return Vec::new(),
    };
    class_members_from(module, class, instance)
}
fn class_members_from(
    module: EditorModule<'_>,
    class: NodeId,
    instance: bool,
) -> Vec<(HirToken, NodeId)> {
    let mut class = Some(class);
    let mut result = Vec::new();
    let mut names = BTreeSet::new();
    let mut seen = Vec::new();
    while let Some(current) = class {
        if seen.contains(&current) {
            break;
        }
        seen.push(current);
        for (name, node) in declared_class_members(module, current, instance) {
            if names.insert(module.hir().text(name.span)) {
                result.push((name, node));
            }
        }
        class = match module.hir().node(current).form {
            Form::Class {
                base: Some(base), ..
            } => match module.types()[module.facts()[base.0 as usize].ty.0 as usize] {
                Type::Class(base) => Some(base),
                _ => None,
            },
            _ => None,
        };
    }
    result
}
fn source_member(module: EditorModule<'_>, object: NodeId, key: HirToken) -> Option<NodeId> {
    class_members(module, module.facts()[object.0 as usize].ty)
        .into_iter()
        .find_map(|(name, node)| {
            (module.hir().text(name.span) == module.hir().text(key.span)).then_some(node)
        })
}
fn class_completion<'a>(
    snapshot: &'a Snapshot,
    module: EditorModule<'a>,
    source: &'a Source,
    class: NodeId,
    instance: bool,
) -> Vec<CompletionItem> {
    let mut result = Vec::new();
    let mut names = BTreeSet::new();
    for parent in class_ancestors(
        snapshot,
        ClassSource {
            module,
            source,
            class,
        },
    ) {
        let module = parent.module;
        for (name, node) in declared_class_members(module, parent.class, instance) {
            let text = module.hir().text(name.span);
            if !names.insert(text) {
                continue;
            }
            let facts = &module.facts()[node.0 as usize];
            let signature = (module.hir().node(node).kind == HirKind::Function)
                .then_some(facts.function)
                .flatten()
                .map(|function| {
                    function_signature(
                        module,
                        parent.source,
                        function,
                        text,
                        module.hir().node(node).span,
                        instance,
                    )
                });
            result.push(CompletionItem {
                label: text.into(),
                kind: Some(if module.hir().node(node).kind == HirKind::Function {
                    CompletionItemKind::METHOD
                } else {
                    CompletionItemKind::FIELD
                }),
                detail: Some(
                    signature
                        .map_or_else(|| type_label(module, facts.ty), |signature| signature.label),
                ),
                documentation: source_docs(parent.source, module.hir().node(node).span)
                    .map(Documentation::String),
                ..Default::default()
            });
        }
    }
    result
}

enum Target<'a> {
    Symbol {
        id: SymbolId,
        ty: Option<TypeId>,
        node: NodeId,
    },
    External(&'a ExternalSymbol),
    Member {
        node: NodeId,
        key: HirToken,
        ty: TypeId,
    },
}
fn target_at(module: EditorModule<'_>, offset: usize) -> Option<(Target<'_>, TextRange)> {
    let hir = module.hir();
    let range = hir.elements.iter().find_map(|element| match element {
        HirElement::Token(token) if token.kind == K::Ident && contains(token.span, offset) => {
            Some(token.span)
        }
        _ => None,
    })?;
    let node = hir
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.span.contains_range(range))
        .min_by_key(|(_, node)| u32::from(node.span.len()))
        .map(|(index, _)| NodeId(index as u32))?;
    let mut next = Some(node);
    while let Some(node) = next {
        next = hir.node(node).parent;
        let selected = match hir.node(node).form {
            Form::Name { name } | Form::Parameter { name, .. } | Form::Catch { name, .. } => {
                name.span == range
            }
            Form::Function { name, .. } | Form::Class { name, .. } => {
                name.is_some_and(|name| name.span == range)
            }
            Form::Member { key, .. } => key.span == range,
            Form::Field {
                key: FieldKey::Name(key),
                ..
            } => key.span == range,
            Form::ImportPath {
                alias,
                group: None,
                wildcard: false,
            } => {
                let imported = hir
                    .tokens(node)
                    .take_while(|token| token.kind != K::As)
                    .filter(|token| token.kind == K::Ident)
                    .last();
                imported.is_some_and(|token| token.span == range)
                    || alias.is_some_and(|token| token.span == range)
            }
            Form::TypeName => {
                let mut tokens = hir.tokens(node).filter(|token| token.kind == K::Ident);
                let first = tokens.next();
                let last = tokens.last().or(first);
                if first.is_some_and(|token| token.span == range)
                    && first.map(|token| token.span) != last.map(|token| token.span)
                    && let Some(symbol) = module.facts()[node.0 as usize].symbol
                {
                    return Some((
                        external(module, None, Some(symbol)).map_or(
                            Target::Symbol {
                                id: symbol,
                                ty: None,
                                node,
                            },
                            Target::External,
                        ),
                        range,
                    ));
                }
                last.is_some_and(|token| token.span == range)
            }
            _ => false,
        };
        if !selected {
            continue;
        }
        let facts = &module.facts()[node.0 as usize];
        let symbol = facts.symbol.or_else(|| {
            module
                .symbols()
                .iter()
                .enumerate()
                .find(|(_, symbol)| {
                    symbol.declaration == range
                        || (hir.node(node).kind == HirKind::ImportPath
                            && symbol.declaration == hir.node(node).span)
                })
                .map(|(index, _)| SymbolId(index as u32))
        });
        if let Some(external) = external(module, Some(node), symbol) {
            return Some((Target::External(external), range));
        }
        if module.source_types().definition(node).is_some() {
            let key = match hir.node(node).form {
                Form::Member { key, .. }
                | Form::Field {
                    key: FieldKey::Name(key),
                    ..
                } => key,
                _ => continue,
            };
            return Some((
                Target::Member {
                    node,
                    key,
                    ty: facts.ty,
                },
                range,
            ));
        }
        if let Form::Member { object, key, .. } = hir.node(node).form
            && key.span == range
            && let Some(declaration) = source_member(module, object, key)
        {
            return Some((
                Target::Member {
                    node: declaration,
                    key,
                    ty: facts.ty,
                },
                key.span,
            ));
        }
        if let Some(symbol) = symbol {
            return Some((
                Target::Symbol {
                    id: symbol,
                    ty: matches!(hir.node(node).form, Form::Name { .. }).then_some(facts.ty),
                    node,
                },
                range,
            ));
        }
    }
    None
}
fn signature_for(
    snapshot: &Snapshot,
    module: EditorModule<'_>,
    source: &Source,
    host: &HostIndex,
    node: NodeId,
) -> Option<Signature> {
    let facts = &module.facts()[node.0 as usize];
    if let Some(symbol) = external(module, Some(node), facts.symbol)
        && let Some(signature) = external_signature(symbol, snapshot, host)
    {
        return Some(signature);
    }
    if let Some(name) = named_type(module, facts.ty)
        && let Some(call) = host.calls.get(name)
    {
        return external_signature(call, snapshot, host);
    }
    if let Form::Member { object, key, .. } = module.hir().node(node).form
        && let Some(declaration) = source_member(module, object, key)
    {
        return value_signature(
            module,
            source,
            facts.ty,
            module.hir().text(key.span),
            module.hir().node(declaration).span,
        );
    }
    local_signature(module, source, facts.symbol?)
}
fn call_at(module: EditorModule<'_>, offset: usize) -> Option<(NodeId, Option<NodeId>)> {
    module
        .hir()
        .nodes
        .iter()
        .filter(|node| node.kind == HirKind::Call && contains(node.span, offset))
        .min_by_key(|node| u32::from(node.span.len()))
        .and_then(|node| match node.form {
            Form::Call {
                callee, arguments, ..
            } => Some((callee, arguments)),
            _ => None,
        })
}
fn active_parameter(
    module: EditorModule<'_>,
    arguments: Option<NodeId>,
    offset: usize,
    signature: &Signature,
) -> usize {
    let Some(args) = arguments else {
        return 0;
    };
    // Nested commas belong to their own expression, not this argument list.
    let positional = module
        .hir()
        .tokens(args)
        .filter(|token| token.kind == K::Comma && (u32::from(token.span.start()) as usize) < offset)
        .count();
    for argument in module.hir().children(args) {
        let node = module.hir().node(argument);
        if contains(node.span, offset)
            && let Form::Argument {
                name: Some(name), ..
            } = node.form
            && let Some(index) = signature
                .parameters
                .iter()
                .position(|(parameter, _)| parameter == module.hir().text(name.span))
        {
            return index;
        }
    }
    positional
}
fn member_base(snapshot: &Snapshot, module: EditorModule<'_>, offset: usize) -> Option<NodeId> {
    let mut token = snapshot
        .source()
        .parsed
        .syntax()
        .token_at_offset(TextSize::from(offset as u32))
        .left_biased()?;
    while token.kind().is_trivia() {
        token = token.prev_token()?;
    }
    let member = token.parent_ancestors().find(|node| {
        node.kind() == K::MemberExpr
            || (node.kind() == K::EnumPattern
                && node
                    .children()
                    .find(|child| child.kind() == K::ObjectPattern)
                    .is_none_or(|payload| {
                        u32::from(payload.text_range().start()) as usize > offset
                    }))
    })?;
    let base = member.children().next()?;
    module
        .hir()
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| {
            node.span == base.text_range() || base.text_range().contains_range(node.span)
        })
        .max_by_key(|(_, node)| u32::from(node.span.len()))
        .map(|(id, _)| NodeId(id as u32))
}
fn completion(symbol: &ExternalSymbol, snapshot: &Snapshot, host: &HostIndex) -> CompletionItem {
    let signature = external_signature(symbol, snapshot, host);
    let docs = signature
        .as_ref()
        .and_then(|signature| signature.markdown.clone())
        .or_else(|| external_docs(symbol, snapshot, host));
    CompletionItem {
        label: symbol.name.clone(),
        kind: Some(external_kind(symbol, snapshot)),
        detail: Some(external_label(symbol, snapshot, host, &symbol.name)),
        documentation: docs.filter(|docs| !docs.is_empty()).map(|value| {
            Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value,
            })
        }),
        ..Default::default()
    }
}
fn local_label(module: EditorModule<'_>, source: &Source, id: SymbolId) -> String {
    let symbol = &module.symbols()[id.0 as usize];
    let name = module.hir().name(symbol.name);
    if symbol.kind == SymbolKind::Class {
        let declaration = if matches!(module.types()[symbol.ty.0 as usize], Type::Enum(..)) {
            "enum"
        } else {
            "class"
        };
        return format!("{declaration} {name}");
    }
    local_signature(module, source, id).map_or_else(
        || {
            format!(
                "{} {name}: {}",
                if symbol.mutable { "let mut" } else { "let" },
                type_label(module, symbol.ty)
            )
        },
        |signature| signature.label,
    )
}
fn symbol_kind(kind: SymbolKind) -> CompletionItemKind {
    match kind {
        SymbolKind::Function => CompletionItemKind::FUNCTION,
        SymbolKind::Class => CompletionItemKind::CLASS,
        _ => CompletionItemKind::VARIABLE,
    }
}
fn external_kind(symbol: &ExternalSymbol, snapshot: &Snapshot) -> CompletionItemKind {
    match symbol.intrinsic {
        Some(Intrinsic::EnumType) => return CompletionItemKind::ENUM,
        Some(Intrinsic::EnumCase) => return CompletionItemKind::ENUM_MEMBER,
        _ => (),
    }
    if symbol.namespace.is_some()
        && symbol.signature.is_none()
        && symbol.class.is_none()
        && !symbol.type_only
    {
        return CompletionItemKind::MODULE;
    }
    if let Some((module, _, id)) = symbol
        .reference
        .as_deref()
        .and_then(|reference| source_symbol(snapshot, reference))
    {
        return symbol_kind(module.symbols()[id.0 as usize].kind);
    }
    if symbol.class.is_some() || symbol.type_only {
        return CompletionItemKind::CLASS;
    }
    if symbol.signature.is_some() {
        CompletionItemKind::FUNCTION
    } else {
        CompletionItemKind::PROPERTY
    }
}
fn external_label(
    symbol: &ExternalSymbol,
    snapshot: &Snapshot,
    host: &HostIndex,
    name: &str,
) -> String {
    if symbol.intrinsic == Some(Intrinsic::EnumType) {
        return format!("enum {name}");
    }
    if let Some(namespace) = &symbol.namespace
        && symbol.signature.is_none()
        && symbol.class.is_none()
        && !symbol.type_only
    {
        let alias = if namespace.module.rsplit("::").next() == Some(name) {
            String::new()
        } else {
            format!(" as {name}")
        };
        return format!("use {}{alias}", namespace.module);
    }
    if let Some((module, source, id)) = symbol
        .reference
        .as_deref()
        .and_then(|reference| source_symbol(snapshot, reference))
    {
        return local_label(module, source, id);
    }
    if symbol.class.is_some() || symbol.type_only {
        return format!("class {}", symbol.name);
    }
    external_signature(symbol, snapshot, host)
        .map_or_else(|| property_label(symbol, host), |signature| signature.label)
}
fn source_completion(
    module: EditorModule<'_>,
    source: &Source,
    id: SymbolId,
    snapshot: &Snapshot,
    host: &HostIndex,
) -> CompletionItem {
    let symbol = &module.symbols()[id.0 as usize];
    let name = module.hir().name(symbol.name);
    if let Some(external) = external(module, None, Some(id)) {
        let mut item = completion(external, snapshot, host);
        item.label = name.into();
        if !matches!(symbol.kind, SymbolKind::Import | SymbolKind::Host)
            && external.intrinsic != Some(Intrinsic::EnumType)
        {
            item.kind = Some(symbol_kind(symbol.kind));
        }
        item.detail = Some(external_label(external, snapshot, host, name));
        return item;
    }
    CompletionItem {
        label: name.into(),
        kind: Some(
            if matches!(module.types()[symbol.ty.0 as usize], Type::Callable(..)) {
                CompletionItemKind::FUNCTION
            } else {
                symbol_kind(symbol.kind)
            },
        ),
        detail: Some(if symbol.kind == SymbolKind::Class {
            local_label(module, source, id)
        } else {
            local_signature(module, source, id).map_or_else(
                || type_label(module, symbol.ty),
                |signature| signature.label,
            )
        }),
        documentation: source_docs(source, symbol.declaration).map(Documentation::String),
        ..Default::default()
    }
}
fn namespace_completions(snapshot: &Snapshot, host: &HostIndex, path: &str) -> Vec<CompletionItem> {
    if let Some(interface) = host.modules.modules.get(path) {
        return interface
            .exports
            .iter()
            .map(|symbol| completion(symbol, snapshot, host))
            .collect();
    }
    snapshot
        .project
        .module(path)
        .zip(snapshot.sources.get(path))
        .map(|(module, source)| {
            module
                .exports()
                .iter()
                .map(|&id| source_completion(module, source, id, snapshot, host))
                .collect()
        })
        .unwrap_or_default()
}
fn hover_type_aliases(
    module: EditorModule<'_>,
    offset: usize,
    host: &HostIndex,
) -> std::collections::BTreeMap<String, String> {
    let mut aliases = std::collections::BTreeMap::new();
    for id in visible(module, offset) {
        let Some(symbol) = external(module, None, Some(id)).filter(|symbol| symbol.type_only)
        else {
            continue;
        };
        let (Some(reference), TypeHint::Named(canonical)) =
            (symbol.reference.as_deref(), &symbol.ty)
        else {
            continue;
        };
        let Some(export) = &reference.export else {
            continue;
        };
        if format!("{}::{export}", reference.module) != *canonical
            || !host.declarations.contains_key(&reference_key(reference))
        {
            continue;
        }
        let name = module
            .hir()
            .name(module.symbols()[id.0 as usize].name)
            .to_owned();
        aliases
            .entry(canonical.clone())
            .and_modify(|alias: &mut String| {
                if (name.chars().count(), &name) < (alias.chars().count(), alias) {
                    alias.clone_from(&name);
                }
            })
            .or_insert(name);
    }
    aliases
}
fn hover_type(ty: &TypeHint, aliases: &std::collections::BTreeMap<String, String>) -> TypeHint {
    match ty {
        TypeHint::Named(name) => TypeHint::Named(aliases.get(name).unwrap_or(name).clone()),
        TypeHint::Tuple(types) => {
            TypeHint::Tuple(types.iter().map(|ty| hover_type(ty, aliases)).collect())
        }
        TypeHint::Union(types) => {
            TypeHint::Union(types.iter().map(|ty| hover_type(ty, aliases)).collect())
        }
        TypeHint::Record(fields) => TypeHint::Record(
            fields
                .iter()
                .map(|(name, ty)| (name.clone(), hover_type(ty, aliases)))
                .collect(),
        ),
        TypeHint::Callable(parameters, result) => TypeHint::Callable(
            parameters
                .iter()
                .map(|ty| hover_type(ty, aliases))
                .collect(),
            Box::new(hover_type(result, aliases)),
        ),
        _ => ty.clone(),
    }
}
fn hover_signature(signature: &Signature, summarized: &BTreeSet<&str>) -> String {
    let parameters: Vec<_> = signature
        .parameters
        .iter()
        .filter(|(name, _)| !summarized.contains(name.as_str()))
        .map(|(_, label)| label.as_str())
        .collect();
    let label = format!(
        "{} {}({}) -> {}",
        signature.prefix,
        signature.name,
        parameters.join(", "),
        signature.result
    );
    if label.chars().count() <= 100 || parameters.is_empty() {
        return label;
    }
    format!(
        "{} {}(\n    {},\n) -> {}",
        signature.prefix,
        signature.name,
        parameters.join(",\n    "),
        signature.result
    )
}
fn external_hover(
    symbol: &ExternalSymbol,
    snapshot: &Snapshot,
    host: &HostIndex,
    aliases: &std::collections::BTreeMap<String, String>,
    name: &str,
) -> (String, usize) {
    let mut presented = symbol.clone();
    presented.ty = hover_type(&symbol.ty, aliases);
    if let Some(signature) = &mut presented.signature {
        signature.result = hover_type(&signature.result, aliases);
        for parameter in &mut signature.parameters {
            parameter.ty = hover_type(&parameter.ty, aliases);
        }
    }
    if !symbol.type_only
        && symbol.class.is_none()
        && let Some(signature) = external_signature(&presented, snapshot, host)
    {
        let summarized = summarized_properties(symbol, host);
        return (hover_signature(&signature, &summarized), summarized.len());
    }
    if symbol.signature.is_none()
        && symbol.namespace.is_none()
        && !symbol.type_only
        && symbol.class.is_none()
        && symbol
            .reference
            .as_deref()
            .and_then(|reference| source_symbol(snapshot, reference))
            .is_none()
    {
        return (
            property_label_using(symbol, host, |ty| hint_label(&hover_type(ty, aliases))),
            0,
        );
    }
    (external_label(&presented, snapshot, host, name), 0)
}
fn summarized_properties<'a>(symbol: &'a ExternalSymbol, host: &HostIndex) -> BTreeSet<&'a str> {
    let property_names = host
        .declaration(symbol)
        .map(|declaration| {
            declaration
                .property_parameters
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    symbol
        .signature
        .as_ref()
        .into_iter()
        .flat_map(|signature| &signature.parameters)
        .filter(|parameter| parameter.default && property_names.contains(parameter.name.as_str()))
        .map(|parameter| parameter.name.as_str())
        .collect()
}
fn property_group_summary(symbol: &ExternalSymbol, host: &HostIndex) -> String {
    let Some(declaration) = host.declaration(symbol) else {
        return String::new();
    };
    let summarized = summarized_properties(symbol, host);
    declaration
        .property_groups
        .iter()
        .filter_map(|group| {
            let names = group
                .parameters
                .iter()
                .filter(|name| summarized.contains(name.as_str()))
                .map(|name| format!("`{name}`"))
                .collect::<Vec<_>>();
            (!names.is_empty()).then(|| format!("\n\n{}: {}.", group.name, names.join(", ")))
        })
        .collect()
}
fn property_label(symbol: &ExternalSymbol, host: &HostIndex) -> String {
    property_label_using(symbol, host, hint_label)
}
fn property_label_using(
    symbol: &ExternalSymbol,
    host: &HostIndex,
    display: impl Fn(&TypeHint) -> String,
) -> String {
    if symbol.type_only {
        return format!("class {}", symbol.name);
    }
    let key = symbol.reference.as_deref().map(reference_key);
    let member = key.as_ref().and_then(|key| host.members.get(key));
    let mutable = member.is_some_and(|member| member.write_type.is_some());
    let default = host
        .declaration(symbol)
        .and_then(|declaration| declaration.property_default.as_deref())
        .map(|value| format!(" = {value}"))
        .unwrap_or_default();
    let mut label = format!(
        "let {}{}: {}{default}",
        if mutable { "mut " } else { "" },
        symbol.name,
        display(&symbol.ty)
    );
    if let Some(write) = member
        .and_then(|member| member.write_type.as_ref())
        .filter(|write| *write != &symbol.ty)
    {
        label.push_str(&format!("\n// Write type: {}", display(write)));
    }
    label
}

fn completion_token(source: &Source, offset: usize) -> Option<aside_compiler::SyntaxToken> {
    source
        .parsed
        .syntax()
        .token_at_offset(TextSize::from(offset as u32))
        .left_biased()
}
fn previous_completion_token(
    source: &Source,
    offset: usize,
) -> Option<aside_compiler::SyntaxToken> {
    let mut token = completion_token(source, offset)?;
    if (token.kind() == K::Ident || K::KEYWORDS.iter().any(|(kind, _)| *kind == token.kind()))
        && offset > u32::from(token.text_range().start()) as usize
    {
        token = token.prev_token()?;
    }
    while token.kind().is_trivia() {
        token = token.prev_token()?;
    }
    Some(token)
}
fn completion_in_text(source: &Source, offset: usize) -> bool {
    completion_token(source, offset).is_some_and(|token| match token.kind() {
        K::LineComment | K::StringText => offset > u32::from(token.text_range().start()) as usize,
        K::BlockComment => {
            offset > u32::from(token.text_range().start()) as usize
                && ((offset < u32::from(token.text_range().end()) as usize)
                    || !token.text().ends_with("*/"))
        }
        K::StringStart => true,
        K::StringEnd => offset < u32::from(token.text_range().end()) as usize,
        _ => false,
    })
}
enum ImportCompletion {
    Path(Vec<String>),
    Alias,
    Complete,
}
fn import_context(source: &Source, offset: usize) -> Option<ImportCompletion> {
    let mut token = completion_token(source, offset)?;
    while token.kind().is_trivia() {
        token = token.prev_token()?;
    }
    if token.kind() == K::Semi {
        return None;
    }
    let ancestors: Vec<_> = token.parent_ancestors().collect();
    if !ancestors.iter().any(|node| node.kind() == K::UseDecl) {
        return None;
    }
    if token.kind() == K::Ident && (u32::from(token.text_range().end()) as usize) < offset {
        return Some(
            if ancestors
                .iter()
                .filter(|node| node.kind() == K::UsePath)
                .any(|node| {
                    node.children_with_tokens()
                        .any(|element| element.kind() == K::As)
                })
            {
                ImportCompletion::Alias
            } else {
                ImportCompletion::Complete
            },
        );
    }
    if matches!(token.kind(), K::Star | K::RBrace) {
        return Some(ImportCompletion::Alias);
    }
    let mut prefix = Vec::new();
    for path in ancestors
        .iter()
        .rev()
        .filter(|node| node.kind() == K::UsePath)
    {
        for element in path.children_with_tokens() {
            let Some(part) = element.into_token() else {
                continue;
            };
            if u32::from(part.text_range().start()) as usize >= offset {
                break;
            }
            if part.kind() == K::As {
                return Some(ImportCompletion::Alias);
            }
            if part.kind() == K::Ident && (u32::from(part.text_range().end()) as usize) < offset {
                prefix.push(part.text().to_owned());
            }
        }
    }
    Some(ImportCompletion::Path(prefix))
}
fn type_completion_context(source: &Source, offset: usize) -> bool {
    let Some(mut token) = completion_token(source, offset) else {
        return false;
    };
    if token.parent_ancestors().any(|node| {
        matches!(
            node.kind(),
            K::TypeName
                | K::RecordFieldType
                | K::TupleType
                | K::UnionType
                | K::FunctionType
                | K::GroupType
        )
    }) {
        return true;
    }
    while token.kind().is_trivia() {
        let Some(previous) = token.prev_token() else {
            return false;
        };
        token = previous;
    }
    token.kind() == K::Arrow
        || (token.kind() == K::Colon
            && token.parent().is_some_and(|node| {
                matches!(
                    node.kind(),
                    K::LetDecl | K::Param | K::ClassDecl | K::RecordFieldType
                )
            }))
}

mod keywords;
use keywords::keyword_completions;

mod completion;
mod enumerations;
mod navigation;

impl Workspace {}
