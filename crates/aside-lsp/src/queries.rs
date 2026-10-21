//! Editor queries consume compiler scopes, facts and canonical symbol references.
use crate::{
    position::Encoding,
    workspace::{HostIndex, Snapshot, Source, Workspace, reference_key},
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
fn node_at(module: EditorModule<'_>, offset: usize) -> Option<NodeId> {
    module
        .hir()
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| contains(node.span, offset))
        .min_by_key(|(_, node)| u32::from(node.span.len()))
        .map(|(index, _)| NodeId(index as u32))
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
    module
        .external_bindings()
        .iter()
        .find(|binding| {
            (node.is_some() && binding.node == node)
                || (symbol.is_some() && binding.symbol == symbol)
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
    parameters: Vec<(String, String)>,
    markdown: Option<String>,
}
fn host_signature(symbol: &ExternalSymbol, host: &HostIndex) -> Option<Signature> {
    let signature = symbol.signature.as_ref()?;
    let declaration = symbol
        .reference
        .as_ref()
        .and_then(|reference| host.declarations.get(&reference_key(reference)));
    let parameters: Vec<_> = signature
        .parameters
        .iter()
        .map(|parameter| {
            let rest = if parameter.rest { "..." } else { "" };
            let default = declaration
                .and_then(|declaration| declaration.parameter_defaults.get(&parameter.name))
                .map(|value| format!(" = {value}"))
                .unwrap_or_default();
            (
                parameter.name.clone(),
                format!(
                    "{rest}{}: {}{default}",
                    parameter.name,
                    hint_label(&parameter.ty)
                ),
            )
        })
        .collect();
    Some(Signature {
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
    host.declarations
        .get(&reference_key(reference))
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
    Symbol(SymbolId),
    External(&'a ExternalSymbol),
    Member {
        node: NodeId,
        key: HirToken,
        ty: TypeId,
    },
}
fn target_at(module: EditorModule<'_>, offset: usize) -> Option<(Target<'_>, TextRange)> {
    let hir = module.hir();
    let node = node_at(module, offset)?;
    let mut next = Some(node);
    while let Some(node) = next {
        let facts = &module.facts()[node.0 as usize];
        if let Some(external) = external(module, Some(node), facts.symbol) {
            return Some((Target::External(external), hir.node(node).span));
        }
        if let Form::Member { object, key, .. } = hir.node(node).form
            && contains(key.span, offset)
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
        if let Some(symbol) = facts.symbol {
            return Some((Target::Symbol(symbol), hir.node(node).span));
        }
        next = hir.node(node).parent;
    }
    module
        .symbols()
        .iter()
        .enumerate()
        .find(|(_, symbol)| contains(symbol.declaration, offset))
        .map(|(id, symbol)| (Target::Symbol(SymbolId(id as u32)), symbol.declaration))
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
    let token = snapshot
        .source()
        .parsed
        .syntax()
        .token_at_offset(TextSize::from(offset as u32))
        .left_biased()?;
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
        kind: Some(if symbol.signature.is_some() {
            CompletionItemKind::FUNCTION
        } else {
            CompletionItemKind::PROPERTY
        }),
        detail: Some(
            signature.map_or_else(|| property_label(symbol, host), |signature| signature.label),
        ),
        documentation: docs.filter(|docs| !docs.is_empty()).map(|value| {
            Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value,
            })
        }),
        ..Default::default()
    }
}
fn property_label(symbol: &ExternalSymbol, host: &HostIndex) -> String {
    if symbol.type_only {
        return format!("class {}", symbol.name);
    }
    let key = symbol.reference.as_deref().map(reference_key);
    let member = key.as_ref().and_then(|key| host.members.get(key));
    let mutable = member.is_some_and(|member| member.write_type.is_some());
    let default = key
        .as_ref()
        .and_then(|key| host.declarations.get(key))
        .and_then(|declaration| declaration.property_default.as_deref())
        .map(|value| format!(" = {value}"))
        .unwrap_or_default();
    let mut label = format!(
        "let {}{}: {}{default}",
        if mutable { "mut " } else { "" },
        symbol.name,
        hint_label(&symbol.ty)
    );
    if let Some(write) = member
        .and_then(|member| member.write_type.as_ref())
        .filter(|write| *write != &symbol.ty)
    {
        label.push_str(&format!("\n// Write type: {}", hint_label(write)));
    }
    label
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
        let Some(module) = snapshot.module() else {
            return Ok(Vec::new());
        };
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
                    .map(|member| completion(&member.symbol, &snapshot, &self.host))
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
                            let symbol = &module.symbols()[id.0 as usize];
                            CompletionItem {
                                label: module.hir().name(symbol.name).into(),
                                ..Default::default()
                            }
                        })
                        .collect());
                }
            }
        }
        let mut items: Vec<_> = visible(module, offset)
            .into_iter()
            .map(|id| {
                let symbol = &module.symbols()[id.0 as usize];
                if let Some(external) = external(module, None, Some(id)) {
                    return completion(external, &snapshot, &self.host);
                }
                let signature = local_signature(module, source, id);
                CompletionItem {
                    label: module.hir().name(symbol.name).into(),
                    kind: Some(match symbol.kind {
                        SymbolKind::Function => CompletionItemKind::FUNCTION,
                        SymbolKind::Class => CompletionItemKind::CLASS,
                        SymbolKind::Parameter => CompletionItemKind::VARIABLE,
                        _ => CompletionItemKind::VARIABLE,
                    }),
                    detail: Some(signature.map_or_else(
                        || type_label(module, symbol.ty),
                        |signature| signature.label,
                    )),
                    documentation: source_docs(source, symbol.declaration)
                        .map(Documentation::String),
                    ..Default::default()
                }
            })
            .collect();
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
                    .filter(|(name, _)| !supplied.contains(name))
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
        let Some(module) = snapshot.module() else {
            return Ok(None);
        };
        let Some((target, range)) = target_at(module, offset) else {
            return Ok(None);
        };
        let (label, docs) = match target {
            Target::External(symbol) => {
                let signature = external_signature(symbol, &snapshot, &self.host);
                let docs = signature
                    .as_ref()
                    .and_then(|signature| signature.markdown.clone())
                    .or_else(|| external_docs(symbol, &snapshot, &self.host));
                (
                    signature.map_or_else(
                        || property_label(symbol, &self.host),
                        |signature| signature.label,
                    ),
                    docs,
                )
            }
            Target::Symbol(id) => {
                let symbol = &module.symbols()[id.0 as usize];
                (
                    local_signature(module, source, id).map_or_else(
                        || {
                            format!(
                                "{} {}: {}",
                                if symbol.mutable { "let mut" } else { "let" },
                                module.hir().name(symbol.name),
                                type_label(module, symbol.ty)
                            )
                        },
                        |signature| signature.label,
                    ),
                    source_docs(source, symbol.declaration),
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
                    |signature| signature.label,
                ),
                source_docs(source, module.hir().node(node).span),
            ),
        };
        let mut value = format!("```aside\n{label}\n```");
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
        let Some(module) = snapshot.module() else {
            return Ok(None);
        };
        let Some((target, _)) = target_at(module, offset) else {
            return Ok(None);
        };
        let reference = match target {
            Target::Symbol(id) => {
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
        if let Some(definition) = self
            .host
            .declarations
            .get(&reference_key(reference))
            .and_then(|declaration| declaration.definition.as_ref())
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
        Ok(Some(SignatureHelp {
            signatures: vec![SignatureInformation {
                label: signature.label,
                documentation: signature.markdown.map(Documentation::String),
                parameters: Some(
                    signature
                        .parameters
                        .into_iter()
                        .map(|(_, label)| ParameterInformation {
                            label: ParameterLabel::Simple(label),
                            documentation: None,
                        })
                        .collect(),
                ),
                active_parameter: Some(active),
            }],
            active_signature: Some(0),
            active_parameter: Some(active),
        }))
    }
}
