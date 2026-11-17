//! Document navigation and edits from the current buffer's lossless tree.
use aside_compiler::{DiagnosticCode, SyntaxKind as K, SyntaxNode, SyntaxToken, TextRange};
use lsp_types::*;

use crate::{
    position::Encoding,
    workspace::{Source, Workspace},
};

impl Workspace {
    pub(crate) fn document_symbols(
        &self,
        uri: &Uri,
        encoding: Encoding,
        hierarchical: bool,
    ) -> Result<DocumentSymbolResponse, String> {
        let source = self.open_source(uri)?;
        let symbols = Outline { source, encoding }.children(&source.parsed.syntax(), false);
        if hierarchical {
            return Ok(DocumentSymbolResponse::Nested(symbols));
        }
        let mut flat = Vec::new();
        flatten(symbols, uri, None, &mut flat);
        Ok(DocumentSymbolResponse::Flat(flat))
    }

    pub(crate) fn quick_fixes(
        &self,
        params: CodeActionParams,
        encoding: Encoding,
    ) -> Result<CodeActionResponse, String> {
        let uri = params.text_document.uri;
        if uri.as_str().starts_with("aside-native:")
            || params
                .context
                .only
                .as_ref()
                .is_some_and(|kinds| !kinds.contains(&CodeActionKind::QUICKFIX))
        {
            return Ok(Vec::new());
        }
        let source = self.open_source(&uri)?;
        let range = |bytes| source.lines.range(&source.text, bytes, encoding);
        Ok(source
            .parsed
            .diagnostics()
            .iter()
            .filter_map(|diagnostic| {
                let fix = diagnostic.fix.as_ref()?;
                let diagnostic_range = range(diagnostic.range);
                if params.range.end < diagnostic_range.start
                    || diagnostic_range.end < params.range.start
                {
                    return None;
                }
                let code = NumberOrString::String(diagnostic.code.as_str().into());
                let matched = params.context.diagnostics.iter().find(|reported| {
                    reported.range == diagnostic_range
                        && reported.code.as_ref() == Some(&code)
                        && reported.source.as_deref() == Some("aside")
                });
                let edit = TextDocumentEdit {
                    text_document: OptionalVersionedTextDocumentIdentifier {
                        uri: uri.clone(),
                        version: source.version,
                    },
                    edits: vec![OneOf::Left(TextEdit {
                        range: range(fix.range),
                        new_text: fix.replacement.clone(),
                    })],
                };
                Some(CodeActionOrCommand::CodeAction(CodeAction {
                    title: match diagnostic.code {
                        DiagnosticCode::MissingSemicolon => "Insert missing semicolon".into(),
                        _ => format!("Fix: {}", diagnostic.message),
                    },
                    kind: Some(CodeActionKind::QUICKFIX),
                    diagnostics: matched.map(|reported| vec![reported.clone()]),
                    edit: Some(WorkspaceEdit {
                        document_changes: Some(DocumentChanges::Edits(vec![edit])),
                        ..Default::default()
                    }),
                    ..Default::default()
                }))
            })
            .collect())
    }
}

struct Outline<'a> {
    source: &'a Source,
    encoding: Encoding,
}

impl Outline<'_> {
    fn children(&self, parent: &SyntaxNode, member: bool) -> Vec<DocumentSymbol> {
        let mut symbols = Vec::new();
        for outer in parent.children() {
            let declaration = if outer.kind() == K::ModifiedDecl {
                outer.children().next()
            } else {
                Some(outer.clone())
            };
            let Some(declaration) = declaration else {
                continue;
            };
            let range = outer.text_range();
            match declaration.kind() {
                K::FnDecl => symbols.extend(ident(&declaration).map(|name| {
                    let kind = if member {
                        SymbolKind::METHOD
                    } else {
                        SymbolKind::FUNCTION
                    };
                    self.symbol(name, kind, range, None)
                })),
                K::ClassDecl => symbols.extend(ident(&declaration).map(|name| {
                    self.symbol(
                        name,
                        SymbolKind::CLASS,
                        range,
                        Some(self.children(&declaration, true)),
                    )
                })),
                K::LetDecl => {
                    let kind = if member {
                        SymbolKind::FIELD
                    } else {
                        SymbolKind::VARIABLE
                    };
                    if let Some(pattern) = declaration.children().next() {
                        let mut bindings = Vec::new();
                        binding_names(&pattern, &mut bindings);
                        symbols.extend(
                            bindings
                                .into_iter()
                                .map(|name| self.symbol(name, kind, range, None)),
                        );
                    }
                }
                _ => {}
            }
        }
        symbols
    }

    #[allow(deprecated)]
    fn symbol(
        &self,
        name: SyntaxToken,
        kind: SymbolKind,
        range: TextRange,
        children: Option<Vec<DocumentSymbol>>,
    ) -> DocumentSymbol {
        let position = |range| {
            self.source
                .lines
                .range(&self.source.text, range, self.encoding)
        };
        DocumentSymbol {
            name: name.text().into(),
            detail: None,
            kind,
            tags: None,
            deprecated: None,
            range: position(range),
            selection_range: position(name.text_range()),
            children,
        }
    }
}

fn ident(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(|element| element.into_token())
        .find(|token| token.kind() == K::Ident)
}

fn binding_names(pattern: &SyntaxNode, names: &mut Vec<SyntaxToken>) {
    match pattern.kind() {
        K::NamePattern | K::RestPattern => {
            names.extend(ident(pattern).filter(|name| name.text() != "_"))
        }
        K::PatternField
            if !pattern
                .children_with_tokens()
                .any(|part| part.kind() == K::Colon) =>
        {
            names.extend(ident(pattern))
        }
        K::TuplePattern
        | K::ArrayPattern
        | K::GroupPattern
        | K::ObjectPattern
        | K::PatternField
        | K::TypePattern => {
            for child in pattern.children() {
                binding_names(&child, names);
            }
        }
        // The alternatives of a valid or-pattern declare the same bindings.
        K::OrPattern => {
            if let Some(first) = pattern.children().next() {
                binding_names(&first, names);
            }
        }
        _ => {}
    }
}

#[allow(deprecated)]
fn flatten(
    symbols: Vec<DocumentSymbol>,
    uri: &Uri,
    container: Option<&str>,
    flat: &mut Vec<SymbolInformation>,
) {
    for symbol in symbols {
        let container_index = flat.len();
        flat.push(SymbolInformation {
            name: symbol.name,
            kind: symbol.kind,
            tags: symbol.tags,
            deprecated: None,
            location: Location::new(uri.clone(), symbol.selection_range),
            container_name: container.map(str::to_owned),
        });
        if let Some(children) = symbol.children {
            let name = flat[container_index].name.clone();
            flatten(children, uri, Some(&name), flat);
        }
    }
}
