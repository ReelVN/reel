//! Control flow, jump targets and reachability.
use super::*;

impl Binder<'_> {
    pub(super) fn condition(&mut self, n: NodeId, ctx: &Context, is_loop: bool) -> TypeId {
        let Form::Condition {
            pattern,
            condition,
            body,
            otherwise,
        } = self.out.hir.node(n).form
        else {
            return ANY;
        };
        let mut inner = ctx.clone();
        if is_loop && inner.loops.last().is_none_or(|(node, _, _)| *node != n) {
            // The condition is inside its loop's control boundary as well.
            // Labelled while already installed this exact target in loop_node.
            inner.loops.push((n, None, ctx.finally.len()));
        }
        if is_loop {
            // Both the condition and body are reentered through the loop head.
            inner.iteration = true;
        }
        let value = self.visit(condition, if is_loop { &inner } else { ctx });
        if pattern.is_none() {
            self.require(condition, value, BOOL, "condition");
        }
        let previous = self.scope;
        self.scope = self.new_scope(Some(previous), ctx.function, self.span(n));
        if let Some(p) = pattern {
            self.bind_pattern(p, value, &inner, PatternUse::Branch, false);
        }
        let constant = self.constant_bool(condition);
        inner.conditional |= constant != Some(true);
        let refinement_mark = self.refinement_undo.len();
        if pattern.is_none() {
            self.refine_condition(condition, true);
        }
        let yes = self.visit(body, &inner);
        self.restore_refinements(refinement_mark);
        self.scope = previous;
        let mut otherwise_ctx = ctx.clone();
        otherwise_ctx.conditional |= constant != Some(false);
        if pattern.is_none() {
            self.refine_condition(condition, false);
        }
        let no = otherwise.map_or(NULL, |c| self.visit(c, &otherwise_ctx));
        self.restore_refinements(refinement_mark);
        if is_loop {
            NULL
        } else if yes == no {
            yes
        } else {
            ANY
        }
    }
    pub(super) fn loop_node(&mut self, n: NodeId, label: Option<NameId>, ctx: &Context) -> TypeId {
        if self.kind(n) == HirKind::While {
            let mut c = ctx.clone();
            c.loops.push((n, label, ctx.finally.len()));
            self.condition(n, &c, true);
            return NULL;
        }
        let mut inner = ctx.clone();
        if label.is_some_and(|label| ctx.loops.iter().any(|(_, old, _)| *old == Some(label))) {
            self.error(
                D::InvalidControl,
                self.span(n),
                "duplicate active loop label",
            );
        }
        inner.loops.push((n, label, ctx.finally.len()));
        inner.iteration = true;
        inner.conditional |= self.kind(n) == HirKind::For;
        let (pattern, source, body, asynchronous) = match self.out.hir.node(n).form {
            Form::For {
                pattern,
                source,
                body,
                asynchronous,
            } => (Some(pattern), Some(source), body, asynchronous),
            Form::Single { child } => (None, None, child, false),
            _ => return ANY,
        };
        let value = source.map_or(ANY, |c| self.visit(c, ctx));
        if asynchronous {
            if ctx.no_wait && (!ctx.conditional || ctx.generator || ctx.cleanup_boundary.is_some())
            {
                self.error(
                    D::NoWait,
                    self.span(n),
                    "for await is forbidden in this execution context",
                );
            }
            if ctx.no_wait {
                self.check(n, RuntimeChecks::NO_WAIT);
            }
            if let Some(f) = ctx.function {
                self.out.functions[f.0 as usize].known_wait = true;
            }
        }
        let previous = self.scope;
        self.scope = self.new_scope(Some(previous), ctx.function, self.span(n));
        if let Some(p) = pattern {
            let item = match &self.out.types[value.0 as usize] {
                Type::Tuple(ts) => ts
                    .iter()
                    .copied()
                    .reduce(|a, b| if a == b { a } else { ANY })
                    .unwrap_or(ANY),
                _ => ANY,
            };
            self.bind_pattern(p, item, &inner, PatternUse::Declaration, false);
        }
        self.visit(body, &inner);
        self.scope = previous;
        NULL
    }
    pub(super) fn match_node(&mut self, n: NodeId, children: &[NodeId], ctx: &Context) -> TypeId {
        let value = children.first().map_or(ANY, |&c| self.visit(c, ctx));
        let mut result = None;
        for &arm in children.iter().skip(1) {
            let previous = self.scope;
            self.scope = self.new_scope(Some(previous), ctx.function, self.span(arm));
            self.out.facts[arm.0 as usize].scope = Some(self.scope);
            let parts = self.children(arm);
            if let Some(&p) = parts.first() {
                self.bind_pattern(p, value, ctx, PatternUse::Branch, false);
            }
            if self.token(arm, K::If).is_some()
                && let Some(&guard) = parts.get(1)
            {
                let mut c = ctx.clone();
                c.no_wait = true;
                c.conditional = true;
                let t = self.visit(guard, &c);
                self.require(guard, t, BOOL, "match guard");
            }
            let mut branch = ctx.clone();
            branch.conditional = true;
            let t = parts.last().map_or(NULL, |&body| self.visit(body, &branch));
            result = Some(match result {
                None => t,
                Some(old) if old == t => t,
                _ => ANY,
            });
            self.scope = previous;
        }
        self.check(n, RuntimeChecks::PATTERN);
        result.unwrap_or(ANY)
    }
    pub(super) fn try_node(&mut self, _n: NodeId, children: &[NodeId], ctx: &Context) -> TypeId {
        let finally = children
            .iter()
            .copied()
            .find(|&c| self.kind(c) == HirKind::Finally);
        let mut protected = ctx.clone();
        if let Some(f) = finally {
            protected.finally.push(f);
        }
        if let Some(&body) = children.first() {
            self.visit(body, &protected);
        }
        for &handler in children.iter().skip(1) {
            let previous = self.scope;
            self.scope = self.new_scope(Some(previous), ctx.function, self.span(handler));
            let mut c = protected.clone();
            if self.kind(handler) == HirKind::Catch {
                c.catch = true;
                if let Some(t) = self.name(handler)
                    && let Some(s) = self.declare(Declaration {
                        name: t.name.unwrap(),
                        span: t.span,
                        kind: SymbolKind::Catch,
                        mutable: false,
                        ty: ANY,
                        owner: ctx.function,
                        iteration: ctx.iteration,
                    })
                {
                    self.initialized[s.0 as usize] = true;
                    self.out.facts[handler.0 as usize].symbol = Some(s);
                }
            } else {
                c = ctx.clone();
                c.no_wait = true;
                c.cleanup_boundary = Some(c.loops.len());
            }
            for body in self.children(handler) {
                self.visit(body, &c);
            }
            self.scope = previous;
        }
        NULL
    }
    pub(super) fn jump(&mut self, n: NodeId, children: &[NodeId], ctx: &Context) {
        let mut target = None;
        let mut cleanup = ctx.finally.iter().rev().copied().collect::<Vec<_>>();
        match self.kind(n) {
            HirKind::Return => {
                if let Some(default) = ctx.default_return_boundary {
                    let previous_diagnostics = self.diagnostics.len();
                    let boundary = self.span(default);
                    self.error(
                        D::InvalidDefault,
                        self.span(n),
                        "explicit return cannot leave a parameter default expression",
                    );
                    if self.diagnostics.len() > previous_diagnostics {
                        self.diagnostics
                            .last_mut()
                            .unwrap()
                            .related
                            .push(RelatedRange {
                                range: boundary,
                                message: "parameter default expression boundary".into(),
                            });
                    }
                    for &child in children {
                        self.visit(child, ctx);
                    }
                    // An invalid source return does not target the declaring
                    // function or get checked against its return contract.
                    self.out.facts[n.0 as usize].control = Some(ControlTransfer {
                        target: None,
                        finally: cleanup,
                    });
                    return;
                }
                if ctx.function.is_none() {
                    self.error(
                        D::InvalidControl,
                        self.span(n),
                        "return requires a function",
                    );
                }
                if ctx.cleanup_boundary.is_some() {
                    self.error(
                        D::InvalidControl,
                        self.span(n),
                        "return cannot leave finally",
                    );
                }
                if let Some(f) = ctx.function {
                    target = Some(self.out.functions[f.0 as usize].node);
                    let expected = self.out.functions[f.0 as usize].return_type;
                    let t = children
                        .first()
                        .map_or(NULL, |&c| self.visit_expected(c, ctx, expected));
                    self.require(
                        children.first().copied().unwrap_or(n),
                        t,
                        expected,
                        "return",
                    );
                } else {
                    for &c in children {
                        self.visit(c, ctx);
                    }
                }
            }
            HirKind::Break | HirKind::Continue => {
                let label = self.name(n).and_then(|t| t.name);
                let found = ctx
                    .loops
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, (_, l, _))| label.is_none() || *l == label);
                if let Some((index, (node, _, depth))) = found {
                    target = Some(*node);
                    cleanup = ctx.finally[*depth..].iter().rev().copied().collect();
                    if ctx
                        .cleanup_boundary
                        .is_some_and(|boundary| index < boundary)
                    {
                        self.error(D::InvalidControl, self.span(n), "jump cannot leave finally");
                    }
                } else {
                    self.error(
                        D::InvalidControl,
                        self.span(n),
                        "jump requires a matching enclosing loop",
                    );
                }
            }
            HirKind::Throw => {
                if children.is_empty() && !ctx.catch {
                    self.error(
                        D::InvalidControl,
                        self.span(n),
                        "throw without a value requires catch",
                    );
                }
                for &c in children {
                    self.visit(c, ctx);
                }
            }
            _ => (),
        }
        self.out.facts[n.0 as usize].control = Some(ControlTransfer {
            target,
            finally: cleanup,
        });
    }
    pub(super) fn root_symbol(&self, n: NodeId) -> Option<SymbolId> {
        if let Some(s) = self.out.facts[n.0 as usize].symbol {
            return Some(s);
        }
        match self.out.hir.node(n).form {
            Form::Member { object, .. }
            | Form::Index { object, .. }
            | Form::Single { child: object } => self.root_symbol(object),
            _ => None,
        }
    }
    pub(super) fn is_fresh(&self, n: NodeId) -> bool {
        if matches!(self.kind(n), HirKind::Object | HirKind::Array) {
            return true;
        }
        if let Some(s) = self.out.facts[n.0 as usize].symbol {
            return self.fresh.contains(&s);
        }
        if let Form::Single { child } = self.out.hir.node(n).form {
            return self.is_fresh(child);
        }
        false
    }
    pub(super) fn constant_bool(&self, n: NodeId) -> Option<bool> {
        match self.out.hir.node(n).form {
            Form::Literal { token } => match token.kind {
                K::True => Some(true),
                K::False => Some(false),
                _ => None,
            },
            Form::Single { child } => self.constant_bool(child),
            Form::Unary { operator, value } if operator.kind == K::Bang => {
                self.constant_bool(value).map(|b| !b)
            }
            _ => None,
        }
    }
    pub(super) fn definitely_nonnull(&self, ty: TypeId) -> bool {
        match &self.out.types[ty.0 as usize] {
            Type::Any | Type::Null => false,
            Type::Union(types) => types.iter().all(|&ty| self.definitely_nonnull(ty)),
            _ => true,
        }
    }
    pub(super) fn falls_through(&self, n: NodeId) -> bool {
        match self.out.hir.node(n).form {
            Form::Transfer { .. } => !matches!(
                self.kind(n),
                HirKind::Return | HirKind::Throw | HirKind::Break | HirKind::Continue
            ),
            Form::Statement { expression, .. } | Form::Single { child: expression }
                if self.kind(n) != HirKind::Loop =>
            {
                self.falls_through(expression)
            }
            Form::Block { .. } | Form::Sequence
                if matches!(self.kind(n), HirKind::Block | HirKind::Module) =>
            {
                self.out.hir.children(n).all(|c| self.falls_through(c))
            }
            Form::Condition {
                condition,
                body,
                otherwise,
                ..
            } if self.kind(n) == HirKind::If => match self.constant_bool(condition) {
                Some(true) => self.falls_through(body),
                Some(false) => otherwise.is_none_or(|body| self.falls_through(body)),
                None => {
                    self.falls_through(body)
                        || otherwise.is_none_or(|body| self.falls_through(body))
                }
            },
            Form::Condition {
                condition, body, ..
            } if self.kind(n) == HirKind::While => {
                self.constant_bool(condition) != Some(true) || self.can_break_to(body, n)
            }
            Form::Single { child } if self.kind(n) == HirKind::Loop => self.can_break_to(child, n),
            Form::Try {
                body,
                catch,
                finally,
            } => {
                finally.is_none_or(|cleanup| self.falls_through(cleanup))
                    && (self.falls_through(body)
                        || catch.is_some_and(|catch| self.falls_through(catch)))
            }
            Form::Catch { body, .. } | Form::Label { body, .. } => self.falls_through(body),
            Form::Match { .. } => self
                .out
                .hir
                .children(n)
                .skip(1)
                .any(|arm| self.falls_through(arm)),
            Form::MatchArm { body, .. } => self.falls_through(body),
            _ => true,
        }
    }
    pub(super) fn can_break_to(&self, n: NodeId, target: NodeId) -> bool {
        if self.kind(n) == HirKind::Break {
            return self.out.facts[n.0 as usize]
                .control
                .as_ref()
                .is_some_and(|flow| flow.target == Some(target));
        }
        if matches!(
            self.kind(n),
            HirKind::Function | HirKind::Closure | HirKind::Class
        ) {
            return false;
        }
        if let Form::Condition {
            condition,
            body,
            otherwise,
            ..
        } = self.out.hir.node(n).form
            && self.kind(n) == HirKind::If
        {
            return match self.constant_bool(condition) {
                Some(true) => self.can_break_to(body, target),
                Some(false) => otherwise.is_some_and(|body| self.can_break_to(body, target)),
                None => {
                    self.can_break_to(body, target)
                        || otherwise.is_some_and(|body| self.can_break_to(body, target))
                }
            };
        }
        if let Form::Try {
            finally: Some(finally),
            ..
        } = self.out.hir.node(n).form
            && !self.falls_through(finally)
        {
            return self.can_break_to(finally, target);
        }
        for child in self.out.hir.children(n) {
            if self.can_break_to(child, target) {
                return true;
            }
            if matches!(self.kind(n), HirKind::Block | HirKind::Module)
                && !self.falls_through(child)
            {
                break;
            }
        }
        false
    }
}
