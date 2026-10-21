//! Adapt the engine's source declarations to the independent Aside language server.
use std::sync::Arc;

use aside_host::source::{Api, constant};
use aside_lsp::{
    HostContext, HostDeclaration, HostDocument, HostLocation, NamedType, ServerOptions,
};

/// Use the same default names, types and source documentation as Reel compilation.
/// Building this context does not create a game session or load game resources.
pub fn options() -> ServerOptions {
    ServerOptions {
        host: host_context(reel_engine::aside::source::api()),
        project_prefix: Some("game".into()),
        source_root: Some(|directory, exists| {
            crate::project::locate_with(directory, exists)
                .ok()
                .map(|project| project.root.join("src"))
        }),
    }
}

/// Project one host API into transport metadata and generated declaration views.
pub fn host_context(api: &Api) -> HostContext {
    let generated = api.documents("reel");
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
                markdown: declaration.documentation,
                definition: Some(HostLocation {
                    uri: location.uri.parse().expect("generated declaration URI"),
                    range: location.range,
                }),
                parameter_defaults: declaration
                    .parameter_defaults
                    .into_iter()
                    .map(|(name, value)| (name, constant(&value)))
                    .collect(),
                property_default: declaration.property_default.as_ref().map(constant),
                property_parameters: declaration.property_parameters,
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
            })
            .collect(),
    }
}
