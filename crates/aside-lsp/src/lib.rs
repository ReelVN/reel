//! Aside v1 editor services over the shared lossless and semantic frontend.
//! Host names, types and declaration documents are injected by the embedding tool.
mod host;
mod position;
mod queries;
mod server;
mod workspace;
pub use server::{run_connection, run_stdio};
#[cfg(test)]
mod tests;
pub use host::{
    HostContext, HostDeclaration, HostDocument, HostLocation, HostMember, NamedType, ServerOptions,
    SourceRootQuery,
};
