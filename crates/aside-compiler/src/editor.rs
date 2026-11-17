//! Read-only semantic queries over recoverable source. These views cannot be lowered.
use crate::{Diagnostic, Parse, hir::*, semantic::*};

/// Cached source and its original lossless parse, supplied together by an editor.
#[derive(Debug, Clone)]
pub struct EditorSource {
    pub source: SourceModule,
    pub parsed: Parse,
}

/// Project facts use the existing linker, including public reexports and cycles.
#[derive(Debug, Clone)]
pub struct EditorProjectAnalysis {
    pub(crate) modules: Vec<AnalyzedModule>,
    pub diagnostics: Vec<ModuleDiagnostic>,
    pub plan: ModulePlan,
    pub exhausted: bool,
}
impl EditorProjectAnalysis {
    pub fn modules(&self) -> impl Iterator<Item = EditorModule<'_>> {
        self.modules.iter().map(EditorModule)
    }

    /// Find the analyzed module by its canonical path.
    pub fn module(&self, path: &str) -> Option<EditorModule<'_>> {
        let index = self
            .modules
            .binary_search_by(|module| module.canonical_path().cmp(path))
            .ok()?;
        Some(EditorModule(&self.modules[index]))
    }
}

/// Partial facts retained for editor queries, including after source errors.
/// Its private module is deliberately never exposed as a runnable `AnalyzedModule`.
#[derive(Debug, Clone)]
pub struct EditorAnalysis {
    pub(crate) module: Option<AnalyzedModule>,
    pub diagnostics: Vec<Diagnostic>,
    pub exhausted: bool,
    pub stats: AnalysisStats,
}

/// A borrowed editor view. Unlike `AnalyzedModule`, this is not a lowering input.
#[derive(Debug, Clone, Copy)]
pub struct EditorModule<'a>(&'a AnalyzedModule);
impl<'a> From<&'a AnalyzedModule> for EditorModule<'a> {
    fn from(module: &'a AnalyzedModule) -> Self {
        Self(module)
    }
}

impl EditorAnalysis {
    pub fn module(&self) -> Option<EditorModule<'_>> {
        self.module.as_ref().map(EditorModule)
    }
}

impl<'a> EditorModule<'a> {
    /// Reuse the compiler's bound source summaries, including imported records.
    pub fn source_types(self) -> &'a SourceTypes {
        self.0.source_types()
    }
    /// Use the compiler's type spelling without exposing a runnable module.
    pub fn display_type(self, ty: TypeId) -> impl std::fmt::Display + 'a {
        self.0.display_type(ty)
    }
    pub fn canonical_path(self) -> &'a str {
        self.0.canonical_path()
    }
    pub fn source_path(self) -> &'a str {
        self.0.source_path()
    }
    pub fn hir(self) -> &'a Hir {
        self.0.hir()
    }
    pub fn facts(self) -> &'a [NodeFacts] {
        self.0.facts()
    }
    pub fn types(self) -> &'a [Type] {
        self.0.types()
    }
    pub fn scopes(self) -> &'a [Scope] {
        self.0.scopes()
    }
    pub fn symbols(self) -> &'a [Symbol] {
        self.0.symbols()
    }
    pub fn functions(self) -> &'a [Function] {
        self.0.functions()
    }
    pub fn imports(self) -> &'a [Import] {
        self.0.imports()
    }
    pub fn exports(self) -> &'a [SymbolId] {
        self.0.exports()
    }
    pub fn external_bindings(self) -> &'a [ExternalBinding] {
        self.0.external_bindings()
    }
    /// Query external identity without admitting this recoverable view to lowering.
    pub fn canonical_reference(self, node: NodeId) -> Option<&'a SymbolReference> {
        self.0.canonical_reference(node)
    }
}

/// Analyze the original lossless tree while retaining useful facts after errors.
/// Ordinary compilation continues to require successful strict analysis.
pub fn analyze(
    parsed: &Parse,
    options: &AnalysisOptions,
    resolver: &mut dyn ModuleResolver,
) -> EditorAnalysis {
    crate::semantic::editor_analysis(parsed, options, resolver)
}

/// Project public declarations using the same module-linker interface rules.
pub fn module_interface(
    parsed: &Parse,
    module: String,
    version: String,
    limits: AnalysisLimits,
) -> Result<ModuleInterface, Diagnostic> {
    crate::semantic::editor_interface(parsed, module, version, limits)
}

