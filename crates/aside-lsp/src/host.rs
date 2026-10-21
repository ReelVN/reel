//! Optional host metadata. All semantic signatures come from the compiler contract.
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use aside_compiler::{
    TextRange,
    semantic::{ExternalSymbol, ModuleInterface, SymbolReference},
};
use lsp_types::Uri;
pub use aside_compiler::semantic::ExternalMember as HostMember;

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
    /// A property's real registered default, when it has one.
    pub property_default: Option<String>,
}

/// Members of a real named host type used by annotations and return values.
#[derive(Debug, Clone)]
pub struct NamedType {
    pub name: String,
    /// Canonical supertypes from the same trusted host registration.
    pub bases: Vec<String>,
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

/// Map a logical import prefix to a source directory.
/// Empty prefixes map standalone project modules relative to the directory.
#[derive(Debug, Clone)]
pub struct ModuleRoot {
    pub prefix: String,
    pub path: PathBuf,
}

/// Host configuration supplied before the LSP initialization handshake.
#[derive(Debug, Clone, Default)]
pub struct ServerOptions {
    pub host: HostContext,
    pub module_roots: Vec<ModuleRoot>,
    /// Optional host project namespace mapped to each file's nearest `main.aside`.
    /// Standalone Aside has no implicit host namespace.
    pub project_prefix: Option<String>,
}
