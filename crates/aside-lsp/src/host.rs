//! Optional host metadata. All semantic signatures come from the compiler contract.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

pub use aside_compiler::semantic::ExternalMember as HostMember;
use aside_compiler::{
    TextRange,
    semantic::{ExternalSymbol, ModuleInterface, SymbolReference},
};
use lsp_types::Uri;

/// A byte range in a generated, read-only declaration document.
#[derive(Debug, Clone)]
pub struct HostLocation {
    pub uri: Uri,
    pub range: TextRange,
}

/// An immutable declaration view generated from a host's real Rust declarations.
/// The text is displayed as Aside, without being executed or imported as source.
#[derive(Debug, Clone)]
pub struct HostDocument {
    pub uri: Uri,
    pub text: Arc<str>,
}

/// Documentation and presentation details for one canonical registered symbol.
/// Types and parameter names remain on its `ExternalSymbol`/`ExternalSignature`.
#[derive(Debug, Clone)]
pub struct HostDeclaration {
    pub reference: SymbolReference,
    pub markdown: String,
    pub definition: Option<HostLocation>,
    /// Literal source defaults, keyed by their actual parameter names.
    pub parameter_defaults: BTreeMap<String, String>,
    /// Parameters contributed by the real property schema. Hover may summarize
    /// optional entries; completion and signature help retain the full contract.
    pub property_parameters: Vec<String>,
    /// A property's real registered default, when it has one.
    pub property_default: Option<String>,
}

/// Members of a real named host type used by annotations and return values.
#[derive(Debug, Clone)]
pub struct NamedType {
    pub name: String,
    /// Canonical supertypes from the same trusted host registration.
    pub bases: Vec<String>,
    /// A named value's real callable contract, including its declaration identity.
    pub call: Option<ExternalSymbol>,
    /// Result of its registered await protocol; absent when no result is known.
    pub await_result: Option<aside_compiler::semantic::TypeHint>,
    pub members: Vec<HostMember>,
}

/// An embedding host's explicit analysis and editor environment.
/// An empty context is standalone Aside and introduces no game-specific names.
#[derive(Debug, Clone, Default)]
pub struct HostContext {
    pub globals: Vec<ExternalSymbol>,
    pub modules: Vec<ModuleInterface>,
    pub named_types: Vec<NamedType>,
    pub declarations: Vec<HostDeclaration>,
    pub documents: Vec<HostDocument>,
}

/// Find one source directory using the host's project convention. The existence
/// query includes open editor buffers, so an unsaved entry can establish a project.
pub type SourceRootQuery = fn(directory: &Path, exists: &dyn Fn(&Path) -> bool) -> Option<PathBuf>;

/// Host configuration supplied before the LSP initialization handshake.
#[derive(Debug, Clone, Default)]
pub struct ServerOptions {
    pub host: HostContext,
    /// The embedding tool's sole project discovery policy. Files outside a project
    /// use their own directory; untitled buffers remain independent documents.
    pub source_root: Option<SourceRootQuery>,
    /// Optional namespace for imports from the discovered source directory.
    pub project_prefix: Option<String>,
}
