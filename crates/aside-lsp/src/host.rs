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
    /// Source ranges retain the original host identity, independently of the
    /// declaration view's display syntax. Symbols are shared per declaration.
    pub references: Vec<HostDocumentReference>,
}

/// One navigable symbol occurrence in a generated host document.
#[derive(Debug, Clone)]
pub struct HostDocumentReference {
    pub range: TextRange,
    pub symbol: Arc<ExternalSymbol>,
}

/// A displayable literal default or an optional parameter with no source value.
#[derive(Debug, Clone)]
pub enum ParameterDefault {
    /// A literal value displayed with the parameter's real source type.
    Value(String),
    /// Omission is meaningful to the host and has no default source value.
    Omitted,
}

/// Documentation and presentation details for one registered source signature.
#[derive(Debug, Clone)]
pub struct HostDeclaration {
    pub reference: SymbolReference,
    /// A bound member can refine one native's signature for its receiver type.
    /// Definition lookup retains that exact source contract.
    pub signature: Option<aside_compiler::semantic::ExternalSignature>,
    pub markdown: String,
    pub definition: Option<HostLocation>,
    /// Source default semantics, keyed by their actual parameter names.
    pub parameter_defaults: BTreeMap<String, ParameterDefault>,
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

/// A host-specific view of source, derived from the compiler's existing facts.
/// Results are cached with the semantic snapshot and use UTF-8 byte ranges.
#[derive(Debug, Clone, Default)]
pub struct SourceFeatures {
    pub diagnostics: Vec<aside_compiler::Diagnostic>,
    pub hovers: Vec<SourceHover>,
    pub completions: Vec<SourceCompletions>,
}

#[derive(Debug, Clone)]
pub struct SourceHover {
    pub range: TextRange,
    pub markdown: Arc<str>,
}

/// Completion candidates for a host-defined source region. Items supply text
/// or snippets; the language server owns conversion to protocol positions.
#[derive(Debug, Clone)]
pub struct SourceCompletions {
    pub range: TextRange,
    pub items: Arc<[lsp_types::CompletionItem]>,
}

pub type SourceAnalysis = fn(aside_compiler::editor::EditorModule<'_>) -> SourceFeatures;

/// Find one source directory using the host's project convention. The existence
/// query includes open editor buffers, so an unsaved entry can establish a project.
pub type SourceRootQuery = fn(directory: &Path, exists: &dyn Fn(&Path) -> bool) -> Option<PathBuf>;

/// Host configuration supplied before the LSP initialization handshake.
#[derive(Debug, Clone, Default)]
pub struct ServerOptions {
    pub host: HostContext,
    /// Read-only analysis for source constructs owned by the embedding host.
    /// It receives recovered compiler facts and never executes author code.
    pub source_analysis: Option<SourceAnalysis>,
    /// The embedding tool's sole project discovery policy. Files outside a project
    /// use their own directory; untitled buffers remain independent documents.
    pub source_root: Option<SourceRootQuery>,
    /// Optional namespace for imports from the discovered source directory.
    pub project_prefix: Option<String>,
}
