//! Name highlighting uses the same identities and recoverable facts as queries.
use crate::{
    position::Encoding,
    workspace::{HostIndex, Snapshot, Workspace, reference_key},
};
use aside_compiler::{
    SyntaxKind as K, TextRange,
    editor::EditorModule,
    hir::*,
    semantic::{AnalysisLimits, ExternalSymbol, TypeHint},
};
use lsp_types::{
    SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokens,
    SemanticTokensFullOptions, SemanticTokensLegend, SemanticTokensOptions, Uri,
};
use std::collections::BTreeMap;

// Indices are part of the advertised legend and the five-integer LSP encoding.
const NAMESPACE: u32 = 0;
const TYPE: u32 = 1;
const CLASS: u32 = 2;
const PARAMETER: u32 = 3;
const VARIABLE: u32 = 4;
const PROPERTY: u32 = 5;
const FUNCTION: u32 = 6;
const METHOD: u32 = 7;
const DECLARATION: u32 = 1;
const READONLY: u32 = 1 << 1;
const STATIC: u32 = 1 << 2;
const DEFAULT_LIBRARY: u32 = 1 << 3;

pub(crate) fn options() -> SemanticTokensOptions {
    SemanticTokensOptions {
        legend: SemanticTokensLegend {
            token_types: vec![
                SemanticTokenType::NAMESPACE,
                SemanticTokenType::TYPE,
                SemanticTokenType::CLASS,
                SemanticTokenType::PARAMETER,
                SemanticTokenType::VARIABLE,
                SemanticTokenType::PROPERTY,
                SemanticTokenType::FUNCTION,
                SemanticTokenType::METHOD,
            ],
            token_modifiers: vec![
                SemanticTokenModifier::DECLARATION,
                SemanticTokenModifier::READONLY,
                SemanticTokenModifier::STATIC,
                SemanticTokenModifier::DEFAULT_LIBRARY,
            ],
        },
        full: Some(SemanticTokensFullOptions::Bool(true)),
        ..Default::default()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Highlight {
    kind: u32,
    modifiers: u32,
}
impl Highlight {
    fn new(kind: u32) -> Self {
        Self { kind, modifiers: 0 }
    }
    fn with(self, modifiers: u32) -> Self {
        Self {
            modifiers: self.modifiers | modifiers,
            ..self
        }
    }
}

fn value_kind(module: EditorModule<'_>, ty: TypeId, fallback: u32) -> u32 {
    match module.types()[ty.0 as usize] {
        Type::Class(_) => CLASS,
        Type::Function(_) | Type::Callable(..) | Type::Closure => FUNCTION,
        Type::BoundMethod(_) => METHOD,
        _ => fallback,
    }
}
fn symbol_highlight(module: EditorModule<'_>, symbol: &Symbol) -> Highlight {
    let kind = match symbol.kind {
        SymbolKind::Class => CLASS,
        SymbolKind::Function => FUNCTION,
        SymbolKind::Parameter | SymbolKind::Receiver => PARAMETER,
        _ => value_kind(module, symbol.ty, VARIABLE),
    };
    Highlight::new(kind).with(if symbol.mutable { 0 } else { READONLY })
}

/// Borrowed canonical export keys retain source mutability through import aliases.
/// Only exported declarations and their class members enter this transient index.
struct Exports<'a> {
    symbols: BTreeMap<(&'a str, &'a str), Highlight>,
    members: BTreeMap<(&'a str, &'a str, bool, &'a str), Highlight>,
}
impl<'a> Exports<'a> {
    fn new(snapshot: &'a Snapshot) -> Self {
        let mut result = Self {
            symbols: BTreeMap::new(),
            members: BTreeMap::new(),
        };
        for module in snapshot.project.modules() {
            for &id in module.exports() {
                let symbol = &module.symbols()[id.0 as usize];
                let name = module.hir().name(symbol.name);
                result.symbols.insert(
                    (module.canonical_path(), name),
                    symbol_highlight(module, symbol),
                );
                if let Type::Class(class) = module.types()[symbol.ty.0 as usize] {
                    for item in module.hir().children(class) {
                        let (node, is_static) = match module.hir().node(item).form {
                            Form::Modified {
                                declaration,
                                static_member,
                                ..
                            } => (declaration, static_member),
                            _ => (item, false),
                        };
                        if let Some((token, highlight)) = class_member(module, node, is_static) {
                            result.members.insert(
                                (
                                    module.canonical_path(),
                                    name,
                                    is_static,
                                    module.hir().text(token.span),
                                ),
                                highlight,
                            );
                        }
                    }
                }
            }
        }
        result
    }
    fn get(&self, symbol: &ExternalSymbol) -> Option<Highlight> {
        let reference = symbol.reference.as_ref()?;
        let export = reference.export.as_deref()?;
        if let Some(highlight) = self.symbols.get(&(reference.module.as_str(), export)) {
            return Some(*highlight);
        }
        let mut parts = export.rsplitn(3, "::");
        let member = parts.next()?;
        let is_static = match parts.next()? {
            "static" => true,
            "instance" => false,
            _ => return None,
        };
        let class = parts.next()?;
        self.members
            .get(&(reference.module.as_str(), class, is_static, member))
            .copied()
    }
}

fn external_highlight(
    symbol: &ExternalSymbol,
    member: bool,
    snapshot: &Snapshot,
    host: &HostIndex,
    exports: &Exports<'_>,
) -> Highlight {
    let kind = if symbol.class.is_some() {
        CLASS
    } else if symbol.type_only {
        TYPE
    } else if symbol.signature.is_some()
        || matches!(symbol.ty, TypeHint::Callable(..) | TypeHint::Closure)
    {
        if member { METHOD } else { FUNCTION }
    } else if symbol.namespace.is_some() {
        NAMESPACE
    } else if member {
        PROPERTY
    } else {
        VARIABLE
    };
    let source = exports.get(symbol);
    let mut highlight = source.unwrap_or_else(|| Highlight::new(kind));
    if source.is_none()
        && (symbol.namespace.is_some()
            || symbol.class.is_some()
            || symbol.type_only
            || symbol.signature.is_some())
    {
        highlight.modifiers |= READONLY;
    }
    if symbol
        .reference
        .as_ref()
        .is_none_or(|reference| !snapshot.sources.contains_key(&reference.module))
    {
        highlight.modifiers |= DEFAULT_LIBRARY;
    }
    if let Some(reference) = symbol.reference.as_ref()
        && let Some(member) = host.members.get(&reference_key(reference))
        && member.write_type.is_none()
    {
        highlight.modifiers |= READONLY;
    }
    highlight
}

fn class_member(
    module: EditorModule<'_>,
    node: NodeId,
    is_static: bool,
) -> Option<(HirToken, Highlight)> {
    let (token, kind, readonly) = match module.hir().node(node).form {
        Form::Function {
            name: Some(name), ..
        } => (name, METHOD, true),
        Form::Binding {
            pattern, mutable, ..
        } => {
            let Form::Name { name } = module.hir().node(pattern).form else {
                return None;
            };
            (name, PROPERTY, !mutable)
        }
        _ => return None,
    };
    Some((
        token,
        Highlight::new(kind)
            .with(if readonly { READONLY } else { 0 })
            .with(if is_static { STATIC } else { 0 }),
    ))
}

fn span_key(span: TextRange) -> (u32, u32) {
    (span.start().into(), span.end().into())
}

impl Workspace {
    /// Return full-document name tokens from the cached editor analysis. Temporary
    /// indices contain classifications and borrowed identities, never project copies.
    pub fn semantic_tokens(
        &mut self,
        uri: &Uri,
        encoding: Encoding,
    ) -> Result<SemanticTokens, String> {
        let snapshot = self.snapshot(uri)?;
        let source = snapshot.source();
        let mut highlights = BTreeMap::new();
        if let Some(module) = snapshot.module() {
            let exports = Exports::new(&snapshot);
            let mut external_nodes = vec![None; module.hir().nodes.len()];
            let mut symbols: Vec<_> = module
                .symbols()
                .iter()
                .map(|symbol| symbol_highlight(module, symbol))
                .collect();
            for binding in module.external_bindings() {
                if let Some(symbol) = binding.symbol {
                    symbols[symbol.0 as usize] = external_highlight(
                        &binding.descriptor,
                        false,
                        &snapshot,
                        &self.host,
                        &exports,
                    );
                }
                if let Some(node) = binding.node {
                    external_nodes[node.0 as usize] = Some(binding.descriptor.as_ref());
                }
            }
            let mut members = BTreeMap::new();
            let mut bases = BTreeMap::new();
            let mut imports = BTreeMap::new();
            for (index, symbol) in module.symbols().iter().enumerate() {
                if symbol.kind == SymbolKind::Import {
                    imports
                        .entry(span_key(symbol.declaration))
                        .or_insert(symbols[index]);
                }
            }
            // Visit each class edge once; field declarations have no lexical symbol.
            for (index, node) in module.hir().nodes.iter().enumerate() {
                if let Form::Class { base, .. } = node.form {
                    let class = NodeId(index as u32);
                    if let Some(base) = base
                        && let Type::Class(parent) =
                            module.types()[module.facts()[base.0 as usize].ty.0 as usize]
                    {
                        bases.insert(class, parent);
                    }
                    for item in module.hir().children(class) {
                        let (member, is_static) = match module.hir().node(item).form {
                            Form::Modified {
                                declaration,
                                static_member,
                                ..
                            } => (declaration, static_member),
                            _ => (item, false),
                        };
                        if let Some((token, highlight)) = class_member(module, member, is_static) {
                            members.insert((class, token.name, is_static), highlight);
                            highlights.insert(span_key(token.span), highlight.with(DECLARATION));
                        }
                    }
                }
            }
            for (index, node) in module.hir().nodes.iter().enumerate() {
                let id = NodeId(index as u32);
                let facts = &module.facts()[index];
                match node.form {
                    Form::Name { name } => {
                        let highlight = facts
                            .symbol
                            .map(|symbol| symbols[symbol.0 as usize])
                            .unwrap_or_else(|| Highlight::new(VARIABLE));
                        highlights.entry(span_key(name.span)).or_insert(highlight);
                    }
                    Form::Function {
                        name: Some(name), ..
                    } => {
                        highlights
                            .entry(span_key(name.span))
                            .or_insert(Highlight::new(FUNCTION).with(DECLARATION | READONLY));
                    }
                    Form::Class {
                        name: Some(name), ..
                    } => {
                        highlights.insert(
                            span_key(name.span),
                            Highlight::new(CLASS).with(DECLARATION | READONLY),
                        );
                    }
                    Form::Parameter { name, mutable, .. } => {
                        highlights.insert(
                            span_key(name.span),
                            Highlight::new(PARAMETER)
                                .with(DECLARATION | if mutable { 0 } else { READONLY }),
                        );
                    }
                    Form::Member { object, key, .. } => {
                        let namespace = external_nodes[object.0 as usize]
                            .is_some_and(|symbol| symbol.namespace.is_some());
                        let is_static = matches!(
                            module.types()[module.facts()[object.0 as usize].ty.0 as usize],
                            Type::Class(_)
                        ) || external_nodes[object.0 as usize]
                            .is_some_and(|symbol| symbol.class.is_some());
                        let mut highlight = external_nodes[index]
                            .map(|symbol| {
                                external_highlight(
                                    symbol, !namespace, &snapshot, &self.host, &exports,
                                )
                            })
                            .unwrap_or_else(|| {
                                Highlight::new(value_kind(module, facts.ty, PROPERTY))
                            });
                        if let Type::Class(mut class) | Type::Instance(mut class) =
                            module.types()[module.facts()[object.0 as usize].ty.0 as usize]
                        {
                            // Match the analyzer's class lookup depth, including
                            // recovery from invalid cyclic inheritance.
                            for _ in 0..AnalysisLimits::default().max_depth.max(1) {
                                if let Some(member) = members.get(&(class, key.name, is_static)) {
                                    highlight = *member;
                                    break;
                                }
                                let Some(parent) = bases.get(&class) else {
                                    break;
                                };
                                class = *parent;
                            }
                        }
                        if is_static {
                            highlight.modifiers |= STATIC;
                        }
                        highlights.insert(span_key(key.span), highlight);
                    }
                    Form::TypeName => {
                        let mut tokens = module
                            .hir()
                            .tokens(id)
                            .filter(|token| token.kind == K::Ident)
                            .peekable();
                        while let Some(token) = tokens.next() {
                            let mut highlight = Highlight::new(if tokens.peek().is_some() {
                                NAMESPACE
                            } else {
                                TYPE
                            });
                            if tokens.peek().is_none() {
                                if let Some(symbol) = external_nodes[index] {
                                    highlight = external_highlight(
                                        symbol, false, &snapshot, &self.host, &exports,
                                    );
                                    highlight.kind =
                                        if symbol.class.is_some() { CLASS } else { TYPE };
                                } else if let Some(symbol) = facts.symbol {
                                    highlight = symbols[symbol.0 as usize];
                                    highlight.kind =
                                        if highlight.kind == CLASS { CLASS } else { TYPE };
                                } else if matches!(
                                    module.types()[facts.ty.0 as usize],
                                    Type::Null
                                        | Type::Bool
                                        | Type::Int
                                        | Type::Float
                                        | Type::String
                                        | Type::Array
                                        | Type::Table
                                        | Type::Future
                                        | Type::Generator
                                        | Type::Closure
                                ) || (matches!(
                                    module.types()[facts.ty.0 as usize],
                                    Type::Any
                                ) && module.hir().text(token.span) == "any")
                                {
                                    highlight.modifiers |= DEFAULT_LIBRARY;
                                }
                            }
                            highlights.insert(span_key(token.span), highlight);
                        }
                    }
                    Form::ImportPath {
                        alias, wildcard, ..
                    } => {
                        for token in module
                            .hir()
                            .tokens(id)
                            .filter(|token| token.kind == K::Ident)
                        {
                            highlights.insert(span_key(token.span), Highlight::new(NAMESPACE));
                        }
                        if !wildcard && let Some(&highlight) = imports.get(&span_key(node.span)) {
                            if let Some(token) = module
                                .hir()
                                .tokens(id)
                                .filter(|token| token.kind == K::Ident && Some(*token) != alias)
                                .last()
                            {
                                highlights.insert(
                                    span_key(token.span),
                                    highlight.with(if alias.is_some() { 0 } else { DECLARATION }),
                                );
                            }
                            if let Some(alias) = alias {
                                highlights
                                    .insert(span_key(alias.span), highlight.with(DECLARATION));
                            }
                        }
                    }
                    Form::Argument {
                        name: Some(name), ..
                    } => {
                        highlights.insert(span_key(name.span), Highlight::new(PARAMETER));
                    }
                    Form::Field {
                        key: FieldKey::Name(name),
                        ..
                    }
                    | Form::PatternField { key: name, .. } => {
                        highlights
                            .entry(span_key(name.span))
                            .or_insert(Highlight::new(PROPERTY));
                    }
                    _ => {}
                }
            }
            for (index, symbol) in module.symbols().iter().enumerate() {
                if !matches!(symbol.kind, SymbolKind::Host | SymbolKind::Import) {
                    highlights.insert(
                        span_key(symbol.declaration),
                        symbols[index].with(DECLARATION),
                    );
                }
            }
        }
        let mut data = Vec::new();
        let mut cursor = 0;
        let (mut line, mut column) = (0, 0);
        let (mut previous_line, mut previous_column) = (0, 0);
        // CST leaves are ordered and include identifiers inside interpolation, but
        // never the words in comments or string text. Advancing once also avoids
        // repeatedly scanning a long UTF-16 line for each identifier position.
        for token in source
            .parsed
            .syntax()
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .filter(|token| token.kind() == K::Ident)
        {
            let (start, end) = span_key(token.text_range());
            let (start, end) = (start as usize, end as usize);
            for ch in source.text[cursor..start].chars() {
                if ch == '\n' {
                    line += 1;
                    column = 0;
                } else {
                    column += width(ch, encoding);
                }
            }
            let length = source.text[start..end]
                .chars()
                .map(|ch| width(ch, encoding))
                .sum();
            let highlight = highlights
                .get(&(start as u32, end as u32))
                .copied()
                .unwrap_or_else(|| Highlight::new(VARIABLE));
            data.push(SemanticToken {
                delta_line: line - previous_line,
                delta_start: if line == previous_line {
                    column - previous_column
                } else {
                    column
                },
                length,
                token_type: highlight.kind,
                token_modifiers_bitset: highlight.modifiers,
            });
            previous_line = line;
            previous_column = column;
            column += length;
            cursor = end;
        }
        Ok(SemanticTokens {
            result_id: None,
            data,
        })
    }
}

fn width(ch: char, encoding: Encoding) -> u32 {
    match encoding {
        Encoding::Utf8 => ch.len_utf8() as u32,
        Encoding::Utf16 => ch.len_utf16() as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HostContext, NamedType, ServerOptions, position::LineIndex, workspace::file_uri};
    use aside_compiler::semantic::{
        CallEffect, ExternalMember, ExternalSignature, SymbolReference,
    };
    use lsp_types::{Position, TextDocumentContentChangeEvent};
    use std::time::Instant;

    fn uri() -> Uri {
        "untitled:tokens.aside".parse().unwrap()
    }
    fn signature(result: TypeHint) -> ExternalSignature {
        ExternalSignature {
            parameters: vec![],
            result,
            result_cases: vec![],
            result_parameter: None,
            effect: CallEffect::NoWait,
            init_safe: Some(true),
            derived_safe: Some(true),
        }
    }
    fn at(source: &str, tokens: &SemanticTokens, offset: usize, encoding: Encoding) -> Highlight {
        let expected = LineIndex::new(source)
            .position(source, offset, encoding)
            .unwrap();
        let (mut line, mut column) = (0, 0);
        for token in &tokens.data {
            line += token.delta_line;
            column = if token.delta_line == 0 {
                column + token.delta_start
            } else {
                token.delta_start
            };
            if Position::new(line, column) == expected {
                return Highlight {
                    kind: token.token_type,
                    modifiers: token.token_modifiers_bitset,
                };
            }
        }
        panic!("missing token at {expected:?}: {}", &source[offset..]);
    }
    fn named(source: &str, tokens: &SemanticTokens, name: &str, last: bool) -> Highlight {
        let offset = if last {
            source.rfind(name)
        } else {
            source.find(name)
        }
        .unwrap();
        at(source, tokens, offset, Encoding::Utf16)
    }
    fn assert_valid_positions(source: &str, tokens: &SemanticTokens, encoding: Encoding) {
        let index = LineIndex::new(source);
        let mut previous_end = 0;
        let (mut line, mut column) = (0, 0);
        for token in &tokens.data {
            line += token.delta_line;
            column = if token.delta_line == 0 {
                column + token.delta_start
            } else {
                token.delta_start
            };
            let start = index
                .offset(source, Position::new(line, column), encoding)
                .unwrap();
            let end = index
                .offset(source, Position::new(line, column + token.length), encoding)
                .unwrap();
            assert!(start >= previous_end && end > start);
            assert!(
                source[start..end]
                    .chars()
                    .all(|ch| ch == '_' || ch.is_alphanumeric())
            );
            previous_end = end;
        }
    }

    #[test]
    fn bindings_shadowing_interpolation_and_recovery_keep_source_positions() {
        let source = "fn 加法(参数:int) {\r\n let 名字=参数; let mut 可写=1;\r\n {let 名字=\"😀 words 名字\"; 名字;}\r\n // 名字 参数 hidden\r\n let 文本=f\"😀 {名字}\"; 可写=2; 名字.\r\n}";
        let mut workspace = Workspace::new(ServerOptions::default());
        workspace.open(uri(), 1, source.into()).unwrap();
        let utf16 = workspace.semantic_tokens(&uri(), Encoding::Utf16).unwrap();
        let utf8 = workspace.semantic_tokens(&uri(), Encoding::Utf8).unwrap();
        assert_valid_positions(source, &utf16, Encoding::Utf16);
        assert_valid_positions(source, &utf8, Encoding::Utf8);
        assert_eq!(utf16.data.len(), utf8.data.len());
        assert_eq!(
            named(source, &utf16, "加法", false),
            Highlight::new(FUNCTION).with(DECLARATION | READONLY)
        );
        assert_eq!(
            named(source, &utf16, "参数", false),
            Highlight::new(PARAMETER).with(DECLARATION | READONLY)
        );
        assert_eq!(
            named(source, &utf16, "可写", false),
            Highlight::new(VARIABLE).with(DECLARATION)
        );
        assert_eq!(
            named(source, &utf16, "可写", true),
            Highlight::new(VARIABLE)
        );
        assert_eq!(
            named(source, &utf16, "名字", true),
            Highlight::new(VARIABLE).with(READONLY)
        );
        let interpolation = source.find("{名字}").unwrap() + "{".len();
        assert_eq!(
            at(source, &utf16, interpolation, Encoding::Utf16),
            Highlight::new(VARIABLE).with(READONLY)
        );
        assert_eq!(
            at(source, &utf8, interpolation, Encoding::Utf8),
            Highlight::new(VARIABLE).with(READONLY)
        );
        assert!(
            utf16
                .data
                .iter()
                .zip(&utf8.data)
                .any(|(a, b)| a.delta_start != b.delta_start || a.length != b.length)
        );
        assert_eq!(
            utf16.data.len(),
            12,
            "string/comment words never become name tokens"
        );
        assert_eq!(
            workspace.analyses, 1,
            "encoding changes reuse the same semantic snapshot"
        );
        workspace
            .change(
                &uri(),
                2,
                vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: "fn main(){let mut 名字=1; 名字;}".into(),
                }],
                Encoding::Utf16,
            )
            .unwrap();
        let updated = workspace.semantic_tokens(&uri(), Encoding::Utf16).unwrap();
        assert_eq!(
            named("fn main(){let mut 名字=1; 名字;}", &updated, "名字", true),
            Highlight::new(VARIABLE)
        );
        assert_eq!(workspace.analyses, 2);
    }

    #[test]
    fn source_class_and_import_aliases_retain_kind_mutability_and_static_members() {
        let project = tempfile::tempdir().unwrap();
        let entry = file_uri(&project.path().join("main.aside")).unwrap();
        let helper = file_uri(&project.path().join("helpers.aside")).unwrap();
        let library = "pub class Counter {let fixed=1; let mut count=0; fn read(self){self.count} static fn create(){Counter()} } pub fn add(value:int){value} pub let mut shared=0;";
        let source = "use helpers::Counter as Meter; use helpers::add; use helpers::shared; use helpers as ns; class Local {let fixed=1; let mut count=0; fn read(self){self.fixed} static fn create(){Local()} } fn main(){let item=Meter(); item.read(); item.count; item.fixed; Meter.create(); let local=Local(); local.read(); local.count; local.fixed; Local.create(); add(1); shared; ns.add(2);}";
        let mut workspace = Workspace::new(ServerOptions::default());
        workspace.open(entry.clone(), 1, source.into()).unwrap();
        workspace.open(helper, 1, library.into()).unwrap();
        let tokens = workspace.semantic_tokens(&entry, Encoding::Utf16).unwrap();
        assert_valid_positions(source, &tokens, Encoding::Utf16);
        assert_eq!(
            named(source, &tokens, "Meter", false),
            Highlight::new(CLASS).with(DECLARATION | READONLY)
        );
        assert_eq!(
            named(source, &tokens, "Counter", false),
            Highlight::new(CLASS).with(READONLY)
        );
        assert_eq!(
            named(source, &tokens, "add;", false),
            Highlight::new(FUNCTION).with(DECLARATION | READONLY)
        );
        assert_eq!(
            named(source, &tokens, "ns;", false),
            Highlight::new(NAMESPACE).with(DECLARATION | READONLY)
        );
        assert_eq!(
            named(source, &tokens, "shared;", true),
            Highlight::new(VARIABLE)
        );
        for prefix in ["item", "local"] {
            for (member, highlight) in [
                ("read", Highlight::new(METHOD).with(READONLY)),
                ("count", Highlight::new(PROPERTY)),
                ("fixed", Highlight::new(PROPERTY).with(READONLY)),
            ] {
                let offset = source.find(&format!("{prefix}.{member}")).unwrap() + prefix.len() + 1;
                assert_eq!(
                    at(source, &tokens, offset, Encoding::Utf16),
                    highlight,
                    "{prefix}.{member}"
                );
            }
        }
        for prefix in ["Meter", "Local"] {
            let offset = source.rfind(&format!("{prefix}.create")).unwrap() + prefix.len() + 1;
            assert_eq!(
                at(source, &tokens, offset, Encoding::Utf16),
                Highlight::new(METHOD).with(READONLY | STATIC)
            );
        }
        assert_eq!(
            at(
                source,
                &tokens,
                source.rfind("ns.add").unwrap() + 3,
                Encoding::Utf16
            ),
            Highlight::new(FUNCTION).with(READONLY)
        );
        assert_eq!(
            named(source, &tokens, "fixed", false),
            Highlight::new(PROPERTY).with(DECLARATION | READONLY)
        );
    }

    #[test]
    fn host_names_types_and_members_use_registered_metadata() {
        let mut make = ExternalSymbol::function("make", signature(TypeHint::Named("Node".into())))
            .with_reference(SymbolReference {
                module: "native".into(),
                version: "v1".into(),
                export: Some("make".into()),
            });
        make.namespace = Some(aside_compiler::semantic::NamespaceRef {
            module: "native::make".into(),
            version: "v1".into(),
        });
        let members = vec![
            ExternalMember {
                symbol: ExternalSymbol::value("width", TypeHint::Float).with_reference(
                    SymbolReference {
                        module: "native".into(),
                        version: "v1".into(),
                        export: Some("Node::width".into()),
                    },
                ),
                write_type: Some(TypeHint::Float),
            },
            ExternalMember {
                symbol: ExternalSymbol::value("focused", TypeHint::Bool).with_reference(
                    SymbolReference {
                        module: "native".into(),
                        version: "v1".into(),
                        export: Some("Node::focused".into()),
                    },
                ),
                write_type: None,
            },
            ExternalMember {
                symbol: ExternalSymbol::function("remove", signature(TypeHint::Null))
                    .with_reference(SymbolReference {
                        module: "native".into(),
                        version: "v1".into(),
                        export: Some("Node::remove".into()),
                    }),
                write_type: None,
            },
        ];
        let mut native_type = ExternalSymbol::value("Node", TypeHint::Named("Node".into()));
        native_type.type_only = true;
        let host = HostContext {
            globals: vec![make, native_type],
            named_types: vec![NamedType {
                name: "Node".into(),
                bases: vec![],
                call: None,
                await_result: None,
                members,
            }],
            ..Default::default()
        };
        let mut workspace = Workspace::new(ServerOptions {
            host,
            ..Default::default()
        });
        let source = "fn main(){let node:Node=make(); node.width; node.focused; node.remove(); let unknown:Missing=node;}";
        workspace.open(uri(), 1, source.into()).unwrap();
        let tokens = workspace.semantic_tokens(&uri(), Encoding::Utf16).unwrap();
        assert_eq!(
            named(source, &tokens, "make", false),
            Highlight::new(FUNCTION).with(READONLY | DEFAULT_LIBRARY)
        );
        assert_eq!(
            named(source, &tokens, "Node", false),
            Highlight::new(TYPE).with(READONLY | DEFAULT_LIBRARY)
        );
        assert_eq!(
            named(source, &tokens, "width", false),
            Highlight::new(PROPERTY).with(DEFAULT_LIBRARY)
        );
        assert_eq!(
            named(source, &tokens, "focused", false),
            Highlight::new(PROPERTY).with(READONLY | DEFAULT_LIBRARY)
        );
        assert_eq!(
            named(source, &tokens, "remove", false),
            Highlight::new(METHOD).with(READONLY | DEFAULT_LIBRARY)
        );
        assert_eq!(
            named(source, &tokens, "Missing", false),
            Highlight::new(TYPE)
        );
    }

    #[test]
    fn protocol_advertises_legend_and_serializes_full_tokens() {
        use lsp_server::{Connection, Message, Notification, Request};
        use serde_json::{Value, json};
        use std::time::Duration;
        let (server, client) = Connection::memory();
        let task =
            std::thread::spawn(|| crate::run_connection(server, ServerOptions::default()).unwrap());
        let request = |id: i32, method: &str, params: Value| {
            client
                .sender
                .send(Message::Request(Request::new(
                    id.into(),
                    method.into(),
                    params,
                )))
                .unwrap()
        };
        request(
            1,
            "initialize",
            json!({"capabilities":{"general":{"positionEncodings":["utf-8"]}}}),
        );
        let Message::Response(initialized) = client
            .receiver
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
        else {
            panic!("initialize response")
        };
        let capabilities = &initialized.result.unwrap()["capabilities"];
        assert_eq!(capabilities["semanticTokensProvider"]["full"], true);
        assert_eq!(
            capabilities["semanticTokensProvider"]["legend"]["tokenTypes"],
            json!([
                "namespace",
                "type",
                "class",
                "parameter",
                "variable",
                "property",
                "function",
                "method"
            ])
        );
        assert_eq!(
            capabilities["semanticTokensProvider"]["legend"]["tokenModifiers"],
            json!(["declaration", "readonly", "static", "defaultLibrary"])
        );
        client
            .sender
            .send(Notification::new("initialized".into(), json!({})).into())
            .unwrap();
        client.sender.send(Notification::new("textDocument/didOpen".into(), json!({"textDocument":{"uri":uri(),"languageId":"aside","version":1,"text":"let 中文=1; 中文;"}})).into()).unwrap();
        request(
            2,
            "textDocument/semanticTokens/full",
            json!({"textDocument":{"uri":uri()}}),
        );
        let tokens = loop {
            if let Message::Response(response) = client
                .receiver
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
            {
                assert!(response.error.is_none(), "{:?}", response.error);
                break response.result.unwrap();
            }
        };
        assert_eq!(
            tokens["data"],
            json!([
                0,
                4,
                6,
                VARIABLE,
                DECLARATION | READONLY,
                0,
                10,
                6,
                VARIABLE,
                READONLY
            ])
        );
        request(3, "shutdown", Value::Null);
        assert!(matches!(
            client
                .receiver
                .recv_timeout(Duration::from_secs(10))
                .unwrap(),
            Message::Response(_)
        ));
        client
            .sender
            .send(Notification::new("exit".into(), Value::Null).into())
            .unwrap();
        task.join().unwrap();
    }

    #[test]
    #[ignore = "reproducible token-query cost measurement"]
    fn measure_long_line_cached_queries() {
        for count in [100, 1_000, 10_000] {
            let source = format!("fn main(){{let value=1; {} }}", "value;".repeat(count));
            let mut workspace = Workspace::new(ServerOptions::default());
            workspace.open(uri(), 1, source.clone()).unwrap();
            workspace.snapshot(&uri()).unwrap();
            let started = Instant::now();
            let mut words = 0;
            for _ in 0..20 {
                let tokens = workspace.semantic_tokens(&uri(), Encoding::Utf16).unwrap();
                assert_eq!(tokens.data.len(), count + 2);
                words = tokens.data.len() * 5;
            }
            eprintln!(
                "semantic tokens: source_bytes={}, names={}, response_u32s={}, queries=20, elapsed_us={}, analyses={}",
                source.len(),
                count + 2,
                words,
                started.elapsed().as_micros(),
                workspace.analyses
            );
            assert_eq!(workspace.analyses, 1);
        }
    }
}
