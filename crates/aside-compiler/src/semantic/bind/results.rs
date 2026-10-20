use super::*;

#[derive(Clone, Copy)]
struct ResultContext {
    used: bool,
    collecting: bool,
}

impl ResultContext {
    const VALUE: Self = Self {
        used: true,
        collecting: false,
    };
    const DISCARD: Self = Self {
        used: false,
        collecting: false,
    };
}

impl Binder<'_> {
    pub(super) fn check_results(&mut self) {
        if self.exhausted
            || !self.diagnostics.is_empty()
            || !self.external_nodes.values().any(|d| d.must_use_result)
        {
            return;
        }
        // Projection orders parents before children. These temporary tables
        // retain no values in HIR, bytecode, or the running program.
        let mut aliases = vec![None; self.out.symbols.len()];
        for i in 0..self.out.hir.nodes.len() {
            let node = NodeId(i as u32);
            if !self.budget(node) {
                return;
            }
            if let Form::Binding {
                pattern,
                initializer: Some(value),
                mutable: false,
                ..
            } = self.out.hir.node(node).form
                && self.kind(pattern) == HirKind::NamePattern
                && let Some(symbol) = self.out.facts[pattern.0 as usize].symbol
            {
                aliases[symbol.0 as usize] = Some(value);
            }
        }
        let module_tail = (self.options.context != AnalysisContext::Module)
            .then(|| self.out.hir.children(NodeId(0)).last())
            .flatten()
            .filter(|&node| {
                matches!(
                    self.out.hir.node(node).form,
                    Form::Statement {
                        terminated: false,
                        ..
                    }
                ) || matches!(
                    self.kind(node),
                    HirKind::Block | HirKind::If | HirKind::Match
                )
            });
        let mut contexts = vec![ResultContext::DISCARD; self.out.hir.nodes.len()];
        for i in 0..contexts.len() {
            let node = NodeId(i as u32);
            if !self.budget(node) {
                return;
            }
            let context = self
                .out
                .hir
                .node(node)
                .parent
                .map_or(ResultContext::DISCARD, |parent| {
                    self.result_context(parent, node, contexts[parent.0 as usize], module_tail)
                });
            contexts[i] = context;
            if !context.used
                && let Form::Call { callee, .. } = self.out.hir.node(node).form
                && self.required_result(callee, &aliases)
            {
                self.error(
                    D::UnusedResult,
                    self.span(node),
                    "call result is discarded; return it, bind it, pass it to another call, \
                     or collect it in children (use `let _ = ...;` to discard explicitly)",
                );
            }
        }
    }

    fn required_result(&mut self, mut node: NodeId, aliases: &[Option<NodeId>]) -> bool {
        loop {
            if !self.budget(node) {
                return false;
            }
            if self.kind(node) == HirKind::Name
                && self.out.facts[node.0 as usize]
                    .symbol
                    .is_some_and(|s| self.out.symbols[s.0 as usize].mutable)
            {
                return false;
            }
            if let Some(descriptor) = self.external_nodes.get(&node) {
                return descriptor.must_use_result;
            }
            if let (HirKind::Group, Form::Single { child }) =
                (self.kind(node), self.out.hir.node(node).form)
            {
                node = child;
            } else if self.kind(node) == HirKind::Name
                && let Some(symbol) = self.out.facts[node.0 as usize].symbol
                && let Some(value) = aliases[symbol.0 as usize]
            {
                node = value;
            } else {
                return false;
            }
        }
    }

    fn result_context(
        &self,
        parent: NodeId,
        child: NodeId,
        context: ResultContext,
        module_tail: Option<NodeId>,
    ) -> ResultContext {
        let node = self.out.hir.node(parent);
        let used = |used| ResultContext { used, ..context };
        match (node.kind, node.form) {
            (HirKind::Module, _) => used(module_tail == Some(child)),
            (HirKind::Block, Form::Block { tail }) => used(context.used && tail == Some(child)),
            (
                HirKind::ExpressionStatement,
                Form::Statement {
                    expression,
                    terminated,
                },
            ) => used(
                (context.collecting && self.out.hir.collects(expression))
                    || (!terminated && context.used),
            ),
            (HirKind::Children, _) => ResultContext {
                used: false,
                collecting: true,
            },
            (HirKind::Group | HirKind::ModifiedDeclaration, _) => context,
            (
                HirKind::If,
                Form::Condition {
                    body, otherwise, ..
                },
            ) if child == body || otherwise == Some(child) => context,
            (HirKind::While, Form::Condition { body, .. })
            | (HirKind::For, Form::For { body, .. })
                if child == body =>
            {
                used(false)
            }
            (
                HirKind::Loop | HirKind::Try | HirKind::Catch | HirKind::Finally | HirKind::Label,
                _,
            ) => used(false),
            (HirKind::Match, Form::Match { value }) if child != value => context,
            (HirKind::MatchArm, Form::MatchArm { body, .. }) if child == body => {
                used(context.used || (context.collecting && self.out.hir.collects(body)))
            }
            // Calls, bindings, return values, conditions, function bodies and
            // defaults are value contexts and suspend the surrounding collector.
            _ => ResultContext::VALUE,
        }
    }
}
