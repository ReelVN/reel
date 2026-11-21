//! Source-module linking. Imports are resolved before initialization; legal SCCs
//! are retained, with initialized-state runtime checks on imported value reads.
use super::types::type_hint;
use super::*;
use crate::hir::{Form, Hir, HirElement, HirKind as H, NameId, NodeId};
use crate::{DiagnosticCode as D, SyntaxKind as K, TextRange, TextSize};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
};

#[derive(Debug, Clone)]
pub struct SourceModule {
    pub canonical_path: String,
    pub source_path: Option<String>,
    pub version: String,
    pub source: Arc<str>,
}

#[derive(Debug)]
pub struct ImportDiscovery {
    pub requests: Vec<(ImportRequest, TextRange)>,
    pub diagnostics: Vec<Diagnostic>,
}
/// File providers discover dependencies through the same parsed import forms
/// as analysis. All syntax diagnostics are preserved; only import subtrees enter
/// the dependency projection. Full analysis separately admits the source bodies.
/// This performs no IO and does not manufacture module interfaces.
pub fn discover_imports(
    source: &str,
    module: &str,
    limits: AnalysisLimits,
) -> Result<ImportDiscovery, SourceTooLarge> {
    let parsed = crate::parse(source)?;
    if !parsed.is_ok() {
        return Ok(ImportDiscovery {
            requests: Vec::new(),
            diagnostics: parsed.diagnostics().to_vec(),
        });
    }
    if parsed.import_offsets().is_empty() && limits.max_nodes != 0 {
        return Ok(ImportDiscovery {
            requests: Vec::new(),
            diagnostics: Vec::new(),
        });
    }
    let hir = match super::project::project_imports(&parsed, source, limits) {
        Ok(hir) => hir,
        Err(diagnostic) => {
            return Ok(ImportDiscovery {
                requests: Vec::new(),
                diagnostics: vec![diagnostic],
            });
        }
    };
    let mut requests = Vec::new();
    for (index, node) in hir
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.kind == H::Import)
    {
        let Some(path) = hir.children(NodeId(index as u32)).next() else {
            continue;
        };
        let mut imports = Vec::new();
        import_requests(&hir, path, &[], false, false, module, &mut imports);
        requests.extend(imports.into_iter().map(|(request, _)| (request, node.span)));
    }
    Ok(ImportDiscovery {
        requests,
        diagnostics: Vec::new(),
    })
}
#[derive(Debug, Clone)]
pub struct ModuleDiagnostic {
    pub module: String,
    pub diagnostic: Diagnostic,
}
#[derive(Debug, Clone, Default)]
pub struct ModulePlan {
    /// Value dependencies precede users; each SCC is sorted by canonical path.
    pub components: Vec<Vec<String>>,
    pub initialization_order: Vec<String>,
}
#[derive(Debug, Clone)]
pub struct ProjectAnalysis {
    modules: Option<Vec<crate::hir::AnalyzedModule>>,
    pub diagnostics: Vec<ModuleDiagnostic>,
    pub plan: ModulePlan,
    pub exhausted: bool,
}
impl ProjectAnalysis {
    pub fn into_modules(self) -> Result<Vec<crate::hir::AnalyzedModule>, Self> {
        if self.is_ok() {
            Ok(self.modules.unwrap())
        } else {
            Err(self)
        }
    }
    pub fn is_ok(&self) -> bool {
        self.modules.is_some() && self.diagnostics.is_empty() && !self.exhausted
    }
    pub fn modules(&self) -> Option<&[crate::hir::AnalyzedModule]> {
        self.modules.as_deref()
    }
}
struct Overlay<'a> {
    memory: &'a mut MemoryResolver,
    fallback: &'a mut dyn ModuleResolver,
    aliases: &'a BTreeMap<String, TypeHint>,
}
impl ModuleResolver for Overlay<'_> {
    fn type_alias(&self, canonical_name: &str) -> Option<TypeHint> {
        self.aliases
            .get(canonical_name)
            .cloned()
            .or_else(|| self.fallback.type_alias(canonical_name))
    }
    fn enum_type(&self, canonical_name: &str) -> Option<TypeHint> {
        self.memory
            .enum_type(canonical_name)
            .or_else(|| self.fallback.enum_type(canonical_name))
    }
    fn has_closed_members(&self, type_name: &str) -> bool {
        let mut pending = VecDeque::from([type_name]);
        let mut visited = BTreeSet::new();
        while let Some(name) = pending.pop_front() {
            if !visited.insert(name) {
                continue;
            }
            let class =
                name.rsplit_once("::")
                    .and_then(|(module, name)| {
                        self.memory.modules.get(module).and_then(|module| {
                            module.exports.iter().find(|symbol| symbol.name == name)
                        })
                    })
                    .and_then(|symbol| symbol.class.as_ref());
            if let Some(class) = class {
                pending.extend(class.bases.iter().map(String::as_str));
            } else if !self.fallback.has_closed_members(name) {
                return false;
            }
        }
        true
    }
    fn resolve_type_member(&mut self, type_name: &str, member: &str) -> Option<ExternalMember> {
        let mut pending = VecDeque::from([type_name.to_owned()]);
        let mut visited = BTreeSet::new();
        while let Some(name) = pending.pop_front() {
            if !visited.insert(name.clone()) {
                continue;
            }
            let class =
                name.rsplit_once("::")
                    .and_then(|(module, name)| {
                        self.memory.modules.get(module).and_then(|module| {
                            module.exports.iter().find(|symbol| symbol.name == name)
                        })
                    })
                    .and_then(|symbol| symbol.class.as_ref());
            if let Some(class) = class {
                if let Some(found) = class
                    .members
                    .iter()
                    .find(|entry| entry.symbol.name == member)
                {
                    return Some(found.clone());
                }
                pending.extend(class.bases.iter().cloned());
            } else if let Some(member) = self.fallback.resolve_type_member(&name, member) {
                return Some(member);
            }
        }
        None
    }
    fn is_named_subtype(&self, actual: &str, expected: &str) -> Option<bool> {
        self.fallback.is_named_subtype(actual, expected)
    }
    fn resolve_type_call(&mut self, name: &str) -> Option<ExternalSymbol> {
        self.fallback.resolve_type_call(name)
    }
    fn resolve_type_await(&mut self, name: &str) -> Option<TypeHint> {
        self.fallback.resolve_type_await(name)
    }
    fn resolve(&mut self, r: &ImportRequest) -> Result<ResolvedImport, ResolveError> {
        let r = self.fallback.canonical_request(r);
        // A private/missing export in a known source module cannot be bypassed
        // by accidentally asking the filesystem/host resolver a second time.
        let full = r.path.join("::");
        let parent = r.path.split_last().map(|(_, p)| p.join("::"));
        if self.memory.modules.contains_key(&full)
            || parent
                .as_ref()
                .is_some_and(|p| self.memory.modules.contains_key(p))
        {
            self.memory.resolve(&r)
        } else {
            self.fallback.resolve(&r)
        }
    }
}
pub fn analyze_project(
    inputs: &[SourceModule],
    options: &AnalysisOptions,
    resolver: &mut dyn ModuleResolver,
) -> Result<ProjectAnalysis, SourceTooLarge> {
    analyze_project_with_progress(inputs, options, resolver, &mut |_| {})
}

