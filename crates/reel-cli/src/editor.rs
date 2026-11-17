//! Adapt the engine's source declarations to the independent Aside language server.
use std::sync::Arc;

use aside_host::source::{Api, constant};
use aside_lsp::{
    HostContext, HostDeclaration, HostDocument, HostDocumentReference, HostLocation, NamedType,
    ServerOptions,
};
mod translations;

/// Use the same default names, types and source documentation as Reel compilation.
/// Building this context does not create a game session or load game resources.
pub fn options() -> ServerOptions {
    ServerOptions {
        host: host_context(reel_engine::aside::source::api()),
        source_analysis: Some(analyze_source),
        project_prefix: Some("game".into()),
        source_root: Some(|directory, exists| {
            crate::project::locate_with(directory, exists)
                .ok()
                .map(|project| project.root.join("src"))
        }),
    }
}

fn analyze_source(module: aside_compiler::editor::EditorModule<'_>) -> aside_lsp::SourceFeatures {
    use aside_compiler::{
        SyntaxKind, TextRange, TextSize,
        hir::{Form, NodeId},
    };
    use lsp_types::{CompletionItem, CompletionItemKind};

    let mut features = translations::analyze(module);
    let hir = module.hir();
    let source = hir.text(hir.node(NodeId(0)).span);
    for node in &hir.nodes {
        let Form::Call {
            callee,
            arguments: Some(arguments),
            ..
        } = node.form
        else {
            continue;
        };
        let Some(reference) = module.canonical_reference(callee) else {
            continue;
        };
        let Some(export) = reference.export.as_deref() else {
            continue;
        };
        for arg in hir.children(arguments) {
            let mut tokens = hir.tokens(arg);
            let Some(name) = tokens
                .next()
                .filter(|token| token.kind == SyntaxKind::Ident)
            else {
                continue;
            };
            let Some(colon) = tokens
                .next()
                .filter(|token| token.kind == SyntaxKind::Colon)
            else {
                continue;
            };
            let choices = reel_engine::aside::source::literal_argument_choices(
                &reference.module,
                export,
                hir.text(name.span),
            );
            if choices.is_empty() {
                continue;
            }
            let start = colon.span.end();
            let whitespace = source[u32::from(start) as usize..]
                .bytes()
                .take_while(u8::is_ascii_whitespace)
                .count();
            features.completions.push(aside_lsp::SourceCompletions {
                range: TextRange::new(start, start + TextSize::from(whitespace as u32)),
                items: choices
                    .into_iter()
                    .map(|value| CompletionItem {
                        label: format!("\"{value}\""),
                        kind: Some(CompletionItemKind::VALUE),
                        ..Default::default()
                    })
                    .collect(),
            });
        }
    }
    features
}

/// Project one host API into transport metadata and generated declaration views.
pub fn host_context(api: &Api) -> HostContext {
    let generated = api.documents("reel");
    let symbols = generated
        .locations
        .iter()
        .map(|location| Arc::new(location.declaration.symbol.clone()))
        .collect::<Vec<_>>();
    let declarations = generated
        .locations
        .into_iter()
        .map(|location| {
            let declaration = location.declaration;
            let reference = declaration
                .symbol
                .reference
                .expect("generated declarations retain their registered identity");
            HostDeclaration {
                reference: (*reference).clone(),
                signature: declaration.symbol.signature,
                markdown: declaration.documentation,
                definition: Some(HostLocation {
                    uri: location.uri.parse().expect("generated declaration URI"),
                    range: location.range,
                }),
                parameter_defaults: declaration
                    .parameter_defaults
                    .into_iter()
                    .map(|(name, value)| {
                        (
                            name,
                            match value {
                                aside_host::source::ParameterDefault::Value(value) => {
                                    aside_lsp::ParameterDefault::Value(constant(&value))
                                }
                                aside_host::source::ParameterDefault::Omitted => {
                                    aside_lsp::ParameterDefault::Omitted
                                }
                            },
                        )
                    })
                    .collect(),
                property_default: declaration.property_default.as_ref().map(constant),
                property_parameters: declaration.property_parameters,
                parameter_documentation: declaration.parameter_documentation,
                property_groups: declaration
                    .property_groups
                    .into_iter()
                    .map(|group| aside_lsp::PropertyParameterGroup {
                        name: group.name,
                        parameters: group.parameters,
                    })
                    .collect(),
            }
        })
        .collect();
    HostContext {
        globals: api.globals.clone(),
        modules: api.modules.clone(),
        named_types: api
            .types
            .iter()
            .map(|ty| NamedType {
                name: ty.name.clone(),
                bases: ty.bases.clone(),
                call: ty
                    .call
                    .as_ref()
                    .map(|declaration| declaration.symbol.clone()),
                await_result: ty.await_result.clone(),
                members: ty.members.iter().map(|member| member.external()).collect(),
            })
            .collect(),
        declarations,
        documents: generated
            .documents
            .into_iter()
            .map(|document| HostDocument {
                uri: document.uri.parse().expect("generated declaration URI"),
                text: Arc::from(document.text),
                references: document
                    .references
                    .into_iter()
                    .map(|reference| HostDocumentReference {
                        range: reference.range,
                        symbol: Arc::clone(&symbols[reference.declaration]),
                    })
                    .collect(),
            })
            .collect(),
    }
}
