//! Function signatures, defaults and body binding.
use super::*;

impl Binder<'_> {
    pub(super) fn make_function(
        &mut self,
        n: NodeId,
        ctx: &Context,
        receiver: Receiver,
    ) -> FunctionId {
        if let Some(&id) = self.fn_nodes.get(&n) {
            return id;
        }
        let id = FunctionId(self.out.functions.len() as u32);
        let (body, kind) = match self.out.hir.node(n).form {
            Form::Function { body, kind, .. } => (body, kind),
            Form::Binding {
                initializer: Some(body),
                ..
            } => (body, FunctionKind::Plain),
            // A source fragment has an embedding-supplied function owner;
            // its Module node is the synthetic body's exact source range.
            _ if n == NodeId(0) => (n, FunctionKind::Plain),
            _ => unreachable!("function nodes have a function form"),
        };
        self.out.functions.push(Function {
            node: n,
            parent: ctx.function,
            kind,
            receiver,
            role: FunctionRole::Body,
            declared_context: DeclaredContext::default(),
            lexical_class: self.class.map(|(c, _)| c),
            parameters: Vec::new(),
            return_type: ANY,
            body,
            captures: Vec::new(),
            known_wait: false,
            dynamic_calls: false,
        });
        self.capture_maps.push(HashMap::new());
        self.fn_nodes.insert(n, id);
        self.out.facts[n.0 as usize].function = Some(id);
        id
    }
    pub(super) fn prepare_function(
        &mut self,
        n: NodeId,
        ctx: &Context,
        receiver: Receiver,
    ) -> TypeId {
        if !self.budget(n) {
            return ANY;
        }
        let f = self.make_function(n, ctx, receiver);
        self.out.functions[f.0 as usize].receiver = receiver;
        let ty = self.intern_type(Type::Function(f));
        if self.signature_ready.contains(&f) {
            return self.set_ty(n, ty);
        }
        if receiver == Receiver::ExplicitSelf
            && self.out.functions[f.0 as usize].kind == FunctionKind::Generator
            && let Some(name) = self.name(n)
            && self.out.hir.name(name.name.unwrap()) == "init"
        {
            self.error(
                D::InvalidControl,
                name.span,
                "class initializers cannot be generators",
            );
        }
        let previous = self.scope;
        self.scope = self.new_scope(Some(previous), Some(f), self.span(n));
        self.out.facts[n.0 as usize].scope = Some(self.scope);
        let inner = self.function_context(f, n, receiver);
        self.out.functions[f.0 as usize].declared_context = DeclaredContext {
            no_wait: inner.no_wait,
            init: inner.init,
            derived: inner.derived,
        };
        let Form::Function {
            parameters,
            result: return_annotation,
            ..
        } = self.out.hir.node(n).form
        else {
            unreachable!()
        };
        let params = parameters.map(|p| self.children(p)).unwrap_or_default();
        if receiver == Receiver::ExplicitSelf
            && params
                .first()
                .and_then(|&p| self.name(p))
                .map(|t| self.out.hir.name(t.name.unwrap()))
                != Some("self")
        {
            self.error(
                D::InvalidReceiver,
                self.span(n),
                "instance methods require explicit first parameter self",
            );
        }
        if receiver == Receiver::Static
            && params
                .first()
                .and_then(|&p| self.name(p))
                .map(|t| self.out.hir.name(t.name.unwrap()))
                == Some("self")
        {
            self.error(
                D::InvalidReceiver,
                self.span(n),
                "static methods do not receive self",
            );
        }
        for (index, p) in params.into_iter().enumerate() {
            if self.exhausted {
                break;
            }
            let Some(token) = self.name(p) else {
                continue;
            };
            let name = token.name.unwrap();
            let Form::Parameter {
                annotation,
                default,
                ..
            } = self.out.hir.node(p).form
            else {
                continue;
            };
            let annotation = annotation.map(|a| self.annotation(a, &inner));
            let mut pty = annotation.unwrap_or(ANY);
            if self.modifier(p, K::Ellipsis) {
                self.require(p, ARRAY, pty, "rest array binding");
            }
            if receiver == Receiver::ExplicitSelf && index == 0 {
                if let Some((class, _)) = self.class {
                    pty = self.intern_type(Type::Instance(class));
                }
                if default.is_some() || self.modifier(p, K::Ellipsis) {
                    self.error(
                        D::InvalidReceiver,
                        token.span,
                        "self cannot have a default or rest modifier",
                    );
                }
            }
            if let Some(s) = self.declare(Declaration {
                name,
                span: token.span,
                kind: if receiver == Receiver::ExplicitSelf && index == 0 {
                    SymbolKind::Receiver
                } else {
                    SymbolKind::Parameter
                },
                mutable: self.modifier(p, K::Mut),
                ty: pty,
                owner: Some(f),
                iteration: false,
            }) {
                self.initialized[s.0 as usize] = true;
                self.out.facts[p.0 as usize].symbol = Some(s);
                if annotation.is_some() {
                    self.annotations.insert(s);
                }
                let rest = self.modifier(p, K::Ellipsis);
                self.out.functions[f.0 as usize].parameters.push(Parameter {
                    symbol: s,
                    annotation,
                    default,
                    rest,
                });
            }
        }
        if receiver == Receiver::ExplicitSelf && self.class.is_some_and(|(_, derived)| derived) {
            let name = self.intern_name("super");
            if let Some(s) = self.declare(Declaration {
                name,
                span: self.span(n),
                kind: SymbolKind::Super,
                mutable: false,
                ty: ANY,
                owner: Some(f),
                iteration: false,
            }) {
                self.initialized[s.0 as usize] = true;
            }
        }
        if let Some(ret) = return_annotation {
            let result = self.annotation(ret, &inner);
            self.out.functions[f.0 as usize].return_type = result;
        }
        self.signature_ready.insert(f);
        self.scope = previous;
        self.set_ty(n, ty)
    }
    pub(super) fn function_context(&self, f: FunctionId, n: NodeId, receiver: Receiver) -> Context {
        let kind = self.out.functions[f.0 as usize].kind;
        let method_name = self.name(n).map(|t| self.out.hir.name(t.name.unwrap()));
        let restricted_method = receiver == Receiver::ExplicitSelf
            && method_name.is_some_and(|s| s == "init" || (s.starts_with('_') && s != "_call"));
        // Function definitions reset dynamic execution context; constructing a
        // closure in children/finally does not constrain its eventual body.
        Context {
            function: Some(f),
            generator: kind == FunctionKind::Generator,
            no_wait: kind == FunctionKind::Generator
                || restricted_method
                || self.derived_functions.contains(&n),
            derived: self.derived_functions.contains(&n),
            ..Default::default()
        }
    }
    pub(super) fn function(&mut self, n: NodeId, ctx: &Context, receiver: Receiver) -> TypeId {
        if self.exhausted {
            return ANY;
        }
        let ty = self.prepare_function(n, ctx, receiver);
        if self.exhausted {
            return ANY;
        }
        let f = self.fn_nodes[&n];
        if !self.analyzed_functions.insert(f) {
            return self.set_ty(n, ty);
        }
        let previous = self.scope;
        self.scope = self.out.facts[n.0 as usize]
            .scope
            .expect("prepared function scope");
        let inner = self.function_context(f, n, receiver);
        for index in 0..self.out.functions[f.0 as usize].parameters.len() {
            if self.exhausted {
                break;
            }
            let parameter = &self.out.functions[f.0 as usize].parameters[index];
            let (symbol, default, annotation) =
                (parameter.symbol, parameter.default, parameter.annotation);
            if let Some(default) = default {
                self.default_boundaries.push((f, symbol));
                let mut default_ctx = inner.clone();
                default_ctx.default = true;
                default_ctx.default_return_boundary = Some(default);
                default_ctx.no_wait = true;
                let actual = self.visit(default, &default_ctx);
                if let Some(expected) = annotation {
                    self.require(default, actual, expected, "parameter default");
                }
                self.default_boundaries.pop();
            }
        }
        let body = self.out.functions[f.0 as usize].body;
        if body != n {
            let result = if self.kind(body) == HirKind::Block {
                self.block(body, &inner, false)
            } else {
                self.visit(body, &inner)
            };
            let expected = self.out.functions[f.0 as usize].return_type;
            // Explicit terminal transfers do not create an implicit null return.
            if self.falls_through(body) {
                self.require(body, result, expected, "function result");
            }
        }
        self.scope = previous;
        self.set_ty(n, ty)
    }
}
