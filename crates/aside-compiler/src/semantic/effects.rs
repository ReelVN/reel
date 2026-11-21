//! Proofs of restricted effects on all source paths through a stable helper.
use super::{AnalysisLimits, CallEffect, ModuleDiagnostic, SymbolReference};
use crate::hir::{
    AnalyzedModule, FieldKey, Form, FunctionId, FunctionKind, HirKind, NodeId, RuntimeChecks,
    SymbolKind, Type,
};
use crate::{Diagnostic, DiagnosticCode as D, RelatedRange, SyntaxKind as K};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub(crate) struct EffectCall {
    pub target: EffectTarget,
    pub node: NodeId,
    pub caller: Option<FunctionId>,
    pub effect: CallEffect,
    pub derived_forbidden: bool,
    pub no_wait: bool,
    pub derived: bool,
}

#[derive(Debug, Clone)]
pub(crate) enum EffectTarget {
    Local(FunctionId),
    Reference(Arc<SymbolReference>),
    Host,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Origin {
    module: usize,
    node: NodeId,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Effects {
    task: Option<Origin>,
    host: Option<Origin>,
}

impl Effects {
    fn then(self, next: Self) -> Self {
        Self {
            task: self.task.or(next.task),
            host: self.host.or(next.host),
        }
    }

    fn join(self, alternative: Self) -> Self {
        Self {
            task: self.task.filter(|_| alternative.task.is_some()),
            host: self.host.filter(|_| alternative.host.is_some()),
        }
    }
}

#[derive(Clone, Copy)]
struct Paths {
    next: Option<Effects>,
    exit: Option<Effects>,
}

impl Paths {
    fn pure() -> Self {
        Self {
            next: Some(Effects::default()),
            exit: None,
        }
    }

    fn opaque(effects: Effects) -> Self {
        Self {
            next: Some(effects),
            exit: Some(effects),
        }
    }

    fn alternatives(left: Option<Effects>, right: Option<Effects>) -> Option<Effects> {
        match (left, right) {
            (Some(left), Some(right)) => Some(left.join(right)),
            _ => left.or(right),
        }
    }

    fn then(self, next: Self) -> Self {
        let follow = |path| self.next.zip(path).map(|(a, b)| a.then(b));
        Self {
            next: follow(next.next),
            exit: Self::alternatives(self.exit, follow(next.exit)),
        }
    }

    fn join(self, alternative: Self) -> Self {
        Self {
            next: Self::alternatives(self.next, alternative.next),
            exit: Self::alternatives(self.exit, alternative.exit),
        }
    }

    fn summary(self) -> Effects {
        Self::alternatives(self.next, self.exit).unwrap_or_default()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Function {
    module: usize,
    id: FunctionId,
}

#[derive(Clone, Copy)]
enum Target {
    Script(Function),
    Host(Paths),
}

type DeclarationIndex<'a> = HashMap<(&'a str, &'a str, &'a str), Function>;

struct Graph {
    calls: Vec<HashMap<NodeId, Target>>,
    edges: HashMap<Function, Vec<Function>>,
    parents: HashMap<Function, Vec<Function>>,
    reports: Vec<(Origin, Function, bool, bool)>,
}

pub(super) fn check(
    modules: &mut [AnalyzedModule],
    limits: AnalysisLimits,
    diagnostics: &mut Vec<ModuleDiagnostic>,
) -> (usize, bool) {
    let mut count = 0usize;
    let mut has_restricted = false;
    let mut has_effect = false;
    for module in &*modules {
        count = count.saturating_add(module.effect_calls.len());
        for call in &module.effect_calls {
            has_effect |= call.derived_forbidden || call.effect == CallEffect::StartsTask;
            has_restricted |=
                !matches!(call.target, EffectTarget::Host) && (call.no_wait || call.derived);
        }
    }
    let calls: Vec<_> = modules
        .iter_mut()
        .map(|module| std::mem::take(&mut module.effect_calls))
        .collect();
    let mut work = count;
    if has_restricted && has_effect && limits.max_diagnostics > 0 {
        work = work.saturating_add(
            modules
                .iter()
                .map(|module| module.exports.len())
                .sum::<usize>(),
        );
        if work <= limits.max_work {
            let graph = Graph::resolve(modules, &calls);
            let mut search = Analysis {
                modules,
                calls: &graph.calls,
                summaries: HashMap::new(),
                limits,
                work,
            };
            search.solve(&graph);
            work = search.work;
            if work <= limits.max_work {
                search.report(&graph, diagnostics);
            }
        }
    }
    let exhausted = work > limits.max_work;
    if exhausted && let Some(module) = modules.first() {
        diagnostics.push(ModuleDiagnostic {
            module: module.canonical_path.clone(),
            diagnostic: Diagnostic::error(
                D::BudgetExceeded,
                module.hir.node(NodeId(0)).span,
                "effect proof work budget exceeded",
            ),
        });
    }
    (work, exhausted)
}

impl Graph {
    fn resolve(modules: &[AnalyzedModule], calls: &[Vec<EffectCall>]) -> Self {
        let mut declarations = DeclarationIndex::new();
        for (index, module) in modules.iter().enumerate() {
            for &export in &module.exports {
                let symbol = &module.symbols[export.0 as usize];
                if symbol.kind == SymbolKind::Function
                    && let Type::Function(id) = module.types[symbol.ty.0 as usize]
                    && module.functions[id.0 as usize].kind == FunctionKind::Plain
                {
                    declarations.insert(
                        (
                            module.canonical_path(),
                            module.version(),
                            module.hir.name(symbol.name),
                        ),
                        Function { module: index, id },
                    );
                }
            }
        }
        let mut graph = Self {
            calls: vec![HashMap::new(); modules.len()],
            edges: HashMap::new(),
            parents: HashMap::new(),
            reports: Vec::new(),
        };
        for (module, calls) in calls.iter().enumerate() {
            for call in calls {
                let script = match &call.target {
                    EffectTarget::Local(id) => Some(Function { module, id: *id }),
                    EffectTarget::Reference(reference) => reference
                        .export
                        .as_deref()
                        .and_then(|export| {
                            declarations.get(&(
                                reference.module.as_str(),
                                reference.version.as_str(),
                                export,
                            ))
                        })
                        .copied(),
                    EffectTarget::Host => None,
                };
                let origin = Origin {
                    module,
                    node: call.node,
                };
                let target = if let Some(script) = script {
                    if let Some(id) = call.caller {
                        let caller = Function { module, id };
                        graph.edges.entry(caller).or_default().push(script);
                        graph.parents.entry(script).or_default().push(caller);
                    }
                    if call.no_wait || call.derived {
                        graph
                            .reports
                            .push((origin, script, call.no_wait, call.derived));
                    }
                    Target::Script(script)
                } else {
                    let effects = Effects {
                        task: (call.effect == CallEffect::StartsTask).then_some(origin),
                        host: call.derived_forbidden.then_some(origin),
                    };
                    let paths = if call.effect == CallEffect::NoWait {
                        Paths {
                            next: Some(effects),
                            exit: None,
                        }
                    } else if effects != Effects::default() {
                        Paths::opaque(effects)
                    } else {
                        continue;
                    };
                    Target::Host(paths)
                };
                graph.calls[module].insert(call.node, target);
            }
        }
        graph
    }
}

struct Analysis<'a> {
    modules: &'a [AnalyzedModule],
    calls: &'a [HashMap<NodeId, Target>],
    summaries: HashMap<Function, Effects>,
    limits: AnalysisLimits,
    work: usize,
}

impl Analysis<'_> {
    fn solve(&mut self, graph: &Graph) {
        let mut queue = VecDeque::new();
        let mut pending: Vec<_> = graph
            .reports
            .iter()
            .map(|(_, target, _, _)| *target)
            .collect();
        while let Some(function) = pending.pop() {
            if let std::collections::hash_map::Entry::Vacant(entry) = self.summaries.entry(function)
            {
                entry.insert(Effects::default());
                queue.push_back(function);
                if let Some(calls) = graph.edges.get(&function) {
                    pending.extend_from_slice(calls);
                }
            }
        }
        let mut queued: std::collections::HashSet<_> = queue.iter().copied().collect();
        while let Some(function) = queue.pop_front() {
            queued.remove(&function);
            let definition = &self.modules[function.module].functions[function.id.0 as usize];
            let mut paths = Paths::pure();
            for parameter in &definition.parameters {
                if let Some(default) = parameter.default {
                    paths = paths.then(Paths::pure().join(self.paths(function.module, default, 0)));
                }
            }
            let next = paths.then(self.paths(function.module, definition.body, 0)).summary();
            if self.work > self.limits.max_work {
                return;
            }
            let summary = self.summaries.get_mut(&function).unwrap();
            // A witness can only be added once for each effect. Recursive
            // paths start unknown and cannot furnish their own witnesses.
            let merged = summary.then(next);
            if merged != *summary {
                *summary = merged;
                for &caller in graph.parents.get(&function).into_iter().flatten() {
                    if self.summaries.contains_key(&caller) && queued.insert(caller) {
                        queue.push_back(caller);
                    }
                }
            }
        }
    }

    fn report(&self, graph: &Graph, diagnostics: &mut Vec<ModuleDiagnostic>) {
        let limit = diagnostics
            .len()
            .saturating_add(self.limits.max_diagnostics);
        for &(site, target, no_wait, derived) in &graph.reports {
            if diagnostics.len() >= limit {
                break;
            }
            let effects = self.summaries[&target];
            let (code, origin, message) = if no_wait && let Some(origin) = effects.task {
                (
                    D::NoWait,
                    origin,
                    "this helper launches a task. In a node block, call it after construction and bind the task to its owner",
                )
            } else if derived && let Some(origin) = effects.host {
                (
                    D::InvalidEffect,
                    origin,
                    "this helper calls a host function that is unavailable in derived evaluation. Compute its result before derive",
                )
            } else {
                continue;
            };
            let caller = &self.modules[site.module];
            let host = &self.modules[origin.module];
            let mut diagnostic = Diagnostic::error(code, caller.hir.node(site.node).span, message);
            diagnostic.related.push(RelatedRange {
                range: host.hir.node(origin.node).span,
                source: (site.module != origin.module).then(|| host.source_path.clone()),
                message: "host call inside the helper".into(),
            });
            diagnostics.push(ModuleDiagnostic {
                module: caller.canonical_path.clone(),
                diagnostic,
            });
        }
    }

    fn paths(&mut self, module: usize, node: NodeId, depth: usize) -> Paths {
        let unknown = Paths::opaque(Effects::default());
        self.work = self.work.saturating_add(1);
        if depth >= self.limits.max_depth || self.work > self.limits.max_work {
            return unknown;
        }
        let hir = &self.modules[module].hir;
        let kind = hir.node(node).kind;
        let descend = |analysis: &mut Self, child| analysis.paths(module, child, depth + 1);
        match hir.node(node).form {
            Form::Literal { .. }
            | Form::Name { .. }
            | Form::Function { .. }
            | Form::TypeAlias { .. } => Paths::pure(),
            Form::Block { .. } | Form::Sequence
                if matches!(
                    kind,
                    HirKind::Block
                        | HirKind::Module
                        | HirKind::Children
                        | HirKind::Arguments
                        | HirKind::Object
                        | HirKind::Array
                        | HirKind::Tuple
                        | HirKind::String
                ) =>
            {
                let mut paths = Paths::pure();
                for child in hir.children(node) {
                    if paths.next.is_none() {
                        break;
                    }
                    paths = paths.then(descend(self, child));
                }
                paths
            }
            Form::Binding { initializer, .. } => {
                initializer.map_or_else(Paths::pure, |value| descend(self, value))
            }
            Form::Statement { expression, .. }
            | Form::Field {
                key: FieldKey::Name(_),
                value: expression,
            }
            | Form::Spread { value: expression }
            | Form::Argument {
                value: expression, ..
            }
            | Form::Modified {
                declaration: expression,
                ..
            } => descend(self, expression),
            Form::Field {
                key: FieldKey::Expression(key),
                value,
            } => descend(self, key).then(descend(self, value)),
            Form::Single { child } if matches!(kind, HirKind::Group | HirKind::Interpolation) => {
                descend(self, child)
            }
            Form::Unary { operator, value } => {
                let paths = descend(self, value);
                if operator.kind == K::Await {
                    paths.then(unknown)
                } else {
                    paths
                }
            }
            Form::Binary {
                left,
                operator,
                right,
            } => {
                let paths = descend(self, left);
                if matches!(operator.kind, K::AndAnd | K::OrOr | K::QuestionQuestion) {
                    let right = match (operator.kind, super::forms::constant_bool(hir, left)) {
                        (K::AndAnd, Some(false)) | (K::OrOr, Some(true)) => Paths::pure(),
                        (K::AndAnd, Some(true)) | (K::OrOr, Some(false)) => descend(self, right),
                        _ => Paths::pure().join(descend(self, right)),
                    };
                    paths.then(right)
                } else {
                    paths.then(descend(self, right))
                }
            }
            Form::Member {
                object,
                optional: false,
                chain: false,
                ..
            } => descend(self, object),
            Form::Index {
                object,
                index,
                optional: false,
                chain: false,
            } => descend(self, object).then(descend(self, index)),
            Form::Call {
                callee,
                arguments,
                children,
                optional: false,
                chain: false,
            } => {
                let mut paths = descend(self, callee);
                for child in arguments.into_iter().chain(children) {
                    paths = paths.then(descend(self, child));
                }
                paths.then(match self.calls[module].get(&node) {
                    Some(Target::Script(function)) => {
                        Paths::opaque(self.summaries.get(function).copied().unwrap_or_default())
                    }
                    Some(Target::Host(paths)) => *paths,
                    None => unknown,
                })
            }
            Form::Condition {
                pattern,
                condition,
                body,
                otherwise,
            } if kind == HirKind::If => {
                let paths = descend(self, condition);
                let constant = pattern
                    .is_none()
                    .then(|| super::forms::constant_bool(hir, condition))
                    .flatten();
                let other = |analysis: &mut Self| {
                    otherwise.map_or_else(Paths::pure, |node| descend(analysis, node))
                };
                let branch = match constant {
                    Some(true) => descend(self, body),
                    Some(false) => other(self),
                    None => descend(self, body).join(other(self)),
                };
                paths.then(branch)
            }
            Form::Match { value } => {
                let input = descend(self, value);
                input.then(self.match_paths(module, node, depth))
            }
            Form::Transfer { value, .. } if matches!(kind, HirKind::Return | HirKind::Throw) => {
                let paths = value.map_or_else(Paths::pure, |value| descend(self, value));
                Paths {
                    next: None,
                    exit: Some(paths.summary()),
                }
            }
            _ => unknown,
        }
    }

    fn match_paths(&mut self, module: usize, node: NodeId, depth: usize) -> Paths {
        let analyzed = &self.modules[module];
        let hir = &analyzed.hir;
        let mut alternatives = Paths {
            next: None,
            exit: None,
        };
        for arm in hir.children(node).skip(1) {
            let Form::MatchArm {
                pattern,
                guard,
                body,
            } = hir.node(arm).form
            else {
                continue;
            };
            let matched =
                guard.map_or_else(Paths::pure, |guard| self.paths(module, guard, depth + 1));
            let constant = guard.and_then(|guard| super::forms::constant_bool(hir, guard));
            let branch = if constant == Some(false) {
                Paths {
                    next: None,
                    exit: matched.exit,
                }
            } else {
                matched.then(self.paths(module, body, depth + 1))
            };
            alternatives = alternatives.join(branch);
            // Earlier guards may have failed without entering their arm. Only
            // a definite match can remove the remaining MatchError path.
            if guard.is_none()
                && (is_catch_all(hir, pattern)
                    || analyzed.facts[arm.0 as usize].checks.0 & RuntimeChecks::LAST_ENUM_ARM != 0)
            {
                return alternatives;
            }
        }
        alternatives.join(Paths {
            next: None,
            exit: Some(Effects::default()),
        })
    }
}

fn is_catch_all(hir: &crate::hir::Hir, mut pattern: NodeId) -> bool {
    loop {
        match hir.node(pattern).form {
            Form::Single { child } => pattern = child,
            form => return matches!(form, Form::Name { .. } | Form::Wildcard),
        }
    }
}