/// Run the same pipeline while reporting completed stages and modules.
pub fn analyze_project_with_progress(
    inputs: &[SourceModule],
    options: &AnalysisOptions,
    resolver: &mut dyn ModuleResolver,
    progress: &mut dyn FnMut(crate::Progress),
) -> Result<ProjectAnalysis, SourceTooLarge> {
    let result = analyze_project_inner(
        inputs.iter().map(|input| (input, None)),
        options,
        resolver,
        false,
        progress,
    )?;
    Ok(ProjectAnalysis {
        modules: if result.diagnostics.is_empty() && !result.exhausted {
            Some(result.modules)
        } else {
            None
        },
        diagnostics: result.diagnostics,
        plan: result.plan,
        exhausted: result.exhausted,
    })
}

pub(crate) fn analyze_editor_project(
    inputs: &[crate::editor::EditorSource],
    options: &AnalysisOptions,
    resolver: &mut dyn ModuleResolver,
) -> Result<crate::editor::EditorProjectAnalysis, SourceTooLarge> {
    analyze_project_inner(
        inputs
            .iter()
            .map(|input| (&input.source, Some(&input.parsed))),
        options,
        resolver,
        true,
        &mut |_| {},
    )
}

fn analyze_project_inner<'a>(
    inputs: impl ExactSizeIterator<Item = (&'a SourceModule, Option<&'a Parse>)> + Clone,
    options: &AnalysisOptions,
    resolver: &mut dyn ModuleResolver,
    recover: bool,
    progress: &mut dyn FnMut(crate::Progress),
) -> Result<crate::editor::EditorProjectAnalysis, SourceTooLarge> {
    let mut diagnostics = Vec::new();
    let mut projected = BTreeMap::new();
    let mut memory = MemoryResolver::default();
    let mut aliases = BTreeMap::new();
    let mut exhausted = false;
    let diagnostic_limit = options.limits.max_diagnostics.max(1);
    let total = inputs.len();
    if total > options.limits.max_modules
        || inputs.clone().fold(0usize, |total, (input, _)| {
            total.saturating_add(input.source.len())
        }) > options.limits.max_total_source_bytes
    {
        diagnostics.push(ModuleDiagnostic {
            module: options.module.clone(),
            diagnostic: Diagnostic::error(
                D::BudgetExceeded,
                TextRange::empty(TextSize::from(0)),
                "project module budget exceeded",
            ),
        });
        return Ok(crate::editor::EditorProjectAnalysis {
            modules: Vec::new(),
            diagnostics,
            plan: ModulePlan::default(),
            exhausted: true,
        });
    }
    let mut total_nodes = 0usize;
    for (completed, (input, parsed)) in inputs.enumerate() {
        progress(crate::Progress {
            stage: crate::ProgressStage::Parse,
            completed,
            total,
        });
        if projected.contains_key(&input.canonical_path) {
            diagnostics.push(ModuleDiagnostic {
                module: input.canonical_path.clone(),
                diagnostic: Diagnostic::error(
                    D::ModuleResolution,
                    TextRange::empty(TextSize::from(0)),
                    "duplicate canonical module path",
                ),
            });
            diagnostics.truncate(diagnostic_limit);
            continue;
        }
        let owned_parse;
        let parsed = match parsed {
            Some(parsed) => parsed,
            None => {
                owned_parse = crate::parse(&input.source)?;
                &owned_parse
            }
        };
        if !parsed.is_ok() {
            exhausted |= parsed.exhausted();
            diagnostics.extend(parsed.diagnostics().iter().cloned().map(|diagnostic| {
                ModuleDiagnostic {
                    module: input.canonical_path.clone(),
                    diagnostic,
                }
            }));
            diagnostics.truncate(diagnostic_limit);
            if !recover {
                continue;
            }
        }
        let project = if recover {
            super::project::project_editor
        } else {
            super::project::project
        };
        match project(
            parsed,
            AnalysisLimits {
                max_nodes: options.limits.max_nodes.saturating_sub(total_nodes),
                ..options.limits
            },
        ) {
            Err(diagnostic) => {
                exhausted = true;
                diagnostics.push(ModuleDiagnostic {
                    module: input.canonical_path.clone(),
                    diagnostic,
                });
            }
            Ok(hir) => {
                total_nodes += hir.nodes.len();
                memory.insert(ModuleInterface {
                    canonical_path: input.canonical_path.clone(),
                    version: input.version.clone(),
                    exports: declarations(&hir, &input.canonical_path, &input.version),
                });
                projected.insert(input.canonical_path.clone(), (hir, input));
            }
        }
    }
    progress(crate::Progress {
        stage: crate::ProgressStage::Parse,
        completed: total,
        total,
    });
    let analysis_total = projected.len();
    progress(crate::Progress {
        stage: crate::ProgressStage::Analyze,
        completed: 0,
        total: analysis_total,
    });
    diagnostics.truncate(diagnostic_limit);
    // Public reexports form monotone interface equations. Cache import paths
    // once and revisit only users of changed interfaces, including legal cycles.
    let mut requests = BTreeMap::new();
    let mut users: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (path, (hir, _)) in &projected {
        let mut module_requests = Vec::new();
        for n in hir.children(NodeId(0)) {
            if hir.node(n).kind != H::ModifiedDeclaration
                || !hir.tokens(n).any(|t| t.kind == K::Pub)
            {
                continue;
            }
            let Some(import) = hir
                .children(n)
                .next()
                .filter(|&n| hir.node(n).kind == H::Import)
            else {
                continue;
            };
            if let Some(use_path) = hir.children(import).next() {
                import_requests(hir, use_path, &[], false, false, path, &mut module_requests);
            }
        }
        for (request, _) in &mut module_requests {
            *request = resolver.canonical_request(request).into_owned();
        }
        for (request, _) in &module_requests {
            for target in memory.dependency_modules(request) {
                users.entry(target).or_default().insert(path.clone());
            }
        }
        requests.insert(path.clone(), module_requests);
    }
    let mut exported_names: BTreeMap<_, BTreeSet<_>> = memory
        .modules
        .iter()
        .map(|(path, module)| {
            (
                path.clone(),
                module.exports.iter().map(|e| e.name.clone()).collect(),
            )
        })
        .collect();
    let mut queue: VecDeque<_> = projected.keys().cloned().collect();
    let mut queued: BTreeSet<_> = projected.keys().cloned().collect();
    let mut work = 0usize;
    while let Some(path) = queue.pop_front() {
        queued.remove(&path);
        let mut changed = false;
        for (request, alias) in &requests[&path] {
            work += 1;
            if work > options.limits.max_work {
                exhausted = true;
                break;
            }
            let result = Overlay {
                memory: &mut memory,
                fallback: resolver,
                aliases: &aliases,
            }
            .resolve(request);
            if let Ok(resolved) = result.map(ResolvedImport::retain_references) {
                let module = memory.modules.get_mut(&path).unwrap();
                for mut symbol in resolved.bindings {
                    if let Some(alias) = alias {
                        symbol.name = alias.clone();
                    }
                    if exported_names
                        .get_mut(&path)
                        .unwrap()
                        .insert(symbol.name.clone())
                    {
                        symbol.resolved_at(&module.canonical_path, &module.version, false);
                        module.exports.push(symbol);
                        changed = true;
                    }
                }
            }
        }
        if exhausted {
            break;
        }
        if changed && let Some(dependents) = users.get(&path) {
            for user in dependents {
                if queued.insert(user.clone()) {
                    queue.push_back(user.clone());
                }
            }
        }
    }
    if exhausted && diagnostics.is_empty() {
        diagnostics.push(ModuleDiagnostic {
            module: options.module.clone(),
            diagnostic: Diagnostic::error(
                D::BudgetExceeded,
                TextRange::empty(TextSize::from(0)),
                "module link budget exceeded",
            ),
        });
    }
    // Resolve annotations in the defining module, before consumers see aliases
    // or reexports. The same canonical names feed compiler and editor queries.
    let prelude_types: BTreeMap<_, _> = options
        .hosts
        .iter()
        .filter_map(|symbol| match &symbol.ty {
            TypeHint::Named(name) if symbol.type_only => {
                Some((symbol.name.as_str(), name.as_str()))
            }
            _ => None,
        })
        .collect();
    let mut type_names = BTreeMap::new();
    for (path, (hir, _)) in &projected {
        let mut names = BTreeMap::new();
        for node in hir.children(NodeId(0)) {
            let declaration = match hir.node(node).form {
                Form::Modified { declaration, .. } => declaration,
                _ => node,
            };
            if let Some(name) = match hir.node(declaration).form {
                Form::Class { name, .. } => name,
                Form::TypeAlias { name, .. } => Some(name),
                _ if hir.node(declaration).kind == H::Enum => {
                    hir.tokens(declaration).find(|token| token.name.is_some())
                }
                _ => None,
            } {
                names.insert(
                    hir.text(name.span).to_owned(),
                    format!("{path}::{}", hir.text(name.span)),
                );
            }
        }
        for (index, _) in hir
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.kind == H::Import)
        {
            let Some(import) = hir.children(NodeId(index as u32)).next() else {
                continue;
            };
            let mut imports = Vec::new();
            import_requests(hir, import, &[], false, false, path, &mut imports);
            for (request, alias) in imports {
                if let Ok(resolved) = (Overlay {
                    memory: &mut memory,
                    fallback: resolver,
                    aliases: &aliases,
                })
                .resolve(&request)
                {
                    for symbol in resolved.bindings {
                        let name = alias.clone().unwrap_or_else(|| symbol.name.clone());
                        if let Some(namespace) = symbol.namespace {
                            names.insert(name, namespace.module);
                        } else if matches!(symbol.ty, TypeHint::Named(_) | TypeHint::Enum { .. })
                            && let Some(reference) = symbol.reference
                            && let Some(export) = &reference.export
                        {
                            names.insert(name, format!("{}::{export}", reference.module));
                        }
                    }
                }
            }
        }
        type_names.insert(path.clone(), names);
    }
    for (path, (hir, _)) in &projected {
        let names = TypeNames {
            module: &type_names[path],
            prelude: &prelude_types,
        };
        for node in hir.children(NodeId(0)) {
            let declaration = match hir.node(node).form {
                Form::Modified { declaration, .. } => declaration,
                _ => node,
            };
            if let Form::TypeAlias { name, target } = hir.node(declaration).form {
                let mut hint = type_hint(hir, target);
                canonical_hint(&mut hint, &|name| names.canonical(name));
                aliases.insert(format!("{path}::{}", hir.text(name.span)), hint);
            }
        }
    }
    for module in memory.modules.values_mut() {
        for symbol in &mut module.exports {
            let origin = symbol
                .reference
                .as_ref()
                .map_or(module.canonical_path.as_str(), |reference| {
                    reference.module.as_str()
                });
            if let Some(names) = type_names.get(origin) {
                canonical_symbol(symbol, &|name| {
                    TypeNames {
                        module: names,
                        prelude: &prelude_types,
                    }
                    .canonical(name)
                });
            }
            if symbol.intrinsic == Some(Intrinsic::TypeAlias) {
                aliases
                    .entry(format!("{}::{}", module.canonical_path, symbol.name))
                    .or_insert_with(|| symbol.ty.clone());
            }
        }
    }
    let mut constructors = Vec::new();
    for (path, module) in &memory.modules {
        for (index, symbol) in module.exports.iter().enumerate() {
            let Some(class) = &symbol.class else {
                continue;
            };
            if class
                .members
                .iter()
                .any(|member| member.symbol.name == "init")
            {
                continue;
            }
            let mut pending: VecDeque<_> = class.bases.iter().cloned().collect();
            let mut visited = BTreeSet::new();
            while let Some(base) = pending.pop_front() {
                if !visited.insert(base.clone()) {
                    continue;
                }
                let Some(class) = base
                    .rsplit_once("::")
                    .and_then(|(path, name)| {
                        memory.modules.get(path).and_then(|module| {
                            module.exports.iter().find(|symbol| symbol.name == name)
                        })
                    })
                    .and_then(|symbol| symbol.class.as_ref())
                else {
                    continue;
                };
                if let Some(mut signature) = class
                    .members
                    .iter()
                    .find(|member| member.symbol.name == "init")
                    .and_then(|member| member.symbol.signature.clone())
                {
                    signature.result = symbol.ty.clone();
                    constructors.push((path.clone(), index, signature));
                    break;
                }
                pending.extend(class.bases.iter().cloned());
            }
        }
    }
    for (path, index, signature) in constructors {
        memory.modules.get_mut(&path).unwrap().exports[index].signature = Some(signature);
    }
    let mut modules = Vec::new();
    let mut dependencies = BTreeMap::new();
    if !exhausted {
        let mut symbols = 0usize;
        let mut captures = 0usize;
        let mut imports = 0usize;
        for (completed, (path, (hir, input))) in projected.into_iter().enumerate() {
            let local = AnalysisOptions {
                module: path.clone(),
                source_path: input.source_path.clone(),
                context: AnalysisContext::Module,
                version: memory.modules[&path].version.clone(),
                limits: AnalysisLimits {
                    max_work: options.limits.max_work.saturating_sub(work),
                    max_symbols: options.limits.max_symbols.saturating_sub(symbols),
                    max_captures: options.limits.max_captures.saturating_sub(captures),
                    max_imports: options.limits.max_imports.saturating_sub(imports),
                    ..options.limits
                },
                ..options.clone()
            };
            let result = super::bind::analyze_editor(
                hir,
                &local,
                &mut Overlay {
                    memory: &mut memory,
                    fallback: resolver,
                    aliases: &aliases,
                },
            );
            work = work.saturating_add(result.stats.work_units);
            symbols += result.stats.symbols;
            captures += result.stats.captures;
            imports += result.stats.imports;
            exhausted |= result.exhausted;
            let valid = result.diagnostics.is_empty() && !result.exhausted;
            diagnostics.extend(
                result
                    .diagnostics
                    .into_iter()
                    .map(|diagnostic| ModuleDiagnostic {
                        module: path.clone(),
                        diagnostic,
                    }),
            );
            diagnostics.truncate(diagnostic_limit);
            if let Some(module) = result.module.filter(|_| recover || valid) {
                let dependency_work = module
                    .facts()
                    .len()
                    .saturating_add(module.symbols().len())
                    .saturating_add(module.exports().len())
                    .saturating_add(
                        module
                            .imports()
                            .iter()
                            .map(|i| i.names.len())
                            .sum::<usize>(),
                    );
                work = work.saturating_add(dependency_work);
                if work > options.limits.max_work {
                    exhausted = true;
                    diagnostics.push(ModuleDiagnostic {
                        module: path,
                        diagnostic: Diagnostic::error(
                            D::BudgetExceeded,
                            module.hir().node(NodeId(0)).span,
                            "project analysis work budget exceeded",
                        ),
                    });
                    diagnostics.truncate(diagnostic_limit);
                    break;
                }
                dependencies.insert(path, initialization_dependencies(&module));
                modules.push(module);
            }
            progress(crate::Progress {
                stage: crate::ProgressStage::Analyze,
                completed: completed + 1,
                total: analysis_total,
            });
        }
    }
    if !exhausted && (recover || modules.iter().any(super::needs_source_types)) {
        let remaining = options.limits.max_work.saturating_sub(work);
        let result = super::source_types::analyze(
            &mut modules,
            &mut Overlay {
                memory: &mut memory,
                fallback: resolver,
                aliases: &aliases,
            },
            AnalysisLimits {
                max_work: options.limits.max_work.saturating_sub(work),
                ..options.limits
            },
        );
        let inferred_work = match result {
            Ok(work) | Err(work) => work,
        };
        if result.is_ok() {
            for module in &mut modules {
                module.stats.work_units = module
                    .stats
                    .work_units
                    .saturating_add(module.source_types.work_units());
                let remaining = diagnostic_limit.saturating_sub(diagnostics.len());
                diagnostics.extend(
                    super::inferred_await_diagnostics(module)
                        .take(remaining)
                        .map(|diagnostic| ModuleDiagnostic {
                            module: module.canonical_path.clone(),
                            diagnostic,
                        }),
                );
            }
        } else if let Some(module) = modules.first_mut() {
            module.stats.work_units = module.stats.work_units.saturating_add(inferred_work);
        }
        if result.is_err() && inferred_work > remaining {
            exhausted = true;
            diagnostics.push(ModuleDiagnostic {
                module: options.module.clone(),
                diagnostic: Diagnostic::error(
                    D::BudgetExceeded,
                    TextRange::empty(TextSize::from(0)),
                    "source type inference work budget exceeded",
                ),
            });
        }
    }
    let plan = plan(&dependencies);
    Ok(crate::editor::EditorProjectAnalysis {
        modules,
        diagnostics,
        plan,
        exhausted,
    })
}

