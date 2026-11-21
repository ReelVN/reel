//! Adapt Reel's source declarations for native and browser editors.
use std::{collections::BTreeMap, sync::Arc};

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
        ..Default::default()
    }
}

fn analyze_source(module: aside_compiler::editor::EditorModule<'_>) -> aside_lsp::SourceFeatures {
    let mut features = translations::analyze(module);
    features
        .diagnostics
        .extend(reel_engine::aside::source::audio_diagnostics(module));
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
            let mut argument_choices = BTreeMap::new();
            if let (Some(signature), Some(export)) =
                (&declaration.symbol.signature, reference.export.as_deref())
            {
                for parameter in &signature.parameters {
                    let choices = reel_engine::aside::source::literal_argument_choices(
                        &reference.module,
                        export,
                        &parameter.name,
                    );
                    if !choices.is_empty() {
                        argument_choices.insert(
                            parameter.name.clone(),
                            choices.into_iter().map(str::to_owned).collect(),
                        );
                    }
                }
            }
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
                argument_choices,
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
