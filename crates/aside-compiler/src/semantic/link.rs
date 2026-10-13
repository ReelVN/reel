//! Source-module linking. Imports are resolved before initialization; legal SCCs
//! are retained, with initialized-state runtime checks on imported value reads.
use super::types::type_hint;
use super::*;
use crate::hir::{Form, Hir, HirKind as H, NodeId};
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
    let mut modules = Vec::new();
    let mut dependencies = BTreeMap::new();
    if !exhausted {
        let mut symbols = 0usize;
        let mut captures = 0usize;
        let mut imports = 0usize;
        for (path, hir) in projected {
            let local = AnalysisOptions {
                module: path.clone(),
                source_path: None,
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
                    result.push(ExternalSymbol::value(
                        hir.name(name),
                        TypeHint::Named(hir.name(name).into()),
                    ));
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
            TypeHint::Table => Shape::Table,
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