fn initialization_dependencies(module: &crate::hir::AnalyzedModule) -> BTreeSet<String> {
    const TYPE: u8 = 1;
    const VALUE: u8 = 2;
    let mut usage = vec![0u8; module.symbols().len()];
    for (node, fact) in module.hir().nodes.iter().zip(module.facts()) {
        if let Some(symbol) = fact.symbol {
            usage[symbol.0 as usize] |= if node.kind == H::NamedType {
                TYPE
            } else {
                VALUE
            };
        }
    }
    for symbol in module.exports() {
        usage[symbol.0 as usize] |= VALUE;
    }
    module
        .imports()
        .iter()
        .filter(|import| {
            let used = import
                .names
                .iter()
                .fold(0, |used, (_, symbol)| used | usage[symbol.0 as usize]);
            // Unused imports retain initialization order, including side effects.
            used != TYPE
        })
        .map(|import| import.module.clone())
        .collect()
}
pub(super) fn declarations(hir: &Hir, module: &str, version: &str) -> Vec<ExternalSymbol> {
    let mut result = Vec::new();
    let mut exported = BTreeSet::new();
    for n in hir.children(NodeId(0)) {
        if hir.node(n).kind != H::ModifiedDeclaration || !hir.tokens(n).any(|t| t.kind == K::Pub) {
            continue;
        }
        let Some(d) = hir.children(n).next() else {
            continue;
        };
        match hir.node(d).kind {
            H::TypeAlias => result.extend(super::type_aliases::descriptor(hir, d, module, version)),
            H::Enum => {
                result.extend(super::enumerations::descriptor(hir, d, module, version));
            }
            H::Function => {
                if let Form::Function {
                    name: Some(name), ..
                } = hir.node(d).form
                    && let Some(name) = name.name
                    && let Some(signature) = super::types::signature_hint(hir, d)
                {
                    result.push(ExternalSymbol::function(hir.name(name), signature));
                }
            }
            H::Class => {
                if let Some(name) = hir
                    .tokens(d)
                    .find(|t| t.kind == K::Ident)
                    .and_then(|t| t.name)
                {
                    let name = hir.name(name);
                    let canonical = format!("{module}::{name}");
                    let mut class = ExternalClass {
                        bases: Vec::new(),
                        members: Vec::new(),
                        static_members: Vec::new(),
                    };
                    if let Form::Class {
                        base: Some(base), ..
                    } = hir.node(d).form
                    {
                        class
                            .bases
                            .push(hir.text(hir.node(base).span).replace('.', "::"));
                    }
                    let mut constructor = None;
                    for item in hir.children(d) {
                        let (member, is_static) = match hir.node(item).form {
                            Form::Modified {
                                declaration,
                                static_member,
                                ..
                            } => (declaration, static_member),
                            _ => (item, false),
                        };
                        let mut symbol = match hir.node(member).form {
                            Form::Function {
                                name: Some(member_name),
                                ..
                            } => {
                                let Some(mut signature) = super::types::signature_hint(hir, member)
                                else {
                                    continue;
                                };
                                if !is_static && !signature.parameters.is_empty() {
                                    signature.parameters.remove(0);
                                }
                                if hir.text(member_name.span) == "init" && !is_static {
                                    constructor = Some(signature.clone());
                                }
                                ExternalSymbol::function(hir.text(member_name.span), signature)
                            }
                            Form::Binding {
                                pattern,
                                annotation,
                                initializer,
                                mutable,
                                ..
                            } => {
                                let Some(member_name) =
                                    hir.tokens(pattern).find(|token| token.name.is_some())
                                else {
                                    continue;
                                };
                                let ty = annotation
                                    .map(|node| type_hint(hir, node))
                                    .unwrap_or_else(|| {
                                        if mutable {
                                            TypeHint::Any
                                        } else {
                                            initializer.map_or(TypeHint::Any, |node| {
                                                initializer_hint(hir, node)
                                            })
                                        }
                                    });
                                ExternalSymbol::value(hir.text(member_name.span), ty)
                            }
                            _ => continue,
                        };
                        symbol.reference = Some(Arc::new(SymbolReference {
                            module: module.into(),
                            version: version.into(),
                            export: Some(format!(
                                "{name}::{}::{}",
                                if is_static { "static" } else { "instance" },
                                symbol.name
                            )),
                        }));
                        let writable = match hir.node(member).form {
                            Form::Binding { mutable, .. } => mutable,
                            _ => false,
                        };
                        let member = ExternalMember {
                            write_type: writable.then(|| symbol.ty.clone()),
                            symbol,
                        };
                        if is_static {
                            class.static_members.push(member);
                        } else {
                            class.members.push(member);
                        }
                    }
                    let mut signature = constructor.unwrap_or(ExternalSignature {
                        result_parameter: None,
                        parameters: Vec::new(),
                        result: TypeHint::Any,
                        result_cases: Vec::new(),
                        effect: CallEffect::Unknown,
                        init_safe: None,
                        derived_safe: None,
                    });
                    signature.result = TypeHint::Named(canonical.clone());
                    let mut symbol = ExternalSymbol::value(name, TypeHint::Named(canonical));
                    symbol.signature = Some(signature);
                    symbol.class = Some(Arc::new(class));
                    result.push(symbol);
                }
            }
            H::Let => {
                if let Form::Binding {
                    pattern,
                    annotation,
                    ..
                } = hir.node(d).form
                {
                    let annotation = annotation.map(|node| type_hint(hir, node));
                    let mut context = ExportPatternTypes(hir);
                    let mut projected = super::patterns::Projections::new();
                    super::patterns::project(
                        &mut context,
                        pattern,
                        annotation.unwrap_or(TypeHint::Any),
                        &mut projected,
                    );
                    let mut bindings = BTreeMap::new();
                    for (node, ty) in projected {
                        let name = match hir.node(node).form {
                            Form::Name { name } => name.name,
                            Form::PatternField { key, .. } => key.name,
                            _ => None,
                        };
                        if let Some(name) = name {
                            bindings
                                .entry(name)
                                .and_modify(|old: &mut TypeHint| {
                                    if *old == TypeHint::Any || ty == TypeHint::Any {
                                        *old = TypeHint::Any;
                                    } else if *old != ty {
                                        *old = TypeHint::Union(vec![old.clone(), ty.clone()]);
                                    }
                                })
                                .or_insert(ty);
                        }
                    }
                    for (name, ty) in bindings {
                        if exported.insert(name) {
                            result.push(ExternalSymbol::value(hir.name(name), ty));
                        }
                    }
                }
            }
            _ => (),
        }
    }
    result
}
struct TypeNames<'a> {
    module: &'a BTreeMap<String, String>,
    prelude: &'a BTreeMap<&'a str, &'a str>,
}

