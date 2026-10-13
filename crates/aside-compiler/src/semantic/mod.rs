//! Conservative semantic analysis. Unknown dynamic behavior remains a runtime obligation.
mod bind;
mod forms;
mod link;
mod patterns;
mod project;
mod types;
pub use link::*;
pub mod modules;
use crate::hir::AnalyzedModule;
use crate::{Diagnostic, Limits, Parse, ParseOptions, SourceTooLarge, parse_with_options};
pub use modules::*;

#[derive(Debug, Clone, Copy)]
pub struct AnalysisLimits {
    pub max_nodes: usize,
    pub max_symbols: usize,
    pub max_captures: usize,
    pub max_work: usize,
    pub max_diagnostics: usize,
    pub max_depth: usize,
    pub max_imports: usize,
    pub max_modules: usize,
    pub max_total_source_bytes: usize,
}
impl Default for AnalysisLimits {
    fn default() -> Self {
        Self {
            max_nodes: 1_000_000,
            max_symbols: 200_000,
            max_captures: 200_000,
            max_work: 16_000_000,
            max_diagnostics: 128,
            max_depth: 128,
            max_imports: 10_000,
            max_modules: 10_000,
            max_total_source_bytes: 32 * 1024 * 1024,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalysisContext {
    Module,
    Function,
    Generator,
    NoWait,
}
/// A binding supplied by the real surrounding author scope of a fragment.
#[derive(Debug, Clone)]
pub struct EnclosingBinding {
    pub symbol: ExternalSymbol,
    pub mutable: bool,
}
#[derive(Debug, Clone)]
pub struct AnalysisOptions {
    pub module: String,
    /// Display path retained in source maps, independent of the logical module.
    /// In-memory callers may omit it and use the module name as their source label.
    pub source_path: Option<String>,
    pub version: String,
    pub context: AnalysisContext,
    pub limits: AnalysisLimits,
    /// Explicitly supplied host/prelude environment, never an allow-all fallback.
    pub hosts: Vec<ExternalSymbol>,
    pub enclosing: Vec<EnclosingBinding>,
}
impl Default for AnalysisOptions {
    fn default() -> Self {
        Self {
            module: "main".into(),
            source_path: None,
            version: "source".into(),
            context: AnalysisContext::Module,
            limits: AnalysisLimits::default(),
            hosts: Vec::new(),
            enclosing: Vec::new(),
        }
    }
}
#[derive(Debug, Clone)]
pub struct Analysis {
    module: Option<AnalyzedModule>,
    pub diagnostics: Vec<Diagnostic>,
    pub exhausted: bool,
    pub stats: crate::hir::AnalysisStats,
}
impl Analysis {
    pub fn is_ok(&self) -> bool {
        self.module.is_some() && self.diagnostics.is_empty() && !self.exhausted
    }
    pub fn module(&self) -> Option<&AnalyzedModule> {
        self.module.as_ref()
    }
    pub fn into_module(self) -> Result<AnalyzedModule, AnalysisFailure> {
        match self.module {
            Some(module) => Ok(module),
            None => Err(AnalysisFailure {
                diagnostics: self.diagnostics,
                exhausted: self.exhausted,
            }),
        }
    }
}
#[derive(Debug, Clone)]
pub struct AnalysisFailure {
    pub diagnostics: Vec<Diagnostic>,
    pub exhausted: bool,
}
pub fn analyze(
    parsed: &Parse,
    options: &AnalysisOptions,
    resolver: &mut dyn ModuleResolver,
) -> Analysis {
    if !parsed.is_ok() {
        return Analysis {
            module: None,
            diagnostics: parsed.diagnostics().to_vec(),
            exhausted: parsed.exhausted(),
            stats: crate::hir::AnalysisStats::default(),
        };
    }
    match project::project(parsed, options.limits) {
        Ok(hir) => bind::analyze(hir, options, resolver),
        Err(diagnostic) => Analysis {
            module: None,
            diagnostics: vec![diagnostic],
            exhausted: true,
            stats: crate::hir::AnalysisStats::default(),
        },
    }
}
/// Real source pipeline. Block fragments use their explicit author execution context.
pub fn analyze_source(
    source: &str,
    parse: ParseOptions,
    options: &AnalysisOptions,
    resolver: &mut dyn ModuleResolver,
) -> Result<Analysis, SourceTooLarge> {
    Ok(analyze(
        &parse_with_options(source, parse)?,
        options,
        resolver,
    ))
}
pub fn analyze_module(
    source: &str,
    options: &AnalysisOptions,
    resolver: &mut dyn ModuleResolver,
) -> Result<Analysis, SourceTooLarge> {
    analyze_source(
        source,
        ParseOptions {
            limits: Limits::default(),
            ..Default::default()
        },
        options,
        resolver,
    )
}
