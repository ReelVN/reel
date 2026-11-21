//! Aside v1 editor services over the shared lossless and semantic frontend.
//! Host names, types and declaration documents are injected by the embedding tool.
mod document;
mod host;
mod position;
mod queries;
mod references;
mod semantic_tokens;
mod server;
mod syntax_queries;
#[cfg(test)]
mod tests;
mod workspace;
pub use document::DocumentAnalysis;
pub use host::{
    HostContext, HostDeclaration, HostDocument, HostDocumentReference, HostLocation, HostMember,
    NamedType, ParameterDefault, PropertyParameterGroup, ServerOptions, SourceAnalysis,
    SourceCompletions, SourceFeatures, SourceHover, SourceRootQuery,
};
pub use server::{run_connection, run_stdio};
