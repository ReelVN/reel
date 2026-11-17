//! References use bound names and canonical exports from editor analysis.
use std::collections::HashMap;

use aside_compiler::{
    SyntaxKind, TextRange, TextSize,
    editor::EditorModule,
    hir::{Form, NodeId, SymbolId, SymbolKind},
    semantic::{ExternalSymbol, SymbolReference},
};
use lsp_types::{Location, Position, Uri};

use crate::{
    position::Encoding,
    workspace::{Snapshot, Workspace},
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Identity<'a> {
    Binding(SymbolId),
    Export { module: &'a str, name: &'a str },
}

fn export(reference: &SymbolReference) -> Option<Identity<'_>> {
    Some(Identity::Export {
        module: &reference.module,
        name: reference.export.as_deref()?,
    })
}

fn occurrences<'a>(module: EditorModule<'a>, mut visit: impl FnMut(Identity<'a>, TextRange, bool)) {
    let hir = module.hir();
    let mut symbols: Vec<_> = module
        .symbols()
        .iter()
        .enumerate()
        .map(|(index, symbol)| {
            (!matches!(symbol.kind, SymbolKind::Import | SymbolKind::Host))
                .then_some(Identity::Binding(SymbolId(index as u32)))
        })
        .collect();
    for &id in module.exports() {
        let symbol = &module.symbols()[id.0 as usize];
        if symbol.kind != SymbolKind::Import {
            symbols[id.0 as usize] = Some(Identity::Export {
                module: module.canonical_path(),
                name: hir.name(symbol.name),
            });
        }
    }
    let mut nodes = HashMap::new();
    for binding in module.external_bindings() {
        let Some(identity) = binding.descriptor.reference.as_deref().and_then(export) else {
            continue;
        };
        if let Some(id) = binding.symbol
            && matches!(
                module.symbols()[id.0 as usize].kind,
                SymbolKind::Import | SymbolKind::Host
            )
        {
            symbols[id.0 as usize] = Some(identity);
        }
        if let Some(id) = binding.node {
            nodes.insert(id, identity);
        }
    }
    let mut imports = HashMap::new();
    for (symbol, &identity) in module.symbols().iter().zip(&symbols) {
        let Some(identity) = identity else {
            continue;
        };
        match symbol.kind {
            SymbolKind::Import => {
                imports.insert(symbol.declaration, identity);
            }
            SymbolKind::Host | SymbolKind::Receiver | SymbolKind::Super => {}
            _ => visit(identity, symbol.declaration, true),
        }
    }
    let external = |id| {
        module
            .source_types()
            .external(id)
            .and_then(|symbol: &ExternalSymbol| symbol.reference.as_deref())
            .and_then(export)
            .or_else(|| nodes.get(&id).copied())
    };
    for (index, (node, facts)) in hir.nodes.iter().zip(module.facts()).enumerate() {
        let id = NodeId(index as u32);
        let binding = facts.symbol.and_then(|symbol| symbols[symbol.0 as usize]);
        match node.form {
            Form::Name { name } => {
                if let Some(identity) = binding
                    && facts
                        .symbol
                        .is_some_and(|id| module.symbols()[id.0 as usize].declaration != name.span)
                {
                    visit(identity, name.span, false);
                }
            }
            Form::TypeName => {
                let mut names = hir
                    .tokens(id)
                    .filter(|token| token.kind == SyntaxKind::Ident);
                let Some(first) = names.next() else {
                    continue;
                };
                let (name, identity) = match names.last() {
                    Some(last) => (last, external(id)),
                    None => (first, binding.or_else(|| external(id))),
                };
                if let Some(identity) = identity {
                    visit(identity, name.span, false);
                }
            }
            Form::Member { key, .. } => {
                if let Some(identity) = external(id) {
                    visit(identity, key.span, false);
                }
            }
            Form::ImportPath {
                alias,
                group: None,
                wildcard: false,
            } => {
                if let Some(&identity) = imports.get(&node.span) {
                    if let Some(name) = hir
                        .tokens(id)
                        .take_while(|token| token.kind != SyntaxKind::As)
                        .filter(|token| token.kind == SyntaxKind::Ident)
                        .last()
                    {
                        visit(identity, name.span, false);
                    }
                    if let Some(alias) = alias {
                        visit(identity, alias.span, false);
                    }
                }
            }
            _ => {}
        }
    }
}

fn selected(module: EditorModule<'_>, offset: TextSize) -> Option<Identity<'_>> {
    let mut best = None;
    occurrences(module, |identity, span, _| {
        if span.start() <= offset && offset <= span.end() && !span.is_empty() {
            let rank = (span.end() == offset, span.len());
            if best.is_none_or(|(old, _)| rank < old) {
                best = Some((rank, identity));
            }
        }
    });
    best.map(|(_, identity)| identity)
}

fn collect(
    snapshot: &Snapshot,
    identity: Identity<'_>,
    encoding: Encoding,
    current_only: bool,
    declarations: bool,
    locations: &mut Vec<Location>,
) {
    let mut collect_module = |module: EditorModule<'_>| {
        let path = module.canonical_path();
        let source = &snapshot.sources[path];
        occurrences(module, |found, span, declaration| {
            if identity == found && (!declaration || declarations) {
                locations.push(Location::new(
                    source.uri.clone(),
                    source.lines.range(&source.text, span, encoding),
                ));
            }
        });
    };
    if current_only {
        if let Some(module) = snapshot.module() {
            collect_module(module);
        }
    } else {
        snapshot.project.modules().for_each(collect_module);
    }
}

fn sorted(mut locations: Vec<Location>) -> Vec<Location> {
    locations.sort_unstable_by(|left, right| {
        (left.uri.as_str(), left.range.start, left.range.end).cmp(&(
            right.uri.as_str(),
            right.range.start,
            right.range.end,
        ))
    });
    locations.dedup();
    locations
}

impl Workspace {
    pub(crate) fn references(
        &mut self,
        uri: &Uri,
        position: Position,
        encoding: Encoding,
        declarations: bool,
    ) -> Result<Vec<Location>, String> {
        let current = self.snapshot(uri)?;
        let Some(identity) = reference_at(&current, position, encoding)? else {
            return Ok(Vec::new());
        };
        let lexical = matches!(identity, Identity::Binding(_));
        let mut locations = Vec::new();
        collect(
            &current,
            identity,
            encoding,
            lexical,
            declarations,
            &mut locations,
        );
        if !lexical && let Some(project) = self.reference_project(uri, &current)? {
            collect(
                &project,
                identity,
                encoding,
                false,
                declarations,
                &mut locations,
            );
        }
        Ok(sorted(locations))
    }
}

fn reference_at(
    snapshot: &Snapshot,
    position: Position,
    encoding: Encoding,
) -> Result<Option<Identity<'_>>, String> {
    let source = snapshot.source();
    let offset = source
        .lines
        .offset(&source.text, position, encoding)
        .ok_or("invalid reference position")?;
    Ok(snapshot
        .module()
        .and_then(|module| selected(module, (offset as u32).into())))
}
