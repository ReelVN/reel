//! Source-module linking. Imports are resolved before initialization; legal SCCs
//! are retained, with initialized-state runtime checks on imported value reads.
use super::*;
use crate::hir::{Form, Hir, HirKind as H, NameId, NodeId};
use crate::{DiagnosticCode as D, SyntaxKind as K, TextRange, TextSize};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
};

#[derive(Debug, Clone)]
pub struct SourceModule {
    pub canonical_path: String,
    pub version: String,
    pub source: Arc<str>,
}
#[derive(Debug, Clone)]
pub struct ModuleDiagnostic {
    pub module: String,
    pub diagnostic: Diagnostic,
}
#[derive(Debug, Clone, Default)]
pub struct ModulePlan {
    /// Dependencies precede users; each SCC is sorted by canonical path.
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
}
impl ModuleResolver for Overlay<'_> {
    fn resolve(&mut self, r: &ImportRequest) -> Result<ResolvedImport, ResolveError> {
        // A private/missing export in a known source module cannot be bypassed
        // by accidentally asking the filesystem/host resolver a second time.
        let full = r.path.join("::");
        let parent = r.path.split_last().map(|(_, p)| p.join("::"));
        if self.memory.modules.contains_key(&full)
            || parent
                .as_ref()
                .is_some_and(|p| self.memory.modules.contains_key(p))
        {
            self.memory.resolve(r)
        } else {
            self.fallback.resolve(r)
        }
    }
}
pub fn analyze_project(
    inputs: &[SourceModule],
    options: &AnalysisOptions,
    resolver: &mut dyn ModuleResolver,
) -> Result<ProjectAnalysis, SourceTooLarge> {
    let mut diagnostics = Vec::new();
    let mut projected = BTreeMap::new();
    let mut memory = MemoryResolver::default();
    let mut exhausted = false;
    let diagnostic_limit = options.limits.max_diagnostics.max(1);
    if inputs.len() > options.limits.max_modules
        || inputs
            .iter()
            .fold(0usize, |total, m| total.saturating_add(m.source.len()))
            > options.limits.max_total_source_bytes
    {
        diagnostics.push(ModuleDiagnostic {
            module: options.module.clone(),
            diagnostic: Diagnostic::error(
                D::BudgetExceeded,
                TextRange::empty(TextSize::from(0)),
                "project module budget exceeded",
            ),
        });
        return Ok(ProjectAnalysis {
            modules: None,
            diagnostics,
            plan: ModulePlan::default(),
            exhausted: true,
        });
    }
    let mut total_nodes = 0usize;
    for input in inputs {
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
        let parsed = crate::parse(&input.source)?;
        if !parsed.is_ok() {
            exhausted |= parsed.exhausted();
            diagnostics.extend(parsed.diagnostics().iter().cloned().map(|diagnostic| {
                ModuleDiagnostic {
                    module: input.canonical_path.clone(),
                    diagnostic,
                }
            }));
            diagnostics.truncate(diagnostic_limit);
            continue;
        }
        match super::project::project(
            &parsed,
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
                    exports: declarations(&hir),
                });
                projected.insert(input.canonical_path.clone(), hir);
            }
        }
    }
    diagnostics.truncate(diagnostic_limit);
    // Public reexports form monotone interface equations. Cache import paths
    // once and revisit only users of changed interfaces, including legal cycles.
    let mut requests = BTreeMap::new();
    let mut users: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (path, hir) in &projected {
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
                import_requests(hir, use_path, &[], false, path, &mut module_requests);
            }
        }
        for (request, _) in &module_requests {
            let full = request.path.join("::");
            let target = if memory.modules.contains_key(&full) {
                full
            } else {
                request
                    .path
                    .split_last()
                    .map_or(String::new(), |(_, p)| p.join("::"))
            };
            users.entry(target).or_default().insert(path.clone());
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
            }
            .resolve(request);
            if let Ok(resolved) = result {
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
    let mut modules = Vec::new();
    let mut dependencies = BTreeMap::new();
    if !exhausted {
        let mut symbols = 0usize;
        let mut captures = 0usize;
        let mut imports = 0usize;
        for (path, hir) in projected {
            let local = AnalysisOptions {
                module: path.clone(),
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
            let result = super::bind::analyze(
                hir,
                &local,
                &mut Overlay {
                    memory: &mut memory,
                    fallback: resolver,
                },
            );
            work = work.saturating_add(result.stats.work_units);
            symbols += result.stats.symbols;
            captures += result.stats.captures;
            imports += result.stats.imports;
            exhausted |= result.exhausted;
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
            if let Some(module) = result.module {
                dependencies.insert(
                    path,
                    module
                        .imports()
                        .iter()
                        .map(|i| i.module.clone())
                        .collect::<BTreeSet<_>>(),
                );
                modules.push(module);
            }
        }
    }
    let plan = plan(&dependencies);
    Ok(ProjectAnalysis {
        modules: if diagnostics.is_empty() && !exhausted {
            Some(modules)
        } else {
            None
        },
        diagnostics,
        plan,
        exhausted,
    })
}
fn declarations(hir: &Hir) -> Vec<ExternalSymbol> {
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
            H::Function => {
                let Some(name) = hir
                    .tokens(d)
                    .find(|t| t.kind == K::Ident)
                    .and_then(|t| t.name)
                else {
                    continue;
                };
                let parameters = hir
                    .children(d)
                    .find(|&n| hir.node(n).kind == H::Parameters)
                    .map(|p| {
                        hir.children(p)
                            .filter_map(|p| {
                                let name = hir.tokens(p).find(|t| t.kind == K::Ident)?.name?;
                                Some(ExternalParameter {
                                    name: hir.name(name).into(),
                                    ty: hir
                                        .children(p)
                                        .find(|&n| is_type(hir.node(n).kind))
                                        .map_or(TypeHint::Any, |n| type_hint(hir, n)),
                                    mutable: hir.tokens(p).any(|t| t.kind == K::Mut),
                                    default: hir.tokens(p).any(|t| t.kind == K::Eq),
                                    rest: hir.tokens(p).any(|t| t.kind == K::Ellipsis),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let result_type = if hir.tokens(d).any(|t| t.kind == K::Async) {
                    TypeHint::Future
                } else if hir.tokens(d).any(|t| t.kind == K::Gen) {
                    TypeHint::Generator
                } else {
                    hir.children(d)
                        .find(|&n| is_type(hir.node(n).kind))
                        .map_or(TypeHint::Any, |n| type_hint(hir, n))
                };
                result.push(ExternalSymbol::function(
                    hir.name(name),
                    ExternalSignature {
                        parameters,
                        result: result_type,
                        effect: CallEffect::Unknown,
                        init_safe: None,
                        derived_safe: None,
                    },
                ));
            }
            H::Class => {
                if let Some(name) = hir
                    .tokens(d)
                    .find(|t| t.kind == K::Ident)
                    .and_then(|t| t.name)
                {
                    result.push(ExternalSymbol::value(
                        hir.name(name),
                        TypeHint::Named(hir.name(name).into()),
                    ));
                }
            }
            H::Let => {
                let annotation = hir
                    .children(d)
                    .find(|&n| is_type(hir.node(n).kind))
                    .map(|n| type_hint(hir, n));
                if let Form::Binding { pattern, .. } = hir.node(d).form {
                    for name in pattern_names(hir, pattern) {
                        if exported.insert(name) {
                            result.push(ExternalSymbol::value(
                                hir.name(name),
                                annotation.clone().unwrap_or(TypeHint::Any),
                            ));
                        }
                    }
                }
            }
            _ => (),
        }
    }
    result
}
fn pattern_names(hir: &Hir, n: NodeId) -> BTreeSet<NameId> {
    let mut stack = vec![n];
    let mut names = BTreeSet::new();
    while let Some(n) = stack.pop() {
        match hir.node(n).form {
            Form::Name { name } => {
                if let Some(name) = name.name
                    && hir.name(name) != "_"
                {
                    names.insert(name);
                }
            }
            Form::PatternField { key, pattern, .. } => {
                if let Some(pattern) = pattern {
                    stack.push(pattern);
                } else {
                    names.insert(key.name.unwrap());
                }
            }
            Form::Or { left, .. } => stack.push(left),
            Form::TypePattern { pattern, .. } => stack.push(pattern),
            Form::Sequence | Form::Single { .. } => stack.extend(hir.children(n)),
            _ => (),
        }
    }
    names
}
fn is_type(k: H) -> bool {
    matches!(
        k,
        H::NamedType | H::GroupType | H::TupleType | H::UnionType | H::FunctionType
    )
}
fn type_hint(hir: &Hir, n: NodeId) -> TypeHint {
    match hir.node(n).kind {
        H::NamedType => {
            let text = hir.tokens(n).map(|t| hir.text(t.span)).collect::<String>();
            match text.as_str() {
                "any" => TypeHint::Any,
                "null" => TypeHint::Null,
                "bool" => TypeHint::Bool,
                "int" => TypeHint::Int,
                "float" => TypeHint::Float,
                "string" => TypeHint::String,
                "array" => TypeHint::Array,
                "table" => TypeHint::Table,
                "Future" => TypeHint::Future,
                "generator" => TypeHint::Generator,
                _ => TypeHint::Named(text),
            }
        }
        H::GroupType => hir
            .children(n)
            .next()
            .map_or(TypeHint::Any, |n| type_hint(hir, n)),
        H::TupleType => TypeHint::Tuple(hir.children(n).map(|n| type_hint(hir, n)).collect()),
        H::UnionType => TypeHint::Union(hir.children(n).map(|n| type_hint(hir, n)).collect()),
        H::FunctionType => {
            let mut args = hir
                .children(n)
                .map(|n| type_hint(hir, n))
                .collect::<Vec<_>>();
            let result = args.pop().unwrap_or(TypeHint::Any);
            TypeHint::Callable(args, Box::new(result))
        }
        _ => TypeHint::Any,
    }
}
fn import_requests(
    hir: &Hir,
    n: NodeId,
    prefix: &[String],
    grouped: bool,
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
        for child in hir.children(group) {
            import_requests(hir, child, &path, true, from, out)
        }
        return;
    }
    out.push((
        ImportRequest {
            from: from.into(),
            path,
            kind: if wildcard {
                ImportKind::Wildcard
            } else if grouped {
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
