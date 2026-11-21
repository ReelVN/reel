//! Proofs of effects reached on every source control-flow path through a helper.
use super::*;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Effects {
    task: Option<NodeId>,
    host: Option<NodeId>,
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

#[derive(Clone, Copy)]
enum Target {
    Script(FunctionId),
    Native(Paths),
}

impl Binder<'_> {
    pub(super) fn check_definite_effects(&mut self, calls: &[CallSite]) {
        let (task, host) = calls.iter().fold((false, false), |(task, host), call| {
            (
                task || call.effect == CallEffect::StartsTask,
                host || call.derived_forbidden,
            )
        });
        if !task && !host {
            return;
        }
        let reports: Vec<_> = calls
            .iter()
            .filter(|call| !call.conditional && ((task && call.no_wait) || (host && call.derived)))
            .filter_map(|call| Some((call, self.stable_script_target(call)?)))
            .collect();
        if reports.is_empty() {
            return;
        }
        let count = self.out.functions.len();
        let mut targets = HashMap::new();
        let mut edges = vec![Vec::new(); count];
        let mut reverse = vec![Vec::new(); count];
        for call in calls {
            if !self.budget(call.node) {
                return;
            }
            if let Some(function) = self.stable_script_target(call) {
                targets.insert(call.node, Target::Script(function));
                if let Some(caller) = call.caller {
                    edges[caller.0 as usize].push(function);
                    reverse[function.0 as usize].push(caller);
                }
            } else {
                let effect = Effects {
                    task: (call.effect == CallEffect::StartsTask).then_some(call.node),
                    host: call.derived_forbidden.then_some(call.node),
                };
                let paths = if call.effect == CallEffect::NoWait {
                    Paths {
                        next: Some(effect),
                        exit: None,
                    }
                } else if effect != Effects::default() {
                    Paths::opaque(effect)
                } else {
                    continue;
                };
                targets.insert(call.node, Target::Native(paths));
            }
        }

        let mut needed = vec![false; count];
        let mut queue = VecDeque::new();
        let mut pending: Vec<_> = reports.iter().map(|&(_, target)| target).collect();
        while let Some(function) = pending.pop() {
            let index = function.0 as usize;
            if !std::mem::replace(&mut needed[index], true) {
                queue.push_back(function);
                pending.extend_from_slice(&edges[index]);
            }
        }
        let mut queued = needed.clone();
        let mut summaries = vec![Effects::default(); count];
        while let Some(function) = queue.pop_front() {
            let index = function.0 as usize;
            queued[index] = false;
            let body = self.out.functions[index].body;
            let next = self.effect_paths(body, &targets, &summaries, 0).summary();
            if self.exhausted {
                return;
            }
            // Each effect becomes proven once. Recursion without a direct
            // witness starts unknown and never fabricates a proof.
            let merged = summaries[index].then(next);
            if merged != summaries[index] {
                summaries[index] = merged;
                for &caller in &reverse[index] {
                    let index = caller.0 as usize;
                    if needed[index] && !std::mem::replace(&mut queued[index], true) {
                        queue.push_back(caller);
                    }
                }
            }
        }

        for (call, target) in reports {
            let effects = summaries[target.0 as usize];
            let report = if call.no_wait
                && let Some(origin) = effects.task
            {
                Some((
                    D::NoWait,
                    origin,
                    "this helper launches a task. In a node block, call it after construction and bind the task to its owner",
                ))
            } else if call.derived
                && let Some(origin) = effects.host
            {
                Some((
                    D::InvalidEffect,
                    origin,
                    "this helper calls a host function that is unavailable in derived evaluation. Compute its result before derive",
                ))
            } else {
                None
            };
            if let Some((code, origin, message)) = report {
                let index = self.diagnostics.len();
                let origin = self.span(origin);
                self.error(code, self.span(call.node), message);
                if let Some(diagnostic) = self.diagnostics.get_mut(index) {
                    diagnostic.related.push(RelatedRange {
                        range: origin,
                        message: "host call inside the helper".into(),
                    });
                }
            }
        }
    }

    fn stable_script_target(&self, call: &CallSite) -> Option<FunctionId> {
        let target = call.callee?;
        let Form::Call {
            callee,
            optional: false,
            chain: false,
            ..
        } = self.out.hir.node(call.node).form
        else {
            return None;
        };
        let symbol = self.out.facts[callee.0 as usize].symbol?;
        (!call.bound
            && self.out.symbols[symbol.0 as usize].kind == SymbolKind::Function
            && self.out.functions[target.0 as usize].kind == FunctionKind::Plain)
            .then_some(target)
    }

    fn effect_paths(
        &mut self,
        node: NodeId,
        targets: &HashMap<NodeId, Target>,
        summaries: &[Effects],
        depth: usize,
    ) -> Paths {
        let unknown = Paths::opaque(Effects::default());
        if depth >= self.options.limits.max_depth || !self.budget(node) {
            return unknown;
        }
        let kind = self.kind(node);
        let descend =
            |binder: &mut Self, node| binder.effect_paths(node, targets, summaries, depth + 1);
        match self.out.hir.node(node).form {
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
                for child in self.children(node) {
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
                    let right = match (operator.kind, self.constant_bool(left)) {
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
                paths.then(match targets.get(&node) {
                    Some(Target::Script(function)) => Paths::opaque(summaries[function.0 as usize]),
                    Some(Target::Native(paths)) => *paths,
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
                let constant = self.branch_constant(pattern, condition);
                let other = |binder: &mut Self| {
                    otherwise.map_or_else(Paths::pure, |node| descend(binder, node))
                };
                let branch = match constant {
                    Some(true) => descend(self, body),
                    Some(false) => other(self),
                    None => descend(self, body).join(other(self)),
                };
                paths.then(branch)
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
}
