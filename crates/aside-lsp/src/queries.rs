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

struct Signature {
    label: String,
    name: String,
    result: String,
    parameters: Vec<(String, String)>,
    markdown: Option<String>,
}
fn host_signature(symbol: &ExternalSymbol, host: &HostIndex) -> Option<Signature> {
    let signature = symbol.signature.as_ref()?;
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
        name: symbol.name.clone(),
        result: hint_label(&signature.result),
        label: format!(
            "fn {}({}) -> {}",
            symbol.name,
            parameters
                .iter()
                .map(|(_, label)| label.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            hint_label(&signature.result)
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
        let parameters: Vec<_> = parameters
            .iter()
            .map(|&ty| (String::new(), type_label(module, ty)))
            .collect();
        let result = type_label(module, *result);
        return Some(Signature {
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
    let function = &module.functions()[function.0 as usize];
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
        name: name.into(),
        result: type_label(module, function.return_type),
        label: format!(
            "fn {name}({}) -> {}",
            parameters
                .iter()
                .map(|(_, label)| label.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            type_label(module, function.return_type)
        ),
        parameters,
        markdown: source_docs(source, declaration),
    }
}
fn source_symbol<'a>(
    snapshot: &'a Snapshot,
    reference: &SymbolReference,
) -> Option<(EditorModule<'a>, &'a Source, SymbolId)> {
    let module = snapshot
        .project
        .modules()
        .find(|module| module.canonical_path() == reference.module)?;
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
    let module = snapshot
        .project
        .modules()
        .find(|module| module.canonical_path() == path)?;
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
    if parts.next().is_some() || !matches!(mode, "instance" | "static") {
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
    let member = token
        .parent_ancestors()
        .find(|node| node.kind() == K::MemberExpr)?;
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
        return format!("class {name}");
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
        if !matches!(symbol.kind, SymbolKind::Import | SymbolKind::Host) {
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
        .modules()
        .find(|module| module.canonical_path() == path)
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
        "fn {}({}) -> {}",
        signature.name,
        parameters.join(", "),
        signature.result
    );
    if label.chars().count() <= 100 || parameters.is_empty() {
        return label;
    }
    format!(
        "fn {}(\n    {},\n) -> {}",
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
        let summarized = symbol
            .signature
            .as_ref()
            .into_iter()
            .flat_map(|signature| &signature.parameters)
            .filter(|parameter| {
                parameter.default && property_names.contains(parameter.name.as_str())
            })
            .map(|parameter| parameter.name.as_str())
            .collect::<BTreeSet<_>>();
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
            K::TypeName | K::TupleType | K::UnionType | K::FunctionType | K::GroupType
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
            && token
                .parent()
                .is_some_and(|node| matches!(node.kind(), K::LetDecl | K::Param | K::ClassDecl)))
}
fn keyword_completions(source: &Source, offset: usize) -> Vec<CompletionItem> {
    let mut token = completion_token(source, offset);
    while token.as_ref().is_some_and(|token| token.kind().is_trivia()) {
        token = token.and_then(|token| token.prev_token());
    }
    let closed_block = |node: &aside_compiler::SyntaxNode| {
        node.children_with_tokens().any(|element| {
            element.kind() == K::RBrace && u32::from(element.text_range().end()) as usize <= offset
        })
    };
    let ancestors: Vec<_> = token
        .as_ref()
        .into_iter()
        .flat_map(|token| token.parent_ancestors())
        .filter(|node| match node.kind() {
            K::Block | K::ClassDecl => !closed_block(node),
            K::FnDecl | K::ClosureExpr => !node
                .children()
                .filter(|node| node.kind() == K::Block)
                .any(|node| closed_block(&node)),
            _ => true,
        })
        .collect();
    if ancestors
        .iter()
        .any(|node| matches!(node.kind(), K::UseDecl | K::MemberExpr))
    {
        return Vec::new();
    }
    let previous_token = previous_completion_token(source, offset);
    let previous = previous_token.as_ref().map(|token| token.kind());
    if matches!(previous, Some(K::Dot | K::QuestionDot | K::ColonColon)) {
        return Vec::new();
    }
    let class_body = ancestors
        .iter()
        .find(|node| matches!(node.kind(), K::Block | K::ClassDecl))
        .is_some_and(|node| {
            node.kind() == K::ClassDecl
                && node.children_with_tokens().any(|element| {
                    element.kind() == K::LBrace
                        && (u32::from(element.text_range().start()) as usize) < offset
                })
        });
    let type_position = type_completion_context(source, offset);
    let function_boundary = ancestors
        .iter()
        .position(|node| matches!(node.kind(), K::FnDecl | K::ClosureExpr));
    let in_loop = ancestors
        .iter()
        .take(function_boundary.unwrap_or(ancestors.len()))
        .any(|node| matches!(node.kind(), K::WhileStmt | K::ForStmt | K::LoopStmt));
    let generator = function_boundary.is_some_and(|index| {
        ancestors[index]
            .children_with_tokens()
            .any(|element| element.kind() == K::Gen)
    });
    let followup = previous_token
        .as_ref()
        .filter(|token| token.kind() == K::RBrace)
        .and_then(|token| token.parent())
        .filter(|node| node.kind() == K::Block)
        .and_then(|node| node.parent())
        .map(|node| node.kind());
    let for_pattern = previous_token.as_ref().is_some_and(|token| {
        token
            .parent_ancestors()
            .take_while(|node| node.kind() != K::ForStmt)
            .any(|node| node.kind() == K::NamePattern)
    }) && ancestors.iter().any(|node| node.kind() == K::ForStmt);
    let expression = matches!(
        previous,
        Some(
            K::Eq
                | K::LParen
                | K::LBracket
                | K::Comma
                | K::Colon
                | K::Return
                | K::Throw
                | K::Yield
                | K::If
                | K::While
                | K::Match
                | K::Await
                | K::FatArrow
                | K::Plus
                | K::Minus
                | K::Star
                | K::Slash
                | K::AndAnd
                | K::OrOr
                | K::QuestionQuestion
        )
    );
    K::KEYWORDS
        .iter()
        .filter(|(kind, _)| match previous {
            Some(K::Let) => *kind == K::Mut,
            Some(K::Fn | K::Class | K::As | K::Mut) => false,
            _ if *kind == K::Else && !expression && !type_position => followup == Some(K::IfExpr),
            _ if *kind == K::Catch && !expression && !type_position => followup == Some(K::TryStmt),
            _ if *kind == K::Finally && !expression && !type_position => {
                matches!(followup, Some(K::TryStmt | K::CatchClause))
            }
            _ if for_pattern => *kind == K::In,
            Some(K::Async | K::Gen) => *kind == K::Fn,
            Some(K::Static) => matches!(kind, K::Let | K::Fn | K::Async | K::Gen),
            Some(K::Pub) => matches!(kind, K::Let | K::Fn | K::Class | K::Use | K::Async | K::Gen),
            _ if type_position => matches!(kind, K::Fn | K::Null),
            _ if class_body => matches!(kind, K::Let | K::Fn | K::Static | K::Async | K::Gen),
            _ if expression => matches!(
                kind,
                K::Fn
                    | K::Class
                    | K::Async
                    | K::Gen
                    | K::If
                    | K::Match
                    | K::Await
                    | K::True
                    | K::False
                    | K::Null
                    | K::Yield
            ),
            _ if matches!(kind, K::Break | K::Continue) => in_loop,
            _ if *kind == K::Yield => generator,
            _ if matches!(kind, K::Return | K::Await) => function_boundary.is_some(),
            _ => !matches!(
                kind,
                K::Mut | K::Static | K::As | K::In | K::Is | K::Else | K::Catch | K::Finally
            ),
        })
        .map(|(_, name)| CompletionItem {
            label: (*name).into(),
            kind: Some(CompletionItemKind::KEYWORD),
            ..Default::default()
        })
        .collect()
}

impl Workspace {
    pub fn complete(
        &mut self,
        uri: &Uri,
        position: Position,
        encoding: Encoding,
    ) -> Result<Vec<CompletionItem>, String> {
        let snapshot = self.snapshot(uri)?;
        let source = snapshot.source();
        let offset = source
            .lines
            .offset(&source.text, position, encoding)
            .ok_or("invalid completion position")?;
        if completion_in_text(source, offset) {
            return Ok(Vec::new());
        }
        if let Some(region) = snapshot
            .features
            .get(&snapshot.entry)
            .into_iter()
            .flat_map(|features| &features.completions)
            .filter(|region| contains(region.range, offset))
            .min_by_key(|region| u32::from(region.range.len()))
        {
            return Ok(region.items.to_vec());
        }
        if let Some(context) = import_context(source, offset) {
            let prefix = match context {
                ImportCompletion::Path(prefix) => prefix,
                ImportCompletion::Alias => return Ok(Vec::new()),
                ImportCompletion::Complete => {
                    return Ok(vec![CompletionItem {
                        label: "as".into(),
                        kind: Some(CompletionItemKind::KEYWORD),
                        ..Default::default()
                    }]);
                }
            };
            let prefix_refs: Vec<_> = prefix.iter().map(String::as_str).collect();
            let path = prefix.join("::");
            let mut items = std::collections::BTreeMap::new();
            for name in self.import_modules(uri, &prefix_refs) {
                items.insert(
                    name.clone(),
                    CompletionItem {
                        label: name,
                        kind: Some(CompletionItemKind::MODULE),
                        ..Default::default()
                    },
                );
            }
            for name in self.host.modules.modules.keys().map(String::as_str).chain(
                snapshot
                    .project
                    .modules()
                    .map(|module| module.canonical_path()),
            ) {
                let suffix = if path.is_empty() {
                    Some(name)
                } else {
                    name.strip_prefix(&format!("{path}::"))
                };
                if let Some(name) = suffix
                    .and_then(|suffix| suffix.split("::").next())
                    .filter(|name| !name.is_empty())
                {
                    items.entry(name.into()).or_insert_with(|| CompletionItem {
                        label: name.into(),
                        kind: Some(CompletionItemKind::MODULE),
                        ..Default::default()
                    });
                }
            }
            if let Some(interface) = self.host.modules.modules.get(&path) {
                for symbol in &interface.exports {
                    items.insert(
                        symbol.name.clone(),
                        completion(symbol, &snapshot, &self.host),
                    );
                }
            }
            if let Some(module) = snapshot
                .project
                .modules()
                .find(|module| module.canonical_path() == path)
                && let Some(source) = snapshot.sources.get(&path)
            {
                for &id in module.exports() {
                    let item = source_completion(module, source, id, &snapshot, &self.host);
                    items.insert(item.label.clone(), item);
                }
            }
            return Ok(items.into_values().collect());
        }
        let keywords = keyword_completions(source, offset);
        if previous_completion_token(source, offset).is_some_and(|token| {
            matches!(
                token.kind(),
                K::Let | K::Mut | K::Fn | K::Class | K::As | K::Pub | K::Static | K::Async | K::Gen
            )
        }) && !type_completion_context(source, offset)
        {
            return Ok(keywords);
        }
        let Some(module) = snapshot.module() else {
            return Ok(keywords);
        };
        if type_completion_context(source, offset) {
            if let Some(node) = completion_token(source, offset).and_then(|token| {
                token
                    .parent_ancestors()
                    .find(|node| node.kind() == K::TypeName)
            }) {
                let parts: Vec<_> = node
                    .children_with_tokens()
                    .filter_map(|element| element.into_token())
                    .filter(|token| {
                        token.kind() == K::Ident
                            && (u32::from(token.text_range().end()) as usize) < offset
                    })
                    .collect();
                if let Some(first) = parts.first()
                    && let Some(namespace) = visible(module, offset).into_iter().find_map(|id| {
                        let symbol = &module.symbols()[id.0 as usize];
                        (module.hir().name(symbol.name) == first.text())
                            .then(|| {
                                external(module, None, Some(id))
                                    .and_then(|symbol| symbol.namespace.as_ref())
                            })
                            .flatten()
                    })
                {
                    let mut path = namespace.module.clone();
                    for part in parts.iter().skip(1) {
                        path.push_str("::");
                        path.push_str(part.text());
                    }
                    return Ok(namespace_completions(&snapshot, &self.host, &path)
                        .into_iter()
                        .filter(|item| {
                            matches!(
                                item.kind,
                                Some(CompletionItemKind::CLASS | CompletionItemKind::MODULE)
                            )
                        })
                        .collect());
                }
            }
            let mut items: std::collections::BTreeMap<_, _> = BUILTIN_TYPE_NAMES
                .iter()
                .map(|name| {
                    (
                        (*name).to_owned(),
                        CompletionItem {
                            label: (*name).into(),
                            kind: Some(CompletionItemKind::TYPE_PARAMETER),
                            ..Default::default()
                        },
                    )
                })
                .collect();
            for id in visible(module, offset) {
                let item = source_completion(module, source, id, &snapshot, &self.host);
                if matches!(
                    item.kind,
                    Some(CompletionItemKind::CLASS | CompletionItemKind::MODULE)
                ) {
                    items.insert(item.label.clone(), item);
                }
            }
            for item in keywords {
                items.entry(item.label.clone()).or_insert(item);
            }
            return Ok(items.into_values().collect());
        }
        if let Some(base) = member_base(&snapshot, module, offset) {
            if let Some(class) =
                external(module, Some(base), module.facts()[base.0 as usize].symbol)
                    .and_then(|symbol| symbol.class.as_ref())
            {
                if let Some(name) = named_type(module, module.facts()[base.0 as usize].ty)
                    && let Some((module, source, class)) = source_class(&snapshot, name)
                {
                    return Ok(class_completion(&snapshot, module, source, class, false));
                }
                return Ok(class
                    .static_members
                    .iter()
                    .map(|member| completion(&member.symbol, &snapshot, &self.host))
                    .collect());
            }
            if let Some(name) = named_type(module, module.facts()[base.0 as usize].ty)
                && let Some(members) = self.host.types.get(name)
            {
                return Ok(members
                    .values()
                    .map(|member| {
                        let mut item = completion(&member.symbol, &snapshot, &self.host);
                        if member.symbol.signature.is_some() {
                            item.kind = Some(CompletionItemKind::METHOD);
                        }
                        item
                    })
                    .collect());
            }
            if let Some(name) = named_type(module, module.facts()[base.0 as usize].ty)
                && let Some((module, source, class)) = source_class(&snapshot, name)
            {
                return Ok(class_completion(&snapshot, module, source, class, true));
            }
            let ty = module.facts()[base.0 as usize].ty;
            if matches!(
                module.types()[ty.0 as usize],
                Type::Class(_) | Type::Instance(_)
            ) {
                let (class, instance) = match module.types()[ty.0 as usize] {
                    Type::Class(class) => (class, false),
                    Type::Instance(class) => (class, true),
                    _ => unreachable!(),
                };
                return Ok(class_completion(&snapshot, module, source, class, instance));
            }
            if let Some(namespace) =
                external(module, Some(base), module.facts()[base.0 as usize].symbol)
                    .and_then(|symbol| symbol.namespace.as_ref())
            {
                if let Some(interface) = self.host.modules.modules.get(&namespace.module) {
                    return Ok(interface
                        .exports
                        .iter()
                        .map(|symbol| completion(symbol, &snapshot, &self.host))
                        .collect());
                }
                if let Some(module) = snapshot
                    .project
                    .modules()
                    .find(|module| module.canonical_path() == namespace.module)
                {
                    return Ok(module
                        .exports()
                        .iter()
                        .map(|&id| {
                            source_completion(
                                module,
                                &snapshot.sources[module.canonical_path()],
                                id,
                                &snapshot,
                                &self.host,
                            )
                        })
                        .collect());
                }
            }
            if let Some(group) = builtin_members(
                module.hir(),
                module.types(),
                module.facts()[base.0 as usize].ty,
            ) {
                return Ok(group
                    .members
                    .iter()
                    .map(|member| {
                        let mut item = completion(&member.symbol, &snapshot, &self.host);
                        item.kind = Some(CompletionItemKind::METHOD);
                        item
                    })
                    .collect());
            }
        }
        let mut items: Vec<_> = visible(module, offset)
            .into_iter()
            .map(|id| source_completion(module, source, id, &snapshot, &self.host))
            .collect();
        items.extend(keywords);
        if let Some((callee, arguments)) = call_at(module, offset)
            && let Some(signature) = signature_for(&snapshot, module, source, &self.host, callee)
        {
            let mut supplied = BTreeSet::new();
            let mut positional = 0;
            for arg in arguments
                .into_iter()
                .flat_map(|args| module.hir().children(args))
            {
                match module.hir().node(arg).form {
                    Form::Argument {
                        name: Some(name), ..
                    } => {
                        supplied.insert(module.hir().text(name.span).to_owned());
                    }
                    Form::Argument { name: None, value }
                        if module.hir().node(value).kind == HirKind::Name
                            && contains(module.hir().node(value).span, offset) => {}
                    Form::Argument { name: None, .. } => {
                        if let Some((name, _)) = signature.parameters.get(positional) {
                            supplied.insert(name.clone());
                        }
                        positional += 1;
                    }
                    _ => {}
                }
            }
            items.extend(
                signature
                    .parameters
                    .into_iter()
                    .filter(|(name, label)| {
                        !name.is_empty() && !supplied.contains(name) && !label.starts_with("...")
                    })
                    .map(|(name, label)| CompletionItem {
                        label: format!("{name}:"),
                        kind: Some(CompletionItemKind::FIELD),
                        detail: Some(label),
                        insert_text: Some(format!("{name}: ")),
                        sort_text: Some(format!("0{name}")),
                        ..Default::default()
                    }),
            );
        }
        Ok(items)
    }
    pub fn hover(
        &mut self,
        uri: &Uri,
        position: Position,
        encoding: Encoding,
    ) -> Result<Option<Hover>, String> {
        let snapshot = self.snapshot(uri)?;
        let source = snapshot.source();
        let offset = source
            .lines
            .offset(&source.text, position, encoding)
            .ok_or("invalid hover position")?;
        if let Some(path) = import_path_at(source, offset)
            && let Some(value) = self.module_hover(uri, &path)
        {
            return Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value,
                }),
                range: Some(source.lines.range(&source.text, path.range, encoding)),
            }));
        }
        if let Some(hover) = snapshot
            .features
            .get(&snapshot.entry)
            .into_iter()
            .flat_map(|features| &features.hovers)
            .filter(|hover| hover.range.contains(TextSize::from(offset as u32)))
            .min_by_key(|hover| u32::from(hover.range.len()))
        {
            return Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: hover.markdown.to_string(),
                }),
                range: Some(source.lines.range(&source.text, hover.range, encoding)),
            }));
        }
        let Some(module) = snapshot.module() else {
            return Ok(None);
        };
        let Some((target, range)) = target_at(module, offset) else {
            return Ok(None);
        };
        let aliases = hover_type_aliases(module, offset, &self.host);
        let (label, docs, additional_properties) = match target {
            Target::External(symbol) => {
                let signature = external_signature(symbol, &snapshot, &self.host);
                let docs = signature
                    .as_ref()
                    .and_then(|signature| signature.markdown.clone())
                    .or_else(|| external_docs(symbol, &snapshot, &self.host));
                let (label, summarized) = external_hover(
                    symbol,
                    &snapshot,
                    &self.host,
                    &aliases,
                    module.hir().text(range),
                );
                (label, docs, summarized)
            }
            Target::Symbol { id, ty } => {
                let symbol = &module.symbols()[id.0 as usize];
                (
                    local_signature(module, source, id).map_or_else(
                        || match ty {
                            Some(ty) if ty != symbol.ty => format!(
                                "let {}: {}",
                                module.hir().name(symbol.name),
                                type_label(module, ty)
                            ),
                            _ => local_label(module, source, id),
                        },
                        |signature| hover_signature(&signature, &BTreeSet::new()),
                    ),
                    source_docs(source, symbol.declaration),
                    0,
                )
            }
            Target::Member { node, key, ty } => (
                value_signature(
                    module,
                    source,
                    ty,
                    module.hir().text(key.span),
                    module.hir().node(node).span,
                )
                .map_or_else(
                    || {
                        format!(
                            "let {}: {}",
                            module.hir().text(key.span),
                            type_label(module, ty)
                        )
                    },
                    |signature| hover_signature(&signature, &BTreeSet::new()),
                ),
                source_docs(source, module.hir().node(node).span),
                0,
            ),
        };
        let mut value = format!("```aside\n{label}\n```");
        if additional_properties > 0 {
            value.push_str(&format!("\n\nAdditional named properties: {additional_properties}. Use named-argument completion or Go to Definition."));
        }
        if let Some(docs) = docs.filter(|docs| !docs.is_empty()) {
            value.push_str("\n\n");
            value.push_str(&docs);
        }
        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value,
            }),
            range: Some(source.lines.range(&source.text, range, encoding)),
        }))
    }
    pub fn definition(
        &mut self,
        uri: &Uri,
        position: Position,
        encoding: Encoding,
    ) -> Result<Option<Location>, String> {
        let snapshot = self.snapshot(uri)?;
        let source = snapshot.source();
        let offset = source
            .lines
            .offset(&source.text, position, encoding)
            .ok_or("invalid definition position")?;
        if let Some(path) = import_path_at(source, offset)
            && let Some(location) = self.module_definition(uri, &path, encoding)
        {
            return Ok(Some(location));
        }
        let Some(module) = snapshot.module() else {
            return Ok(None);
        };
        let Some((target, _)) = target_at(module, offset) else {
            return Ok(None);
        };
        let host_declaration = match &target {
            Target::External(symbol) => self.host.declaration(symbol),
            _ => None,
        };
        let reference = match target {
            Target::Symbol { id, .. } => {
                return Ok(Some(Location {
                    uri: source.uri.clone(),
                    range: source.lines.range(
                        &source.text,
                        module.symbols()[id.0 as usize].declaration,
                        encoding,
                    ),
                }));
            }
            Target::Member { node, .. } => {
                let span = match module.hir().node(node).form {
                    Form::Function {
                        name: Some(name), ..
                    } => name.span,
                    Form::Binding { pattern, .. } => module.hir().node(pattern).span,
                    _ => unreachable!("class member is a field or method"),
                };
                return Ok(Some(Location {
                    uri: source.uri.clone(),
                    range: source.lines.range(&source.text, span, encoding),
                }));
            }
            Target::External(symbol) => symbol.reference.as_deref(),
        };
        let Some(reference) = reference else {
            return Ok(None);
        };
        if let Some(definition) =
            host_declaration.and_then(|declaration| declaration.definition.as_ref())
            && let Some(document) = self.host.documents.get(definition.uri.as_str())
        {
            return Ok(Some(Location {
                uri: definition.uri.clone(),
                range: crate::position::LineIndex::new(&document.text).range(
                    &document.text,
                    definition.range,
                    encoding,
                ),
            }));
        }
        if let Some((module, source, node, _)) = source_member_reference(&snapshot, reference) {
            let range = match module.hir().node(node).form {
                Form::Function {
                    name: Some(name), ..
                } => name.span,
                Form::Binding { pattern, .. } => module.hir().node(pattern).span,
                _ => unreachable!(),
            };
            return Ok(Some(Location {
                uri: source.uri.clone(),
                range: source.lines.range(&source.text, range, encoding),
            }));
        }
        let Some(target) = snapshot
            .project
            .modules()
            .find(|module| module.canonical_path() == reference.module)
        else {
            return Ok(None);
        };
        let Some(source) = snapshot.sources.get(&reference.module) else {
            return Ok(None);
        };
        if reference.export.is_none() {
            return Ok(Some(Location {
                uri: source.uri.clone(),
                range: Range::new(Position::default(), Position::default()),
            }));
        }
        let symbol = target.exports().iter().find_map(|&id| {
            let symbol = &target.symbols()[id.0 as usize];
            (Some(target.hir().name(symbol.name)) == reference.export.as_deref()).then_some(symbol)
        });
        Ok(symbol.map(|symbol| Location {
            uri: source.uri.clone(),
            range: source
                .lines
                .range(&source.text, symbol.declaration, encoding),
        }))
    }
    pub fn signature(
        &mut self,
        uri: &Uri,
        position: Position,
        encoding: Encoding,
    ) -> Result<Option<SignatureHelp>, String> {
        let snapshot = self.snapshot(uri)?;
        let source = snapshot.source();
        let offset = source
            .lines
            .offset(&source.text, position, encoding)
            .ok_or("invalid signature position")?;
        let Some(module) = snapshot.module() else {
            return Ok(None);
        };
        let Some((callee, args)) = call_at(module, offset) else {
            return Ok(None);
        };
        let Some(signature) = signature_for(&snapshot, module, source, &self.host, callee) else {
            return Ok(None);
        };
        let mut active = 0;
        if let Some(args) = args {
            // Only separators owned by this argument list advance the parameter.
            // Commas inside nested calls, tuples and arrays belong to their nodes.
            active = module
                .hir()
                .tokens(args)
                .filter(|token| {
                    token.kind == K::Comma && (u32::from(token.span.start()) as usize) < offset
                })
                .count();
            for arg in module.hir().children(args) {
                if contains(module.hir().node(arg).span, offset)
                    && let Form::Argument {
                        name: Some(name), ..
                    } = module.hir().node(arg).form
                    && let Some(index) = signature.parameters.iter().position(|(parameter, _)| {
                        Some(parameter.as_str()) == name.name.map(|name| module.hir().name(name))
                    })
                {
                    active = index;
                }
            }
        }
        let active = active.min(signature.parameters.len().saturating_sub(1)) as u32;
        let mut parameter_offset = 4 + signature.name.encode_utf16().count() as u32;
        let parameters = signature
            .parameters
            .into_iter()
            .map(|(name, label)| {
                let end = parameter_offset + label.encode_utf16().count() as u32;
                let label = if name.is_empty() {
                    ParameterLabel::LabelOffsets([parameter_offset, end])
                } else {
                    ParameterLabel::Simple(label)
                };
                parameter_offset = end + 2;
                ParameterInformation {
                    label,
                    documentation: None,
                }
            })
            .collect();
        Ok(Some(SignatureHelp {
            signatures: vec![SignatureInformation {
                label: signature.label,
                documentation: signature.markdown.map(Documentation::String),
                parameters: Some(parameters),
                active_parameter: Some(active),
            }],
            active_signature: Some(0),
            active_parameter: Some(active),
        }))
    }
}