impl TypeNames<'_> {
    fn canonical(&self, name: &str) -> String {
        let (head, suffix) = name
            .split_once("::")
            .map_or((name, None), |(head, suffix)| (head, Some(suffix)));
        let canonical = self
            .module
            .get(head)
            .map(String::as_str)
            .or_else(|| self.prelude.get(head).copied());
        match (canonical, suffix) {
            (Some(canonical), Some(suffix)) => format!("{canonical}::{suffix}"),
            (Some(canonical), None) => canonical.into(),
            (None, _) => name.into(),
        }
    }
}

fn canonical_hint(hint: &mut TypeHint, names: &impl Fn(&str) -> String) {
    match hint {
        TypeHint::Named(name) => *name = names(name),
        TypeHint::Enum { cases, .. } => {
            for hint in Arc::make_mut(cases) {
                canonical_hint(hint, names);
            }
        }
        TypeHint::Tuple(types) | TypeHint::Union(types) => {
            for hint in types {
                canonical_hint(hint, names);
            }
        }
        TypeHint::Record(fields) => {
            for (_, ty) in fields {
                canonical_hint(ty, names);
            }
        }
        TypeHint::Callable(parameters, result) => {
            for hint in parameters {
                canonical_hint(hint, names);
            }
            canonical_hint(result, names);
        }
        _ => {}
    }
}
pub(super) fn canonical_symbol(symbol: &mut ExternalSymbol, names: &impl Fn(&str) -> String) {
    canonical_hint(&mut symbol.ty, names);
    if let Some(signature) = &mut symbol.signature {
        canonical_hint(&mut signature.result, names);
        for parameter in &mut signature.parameters {
            canonical_hint(&mut parameter.ty, names);
        }
        for case in &mut signature.result_cases {
            canonical_hint(&mut case.result, names);
        }
    }
    if let Some(class) = &mut symbol.class {
        let class = Arc::make_mut(class);
        for base in &mut class.bases {
            *base = names(base);
        }
        for member in class.members.iter_mut().chain(&mut class.static_members) {
            canonical_symbol(&mut member.symbol, names);
            if let Some(write) = &mut member.write_type {
                canonical_hint(write, names);
            }
        }
    }
}
fn initializer_hint(hir: &Hir, node: NodeId) -> TypeHint {
    match hir.node(node).form {
        Form::Single { child } => initializer_hint(hir, child),
        Form::Literal { token } => match token.kind {
            K::Null => TypeHint::Null,
            K::True | K::False => TypeHint::Bool,
            K::Number => {
                let text = hir.text(token.span).to_ascii_lowercase();
                if !text.starts_with("0x")
                    && !text.starts_with("0o")
                    && !text.starts_with("0b")
                    && text.contains(['.', 'e'])
                {
                    TypeHint::Float
                } else {
                    TypeHint::Int
                }
            }
            _ => TypeHint::Any,
        },
        _ => match hir.node(node).kind {
            H::String => TypeHint::String,
            H::Array => TypeHint::Array,
            H::Object => TypeHint::Table,
            _ => TypeHint::Any,
        },
    }
}
struct ExportPatternTypes<'a>(&'a Hir);
impl super::patterns::PatternTypes for ExportPatternTypes<'_> {
    type Ty = TypeHint;
    fn hir(&self) -> &Hir {
        self.0
    }
    fn unknown(&self) -> TypeHint {
        TypeHint::Any
    }
    fn shape(&self, ty: &TypeHint) -> super::patterns::Shape<TypeHint> {
        use super::patterns::Shape;
        match ty {
            TypeHint::Any | TypeHint::Named(_) | TypeHint::Union(_) => Shape::Unknown,
            TypeHint::Tuple(elements) => Shape::Tuple(elements.clone()),
            TypeHint::Array => Shape::Array,
            TypeHint::Table | TypeHint::Record(_) => Shape::Table,
            _ => Shape::Other,
        }
    }
    fn tuple(&mut self, elements: Vec<TypeHint>) -> TypeHint {
        TypeHint::Tuple(elements)
    }
    fn array(&self) -> TypeHint {
        TypeHint::Array
    }
    fn table(&self) -> TypeHint {
        TypeHint::Table
    }
    fn field(&self, ty: &TypeHint, name: NameId) -> TypeHint {
        if let TypeHint::Record(fields) = ty {
            let name = self.0.name(name);
            if let Some((_, field)) = fields.iter().find(|(key, _)| key == name) {
                return field.clone();
            }
        }
        TypeHint::Any
    }
    fn annotation(&mut self, node: NodeId) -> TypeHint {
        type_hint(self.0, node)
    }
    fn compatible(&self, actual: &TypeHint, expected: &TypeHint) -> bool {
        matches!(self.shape(actual), super::patterns::Shape::Unknown)
            || matches!(self.shape(expected), super::patterns::Shape::Unknown)
            || actual == expected
    }
    fn charge(&mut self, _: NodeId) -> bool {
        true
    }
}
pub(super) fn import_requests(
    hir: &Hir,
    n: NodeId,
    prefix: &[String],
    grouped: bool,
    editor_discovery: bool,
    from: &str,
    out: &mut Vec<(ImportRequest, Option<String>)>,
) {
    let mut path = prefix.to_vec();
    let mut alias = None;
    let mut after_as = false;
    let mut wildcard = false;
    for t in hir.tokens(n) {
        match t.kind {
            K::As => after_as = true,
            K::Ident => {
                let name = hir.name(t.name.unwrap()).to_owned();
                if after_as {
                    alias = Some(name)
                } else {
                    path.push(name)
                }
            }
            K::Star => wildcard = true,
            _ => (),
        }
    }
    if let Some(group) = hir
        .children(n)
        .find(|&n| hir.node(n).kind == H::ImportGroup)
    {
        let mut children = hir.children(group).peekable();
        if editor_discovery && children.peek().is_none() && !path.is_empty() {
            // Editors need the module's real exports before its first grouped
            // name is typed. This hint never creates a binding or runtime edge.
            out.push((
                ImportRequest {
                    from: from.into(),
                    path,
                    kind: ImportKind::NamespaceOrSymbol,
                },
                None,
            ));
        } else {
            for child in children {
                import_requests(hir, child, &path, true, editor_discovery, from, out)
            }
        }
        return;
    }
    // A trailing separator leaves the current module path ready for completion.
    let unfinished_path = editor_discovery
        && grouped
        && matches!(hir.elements(n).last(), Some(HirElement::Token(token)) if token.kind == K::ColonColon);
    out.push((
        ImportRequest {
            from: from.into(),
            path,
            kind: if wildcard {
                ImportKind::Wildcard
            } else if grouped && !unfinished_path {
                ImportKind::Symbol
            } else {
                ImportKind::NamespaceOrSymbol
            },
        },
        alias,
    ));
}
fn plan(graph: &BTreeMap<String, BTreeSet<String>>) -> ModulePlan {
    // Iterative Kosaraju, then stable Kahn order of the SCC condensation graph.
    let mut visited = BTreeSet::new();
    let mut order = Vec::new();
    for start in graph.keys() {
        let mut stack = vec![(start.clone(), false)];
        while let Some((node, exit)) = stack.pop() {
            if exit {
                order.push(node);
                continue;
            }
            if !visited.insert(node.clone()) {
                continue;
            }
            stack.push((node.clone(), true));
            if let Some(deps) = graph.get(&node) {
                for dep in deps.iter().rev() {
                    if graph.contains_key(dep) && !visited.contains(dep) {
                        stack.push((dep.clone(), false));
                    }
                }
            }
        }
    }
    let mut reverse: BTreeMap<String, BTreeSet<String>> =
        graph.keys().map(|k| (k.clone(), BTreeSet::new())).collect();
    for (node, deps) in graph {
        for dep in deps {
            if let Some(users) = reverse.get_mut(dep) {
                users.insert(node.clone());
            }
        }
    }
    visited.clear();
    let mut components = Vec::new();
    let mut membership = BTreeMap::new();
    for start in order.into_iter().rev() {
        if visited.contains(&start) {
            continue;
        }
        let mut stack = vec![start];
        let mut component = Vec::new();
        while let Some(node) = stack.pop() {
            if !visited.insert(node.clone()) {
                continue;
            }
            membership.insert(node.clone(), components.len());
            component.push(node.clone());
            stack.extend(reverse[&node].iter().cloned());
        }
        component.sort();
        components.push(component);
    }
    let mut deps = vec![BTreeSet::new(); components.len()];
    let mut users = vec![BTreeSet::new(); components.len()];
    for (node, edges) in graph {
        let a = membership[node];
        for edge in edges {
            if let Some(&b) = membership.get(edge)
                && a != b
            {
                deps[a].insert(b);
                users[b].insert(a);
            }
        }
    }
    let mut ready = BTreeSet::new();
    for (i, d) in deps.iter().enumerate() {
        if d.is_empty() {
            ready.insert((components[i][0].clone(), i));
        }
    }
    let mut ordered = Vec::new();
    while let Some((_, i)) = ready.pop_first() {
        ordered.push(components[i].clone());
        for &user in &users[i] {
            deps[user].remove(&i);
            if deps[user].is_empty() {
                ready.insert((components[user][0].clone(), user));
            }
        }
    }
    ModulePlan {
        initialization_order: ordered.iter().flatten().cloned().collect(),
        components: ordered,
    }
}
