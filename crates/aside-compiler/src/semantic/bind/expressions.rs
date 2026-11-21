//! Expression binding and type propagation.
use super::*;

impl Binder<'_> {
    pub(super) fn visit(&mut self, n: NodeId, ctx: &Context) -> TypeId {
        self.visit_expected(n, ctx, ANY)
    }
    pub(super) fn visit_expected(&mut self, n: NodeId, ctx: &Context, expected: TypeId) -> TypeId {
        if !self.budget(n) {
            return ANY;
        }
        self.out.facts[n.0 as usize].scope = Some(self.scope);
        let children = self.children(n);
        use HirKind as H;
        let ty = match self.kind(n) {
            H::Module | H::Block => return self.block_expected(n, ctx, true, expected),
            H::ModifiedDeclaration => children.first().map_or(NULL, |&c| self.visit(c, ctx)),
            H::Function => return self.function(n, ctx, Receiver::None),
            H::Enum | H::TypeAlias => NULL,
            H::Closure => return self.function_expected(n, ctx, Receiver::None, expected),
            H::Let => {
                let pattern = children.iter().copied().find(|&c| is_pattern(self.kind(c)));
                let annotation = children
                    .iter()
                    .copied()
                    .find(|&c| is_type(self.kind(c)))
                    .map(|c| self.annotation(c, ctx));
                let init = children
                    .iter()
                    .copied()
                    .find(|&c| !is_pattern(self.kind(c)) && !is_type(self.kind(c)));
                let value = init.map_or(ANY, |c| {
                    self.visit_expected(c, ctx, annotation.unwrap_or(ANY))
                });
                if let (Some(init), Some(expected)) = (init, annotation) {
                    self.require(init, value, expected, "let initializer");
                }
                if let Some(p) = pattern {
                    self.bind_pattern(
                        p,
                        annotation.unwrap_or(value),
                        ctx,
                        PatternUse::Existing,
                        self.modifier(n, K::Mut),
                    );
                    if !self.modifier(n, K::Mut)
                        && self.kind(p) == HirKind::NamePattern
                        && let Some(initializer) = init
                        && let Some(descriptor) = self.external_nodes.get(&initializer).cloned()
                        && let Some(symbol) = self.out.facts[p.0 as usize].symbol
                    {
                        // Keep typing and navigation while execution retains the
                        // alias's own binding, module slot and initialized state.
                        self.external.insert(
                            symbol,
                            ExternalDescriptor {
                                kind: ExternalBindingKind::Metadata,
                                ..descriptor
                            },
                        );
                    }
                    if let Some(init) = init
                        && self.is_fresh(init)
                    {
                        for name in self.pattern_names(p) {
                            if let Some(s) = self.lookup(name.name) {
                                self.fresh.insert(s);
                            }
                        }
                    }
                    if annotation.is_some() {
                        for name in self.pattern_names(p) {
                            if let Some(s) = self.lookup(name.name) {
                                self.annotations.insert(s);
                                self.out.symbols[s.0 as usize].ty = self.ty(name.node);
                            }
                        }
                    }
                }
                NULL
            }
            H::Name => return self.name(n).map_or(ANY, |t| self.read(n, t, ctx, false)),
            H::Literal => self.literal(n),
            H::String => {
                for c in children {
                    self.visit(c, ctx);
                }
                STRING
            }
            H::Tuple => {
                let ts = children.into_iter().map(|c| self.visit(c, ctx)).collect();
                self.intern_type(Type::Tuple(ts))
            }
            H::Array => {
                for c in children {
                    self.visit(c, ctx);
                }
                ARRAY
            }
            H::Object => {
                for c in children {
                    self.visit(c, ctx);
                }
                TABLE
            }
            H::Group => {
                let t = children
                    .first()
                    .map_or(ANY, |&c| self.visit_expected(c, ctx, expected));
                if let Some(&c) = children.first()
                    && let Some(ext) = self.external_nodes.get(&c)
                {
                    self.external_nodes.insert(n, ext.clone());
                }
                t
            }
            H::ExpressionStatement
            | H::Interpolation
            | H::Argument
            | H::NamedArgument
            | H::Spread => {
                if let Form::Statement { expression, .. } = self.out.hir.node(n).form
                    && matches!(self.out.hir.node(expression).form, Form::Spread { .. })
                {
                    self.has_children_spread = true;
                }
                let mut result = NULL;
                for c in children {
                    result = self.visit_expected(c, ctx, expected);
                }
                result
            }
            H::ObjectField => {
                let mut result = ANY;
                for c in children {
                    result = self.visit(c, ctx);
                }
                result
            }
            H::Prefix => self.prefix(n, &children, ctx),
            H::Binary | H::Range | H::Member | H::Index | H::Call => self.chain(n, ctx),
            H::Assignment => {
                self.assignment(n, ctx);
                NULL
            }
            H::Children => {
                let mut c = ctx.clone();
                c.no_wait = true;
                for child in children {
                    self.visit(child, &c);
                }
                ARRAY
            }
            H::If => self.condition(n, ctx, false),
            H::While => self.condition(n, ctx, true),
            H::For | H::Loop => self.loop_node(n, None, ctx),
            H::Label => {
                if let Some(&child) = children.first() {
                    self.loop_node(child, self.name(n).and_then(|t| t.name), ctx);
                }
                NULL
            }
            H::Match => self.match_node(n, &children, ctx),
            H::Try => self.try_node(n, &children, ctx),
            H::Return | H::Break | H::Continue | H::Throw => {
                self.jump(n, &children, ctx);
                NULL
            }
            H::Yield => {
                if !ctx.generator {
                    self.error(
                        D::InvalidControl,
                        self.span(n),
                        "yield requires a generator body",
                    );
                }
                if ctx.default || ctx.cleanup_boundary.is_some() {
                    self.error(
                        D::InvalidControl,
                        self.span(n),
                        "yield is forbidden in defaults/finally",
                    );
                }
                for c in children {
                    self.visit(c, ctx);
                }
                NULL
            }
            H::Class => self.class_node(n, &children, ctx),
            H::NamedType
            | H::StringType
            | H::RecordType
            | H::TupleType
            | H::UnionType
            | H::FunctionType
            | H::GroupType => {
                return self.annotation(n, ctx);
            }
            H::Import => NULL,
            _ => {
                for c in children {
                    self.visit(c, ctx);
                }
                ANY
            }
        };
        self.set_ty(n, ty)
    }
    pub(super) fn chain(&mut self, root: NodeId, ctx: &Context) -> TypeId {
        let mut chain = Vec::new();
        let mut node = root;
        loop {
            let receiver = match self.out.hir.node(node).form {
                Form::Binary { left, .. }
                    if matches!(self.kind(node), HirKind::Binary | HirKind::Range) =>
                {
                    left
                }
                Form::Member { object, .. } | Form::Index { object, .. } => object,
                Form::Call { callee, .. } => callee,
                _ => break,
            };
            if node != root {
                if !self.budget(node) {
                    return ANY;
                }
                self.out.facts[node.0 as usize].scope = Some(self.scope);
            }
            chain.push(node);
            node = receiver;
        }
        let mut ty = self.visit(node, ctx);
        let refinement_mark = self.refinement_undo.len();
        let mut boolean_path: Option<(K, NodeId)> = None;
        for node in chain.into_iter().rev() {
            let children = self.children(node);
            ty = match self.kind(node) {
                HirKind::Binary | HirKind::Range => {
                    let op = self.tokens(node).first().map(|token| token.kind);
                    if let Some(op @ (K::AndAnd | K::OrOr)) = op {
                        let condition = match boolean_path.take() {
                            Some((previous, last)) if previous == op => last,
                            _ => {
                                self.restore_refinements(refinement_mark);
                                children[0]
                            }
                        };
                        // A left-associated chain accumulates each guard once.
                        self.refine_condition(condition, op == K::AndAnd);
                        let result = self.binary(node, &children, ty, ctx);
                        boolean_path = Some((op, children[1]));
                        result
                    } else {
                        boolean_path = None;
                        self.restore_refinements(refinement_mark);
                        self.binary(node, &children, ty, ctx)
                    }
                }
                HirKind::Member => self.member(node, &children, ty),
                HirKind::Index => self.index(node, &children, ty, ctx),
                HirKind::Call => self.call(node, ty, ctx),
                _ => unreachable!(),
            };
            self.set_ty(node, ty);
        }
        self.restore_refinements(refinement_mark);
        ty
    }
    pub(super) fn index(
        &mut self,
        n: NodeId,
        children: &[NodeId],
        object: TypeId,
        ctx: &Context,
    ) -> TypeId {
        let mut index_ctx = ctx.clone();
        let optional = matches!(
            self.out.hir.node(n).form,
            Form::Index { optional: true, .. } | Form::Index { chain: true, .. }
        );
        index_ctx.conditional |= optional && !self.definitely_nonnull(object);
        let index = children.get(1).map_or(ANY, |&c| self.visit(c, &index_ctx));
        if optional && object == NULL {
            return self.set_ty(n, NULL);
        }
        match self.out.types[object.0 as usize].clone() {
            Type::Tuple(ts) => {
                if let Some(&c) = children.get(1) {
                    self.require(c, index, INT, "tuple index");
                }
                ts.into_iter()
                    .reduce(|a, b| if a == b { a } else { ANY })
                    .unwrap_or(ANY)
            }
            Type::Array => {
                if let Some(&c) = children.get(1) {
                    self.require(c, index, INT, "array index");
                }
                ANY
            }
            Type::Class(_) | Type::Instance(_) => {
                if let Some(&c) = children.get(1) {
                    self.require(c, index, STRING, "class member name");
                }
                self.check(n, RuntimeChecks::TYPE);
                ANY
            }
            Type::String | Type::StringLiteral(_) => {
                self.error(
                    D::TypeMismatch,
                    self.span(n),
                    "string has no ordinary index; use an explicit text view",
                );
                ANY
            }
            _ => {
                self.check(n, RuntimeChecks::TYPE);
                ANY
            }
        }
    }
    pub(super) fn literal(&self, n: NodeId) -> TypeId {
        for token in self.tokens(n) {
            match token.kind {
                K::Null => return NULL,
                K::True | K::False => return BOOL,
                K::Number => {
                    let text = self.out.hir.text(token.span);
                    let lower = text.to_ascii_lowercase();
                    return if !lower.starts_with("0x")
                        && !lower.starts_with("0o")
                        && !lower.starts_with("0b")
                        && text.contains(['.', 'e', 'E'])
                    {
                        FLOAT
                    } else {
                        INT
                    };
                }
                _ => (),
            }
        }
        ANY
    }
    pub(super) fn prefix(&mut self, n: NodeId, children: &[NodeId], ctx: &Context) -> TypeId {
        let t = children.first().map_or(ANY, |&c| self.visit(c, ctx));
        let op = self.tokens(n).first().map(|t| t.kind);
        match op {
            Some(K::Await) => {
                if ctx.no_wait
                    && (!ctx.conditional || ctx.generator || ctx.cleanup_boundary.is_some())
                {
                    self.error(
                        D::NoWait,
                        self.span(n),
                        "await is forbidden in this execution context",
                    );
                }
                if ctx.no_wait {
                    self.check(n, RuntimeChecks::NO_WAIT);
                }
                if let Some(f) = ctx.function {
                    self.out.functions[f.0 as usize].known_wait = true;
                }
                if is_primitive(&self.out.types[t.0 as usize]) {
                    self.error(
                        D::TypeMismatch,
                        self.span(n),
                        "await requires an Awaitable. Use an ordinary call result directly",
                    );
                } else if t == ANY
                    && let Some(&operand) = children.first()
                    && self.call_result_can_be_inferred(operand)
                {
                    self.out.inferred_awaits.push(n);
                }
                self.check(n, RuntimeChecks::TYPE);
                if let Some(name) = self.nullable_named_type(t) {
                    if !self.await_types.contains_key(&name) {
                        let hint = self.resolver.resolve_type_await(self.out.hir.name(name));
                        let result = hint.as_ref().map(|hint| self.hint(hint));
                        self.await_types.insert(name, result);
                    }
                    if let Some(result) = self.await_types[&name] {
                        return result;
                    }
                }
                ANY
            }
            Some(K::Bang) => {
                if let Some(&c) = children.first() {
                    self.require(c, t, BOOL, "logical negation");
                }
                BOOL
            }
            Some(K::Tilde) => {
                if let Some(&c) = children.first() {
                    self.require(c, t, INT, "bitwise negation");
                }
                INT
            }
            Some(K::Plus | K::Minus) => {
                if t == INT || t == FLOAT {
                    t
                } else if is_primitive(&self.out.types[t.0 as usize]) {
                    self.error(
                        D::TypeMismatch,
                        self.span(n),
                        "numeric unary operator requires a number",
                    );
                    ANY
                } else {
                    self.check(n, RuntimeChecks::TYPE);
                    ANY
                }
            }
            _ => ANY,
        }
    }
    fn call_result_can_be_inferred(&self, mut operand: NodeId) -> bool {
        while let (HirKind::Group, Form::Single { child }) =
            (self.kind(operand), self.out.hir.node(operand).form)
        {
            operand = child;
        }
        let Form::Call { callee, .. } = self.out.hir.node(operand).form else {
            return false;
        };
        matches!(
            self.out.types[self.ty(callee).0 as usize],
            Type::Function(_) | Type::BoundMethod(_)
        ) || self.external_nodes.get(&callee).is_some_and(|value| {
            value
                .reference
                .as_ref()
                .is_some_and(|reference| reference.export.is_some())
        })
    }

    pub(super) fn binary(
        &mut self,
        n: NodeId,
        children: &[NodeId],
        a: TypeId,
        ctx: &Context,
    ) -> TypeId {
        let op = self.tokens(n).first().map(|t| t.kind);
        let mut right_ctx = ctx.clone();
        right_ctx.conditional |= match op {
            Some(K::AndAnd) => children.first().and_then(|&n| self.constant_bool(n)) != Some(true),
            Some(K::OrOr) => children.first().and_then(|&n| self.constant_bool(n)) != Some(false),
            Some(K::QuestionQuestion) => a != NULL,
            _ => false,
        };
        let b = children.get(1).map_or(ANY, |&c| self.visit(c, &right_ctx));
        match op {
            Some(K::Is) => BOOL,
            Some(K::EqEq | K::NotEq) => BOOL,
            Some(K::AndAnd | K::OrOr) => {
                for (&c, t) in children.iter().zip([a, b]) {
                    self.require(c, t, BOOL, "logical operand");
                }
                BOOL
            }
            Some(K::QuestionQuestion) => self.coalesced_type(n, a, b),
            Some(K::Range | K::RangeInclusive) => {
                for (&c, t) in children.iter().zip([a, b]) {
                    self.require(c, t, INT, "range bound");
                }
                let name = self.intern_name("Range");
                self.intern_type(Type::Named(name))
            }
            Some(K::Shl | K::Shr | K::Amp | K::Pipe | K::Caret) => {
                for (&c, t) in children.iter().zip([a, b]) {
                    self.require(c, t, INT, "bitwise operand");
                }
                INT
            }
            Some(K::Lt | K::Le | K::Gt | K::Ge) => {
                if is_primitive(&self.out.types[a.0 as usize])
                    && is_primitive(&self.out.types[b.0 as usize])
                    && !((is_number(a) && is_number(b))
                        || (self.string_like(a) && self.string_like(b)))
                {
                    self.error(
                        D::TypeMismatch,
                        self.span(n),
                        "incompatible comparison operands",
                    );
                }
                BOOL
            }
            Some(K::Plus | K::Minus | K::Star | K::Slash | K::Percent) => {
                if is_number(a) && is_number(b) {
                    if a == FLOAT || b == FLOAT { FLOAT } else { INT }
                } else if op == Some(K::Plus) && self.string_like(a) && self.string_like(b) {
                    STRING
                } else if is_primitive(&self.out.types[a.0 as usize])
                    && is_primitive(&self.out.types[b.0 as usize])
                {
                    self.error(
                        D::TypeMismatch,
                        self.span(n),
                        "incompatible arithmetic operands",
                    );
                    ANY
                } else {
                    self.check(n, RuntimeChecks::TYPE);
                    ANY
                }
            }
            _ => ANY,
        }
    }
    pub(super) fn assignment(&mut self, n: NodeId, ctx: &Context) -> TypeId {
        let children = self.children(n);
        let Some((&target, rest)) = children.split_first() else {
            return ANY;
        };
        self.location(target, ctx);
        let rhs = rest.first().copied();
        let expected = self
            .member_write_types
            .get(&target)
            .copied()
            .flatten()
            .unwrap_or(self.ty(target));
        let value = rhs.map_or(ANY, |c| {
            if self.kind(c) == HirKind::Assignment {
                self.assignment(c, ctx)
            } else {
                self.visit_expected(c, ctx, expected)
            }
        });
        if is_pattern(self.kind(target)) {
            self.pattern_names(target);
            self.check(target, RuntimeChecks::PATTERN);
        }
        self.pattern_type(target, value, PatternUse::Assignment);
        if let Some(Some(expected)) = self.member_write_types.get(&target).copied()
            && let Some(rhs) = rhs
        {
            self.require(rhs, value, expected, "property assignment");
        } else if self.kind(target) == HirKind::Name
            && let Some(symbol) = self.out.facts[target.0 as usize].symbol
            && let Some(rhs) = rhs
        {
            let contract = self.out.symbols[symbol.0 as usize].ty;
            if self.has_value_contract(contract) {
                self.require(rhs, value, contract, "assignment");
            }
        }
        value
    }
}