/// Analyze cached project trees with editor recovery while retaining strict
/// compiler admission for normal `analyze_project` callers.
pub fn analyze_project(
    inputs: &[EditorSource],
    options: &AnalysisOptions,
    resolver: &mut dyn ModuleResolver,
) -> Result<EditorProjectAnalysis, crate::SourceTooLarge> {
    crate::semantic::analyze_editor_project(inputs, options, resolver)
}

/// Discover imports from an already parsed, possibly incomplete editor tree.
/// Empty groups and grouped paths ending in `::` retain their module as a
/// discovery hint for completion. These hints do not introduce bound names.
pub fn imports(
    parsed: &Parse,
    module: &str,
    limits: AnalysisLimits,
) -> Result<Vec<(ImportRequest, crate::TextRange)>, Diagnostic> {
    crate::semantic::editor_imports(parsed, module, limits)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unfinished_grouped_paths_discover_editor_modules() {
        for (source, path) in [
            ("use local::api::{};", vec!["local", "api"]),
            ("use local::{api::{ ", vec!["local", "api"]),
            ("use local::{api::", vec!["local", "api"]),
            (
                "use local::{api:: /* choose an export */ };",
                vec!["local", "api"],
            ),
        ] {
            let parsed = crate::parse(source).unwrap();
            let hints = imports(&parsed, "main", AnalysisLimits::default()).unwrap();
            assert_eq!(hints.len(), 1);
            assert_eq!(hints[0].0.path, path);
            assert_eq!(hints[0].0.kind, ImportKind::NamespaceOrSymbol);
        }
        let complete = crate::parse("use local::{api::show};").unwrap();
        let requests = imports(&complete, "main", AnalysisLimits::default()).unwrap();
        assert_eq!(requests[0].0.path, ["local", "api", "show"]);
        assert_eq!(requests[0].0.kind, ImportKind::Symbol);
        let source = "use local::api::{}; fn main(){}";
        let strict =
            crate::semantic::discover_imports(source, "main", AnalysisLimits::default()).unwrap();
        assert!(strict.requests.is_empty());
        let module = crate::analyze_module(source, &AnalysisOptions::default(), &mut NoModules)
            .unwrap()
            .into_module()
            .unwrap();
        assert!(module.imports().is_empty());
    }
    #[test]
    fn duplicate_module_uses_the_accepted_inputs_parse_and_source_path() {
        let inputs = [
            ("current.aside", "fn current() {}"),
            ("duplicate.aside", "fn duplicate() {}"),
        ]
        .map(|(path, source)| EditorSource {
            source: SourceModule {
                canonical_path: "main".into(),
                source_path: Some(path.into()),
                version: "1".into(),
                source: source.into(),
            },
            parsed: crate::parse(source).unwrap(),
        });
        let project =
            analyze_project(&inputs, &AnalysisOptions::default(), &mut NoModules).unwrap();
        assert_eq!(project.diagnostics.len(), 1);
        let module = project.modules().next().unwrap();
        assert_eq!(module.source_path(), "current.aside");
        assert!(
            module
                .symbols()
                .iter()
                .any(|symbol| module.hir().name(symbol.name) == "current")
        );
    }

    #[test]
    fn partial_editor_facts_do_not_admit_invalid_executable_modules() {
        for source in [
            "fn main(){let local=1; missing; local}",
            "fn main(){let local=1; local. }",
        ] {
            let parsed = crate::parse(source).unwrap();
            let options = AnalysisOptions::default();
            assert!(
                crate::analyze(&parsed, &options, &mut NoModules)
                    .into_module()
                    .is_err()
            );
            let editor = analyze(&parsed, &options, &mut NoModules);
            assert!(!editor.diagnostics.is_empty());
            let module = editor
                .module()
                .expect("unaffected declarations remain queryable");
            assert!(
                module
                    .symbols()
                    .iter()
                    .any(|symbol| module.hir().name(symbol.name) == "local")
            );
            let project = analyze_project(
                &[EditorSource {
                    source: SourceModule {
                        canonical_path: "main".into(),
                        source_path: None,
                        version: "1".into(),
                        source: source.into(),
                    },
                    parsed,
                }],
                &options,
                &mut NoModules,
            )
            .unwrap();
            assert!(!project.diagnostics.is_empty());
            assert_eq!(project.modules().count(), 1);
        }
        assert!(
            crate::analyze_module(
                "fn main(){let local=1; local}",
                &AnalysisOptions::default(),
                &mut NoModules
            )
            .unwrap()
            .is_ok()
        );
    }
}
