//! Conservative semantic analysis. Unknown dynamic behavior remains a runtime obligation.
mod bind;
mod builtin_methods;
pub(crate) mod effects;
mod enumerations;
pub use builtin_methods::{
    BuiltinCatalog, BuiltinMember, BuiltinTypeMembers, builtin_catalog, builtin_members,
};
mod forms;
mod link;
mod patterns;
mod project;
mod source_types;
mod type_aliases;
mod types;
pub use link::*;
pub use source_types::{SourceDefinition, SourceMember, SourceShape, SourceType, SourceTypes};
pub mod modules;
use crate::hir::AnalyzedModule;
use crate::{Diagnostic, Limits, Parse, ParseOptions, SourceTooLarge, parse_with_options};
pub use modules::*;
pub use types::BUILTIN_TYPE_NAMES;

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
            max_nodes: u32::MAX as usize,
            max_symbols: u32::MAX as usize,
            max_captures: u32::MAX as usize,
            max_work: usize::MAX,
            max_diagnostics: 128,
            max_depth: 128,
            max_imports: u32::MAX as usize,
            max_modules: u32::MAX as usize,
            max_total_source_bytes: isize::MAX as usize,
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

pub(crate) fn editor_analysis(
    parsed: &Parse,
    options: &AnalysisOptions,
    resolver: &mut dyn ModuleResolver,
) -> crate::editor::EditorAnalysis {
    match project::project_editor(parsed, options.limits) {
        Ok(hir) => {
            let mut result = bind::analyze_editor(hir, options, resolver);
            infer_source_types(&mut result, options, resolver);
            result
                .diagnostics
                .splice(0..0, parsed.diagnostics().iter().cloned());
            result.exhausted |= parsed.exhausted();
            result
        }
        Err(diagnostic) => crate::editor::EditorAnalysis {
            module: None,
            diagnostics: parsed
                .diagnostics()
                .iter()
                .cloned()
                .chain([diagnostic])
                .collect(),
            exhausted: true,
            stats: Default::default(),
        },
    }
}

fn needs_source_types(module: &AnalyzedModule) -> bool {
    !module.inferred_awaits.is_empty()
        || module
            .facts()
            .iter()
            .any(|facts| facts.intrinsic == Some(Intrinsic::ResumeFlow))
}

fn infer_source_types(
    analysis: &mut crate::editor::EditorAnalysis,
    options: &AnalysisOptions,
    resolver: &mut dyn ModuleResolver,
) {
    if analysis.exhausted {
        return;
    }
    let Some(module) = &mut analysis.module else {
        return;
    };
    let result = source_types::analyze(
        std::slice::from_mut(module),
        resolver,
        AnalysisLimits {
            max_work: options
                .limits
                .max_work
                .saturating_sub(analysis.stats.work_units),
            ..options.limits
        },
    );
    let work = match result {
        Ok(work) => {
            let remaining = options
                .limits
                .max_diagnostics
                .max(1)
                .saturating_sub(analysis.diagnostics.len());
            analysis
                .diagnostics
                .extend(inferred_await_diagnostics(module).take(remaining));
            work
        }
        Err(work) => {
            if work
                > options
                    .limits
                    .max_work
                    .saturating_sub(analysis.stats.work_units)
            {
                analysis.exhausted = true;
                analysis.diagnostics.push(Diagnostic::error(
                    crate::DiagnosticCode::BudgetExceeded,
                    module.hir.node(crate::hir::NodeId(0)).span,
                    "source type inference work budget exceeded",
                ));
            }
            work
        }
    };
    analysis.stats.work_units = analysis.stats.work_units.saturating_add(work);
    module.stats.work_units = analysis.stats.work_units;
}

fn inferred_await_diagnostics(module: &AnalyzedModule) -> impl Iterator<Item = Diagnostic> + '_ {
    fn non_awaitable(ty: &TypeHint) -> bool {
        match ty {
            TypeHint::Union(cases) => !cases.is_empty() && cases.iter().all(non_awaitable),
            TypeHint::Any
            | TypeHint::Named(_)
            | TypeHint::Future
            | TypeHint::Closure
            | TypeHint::Callable(..) => false,
            _ => true,
        }
    }
    module.inferred_awaits.iter().filter_map(|&node| {
        let crate::hir::Form::Unary { value, .. } = module.hir.node(node).form else {
            unreachable!("await candidate must be a prefix expression");
        };
        let result = module.source_types.value(value)?;
        non_awaitable(&result.ty).then(|| invalid_await(&module.hir, node))
    })
}

fn invalid_await(hir: &crate::hir::Hir, node: crate::hir::NodeId) -> Diagnostic {
    let crate::hir::Form::Unary { operator, value } = hir.node(node).form else {
        unreachable!("await must be a prefix expression");
    };
    let between = crate::TextRange::new(operator.span.end(), hir.node(value).span.start());
    let spaces = hir
        .text(between)
        .bytes()
        .take_while(|byte| matches!(byte, b' ' | b'\t'))
        .count();
    let mut diagnostic = Diagnostic::error(
        crate::DiagnosticCode::TypeMismatch,
        operator.span,
        "await requires an Awaitable. Use the result directly",
    );
    diagnostic.fix = Some(crate::diagnostic::Fix {
        range: crate::TextRange::new(
            operator.span.start(),
            operator.span.end() + crate::TextSize::from(spaces as u32),
        ),
        replacement: String::new(),
    });
    diagnostic
}

pub(crate) fn editor_interface(
    parsed: &Parse,
    module: String,
    version: String,
    limits: AnalysisLimits,
) -> Result<ModuleInterface, Diagnostic> {
    let hir = project::project_editor(parsed, limits)?;
    let exports = link::declarations(&hir, &module, &version);
    Ok(ModuleInterface {
        canonical_path: module,
        version,
        exports,
    })
}

pub(crate) fn editor_imports(
    parsed: &Parse,
    module: &str,
    limits: AnalysisLimits,
) -> Result<Vec<(ImportRequest, crate::TextRange)>, Diagnostic> {
    use crate::hir::{HirKind, NodeId};
    let hir = project::project_editor(parsed, limits)?;
    let mut requests = Vec::new();
    for (index, node) in hir
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.kind == HirKind::Import)
    {
        if let Some(path) = hir.children(NodeId(index as u32)).next() {
            let mut imports = Vec::new();
            link::import_requests(&hir, path, &[], false, true, module, &mut imports);
            requests.extend(imports.into_iter().map(|(request, _)| (request, node.span)));
        }
    }
    Ok(requests)
}
