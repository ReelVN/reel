//! Class and import declarations, effect and call validation.
use super::*;

impl Binder<'_> {
    pub(super) fn prepare_class(&mut self, n: NodeId, ctx: &Context) {
        if !self.prepared_classes.insert(n) {
            return;
        }
        let children = self.children(n);
        let base = children
            .iter()
            .copied()
            .find(|&c| !matches!(self.kind(self.unwrap(c)), HirKind::Let | HirKind::Function));
        if let Some(base) = base
            && let Form::Name { name } = self.out.hir.node(base).form
            && let Some(symbol) = name.name.and_then(|name| self.lookup(name))
            && let Type::Class(parent) =
                self.out.types[self.out.symbols[symbol.0 as usize].ty.0 as usize]
        {
            self.class_bases.insert(n, parent);
        }
        let old_class = self.class;
        self.class = Some((n, base.is_some()));
        let mut seen = HashSet::new();
        for &member in &children {
            if !self.budget(member) {
                break;
            }
            let d = self.unwrap(member);
            if !matches!(self.kind(d), HirKind::Function | HirKind::Let) {
                continue;
            }
            let name = if self.kind(d) == HirKind::Let {
                self.children(d)
                    .into_iter()
                    .find(|&c| is_pattern(self.kind(c)))
                    .and_then(|c| self.name(c))
            } else {
                self.name(d)
            };
            if let Some(name) = name {
                let static_member = self.modifier(member, K::Static);
                self.class_members
                    .entry(n)
                    .or_default()
                    .insert((name.name.unwrap(), static_member), d);
            }
            if let Some(name) = name
                && !seen.insert((name.name, self.modifier(member, K::Static)))
            {
                self.error(D::DuplicateBinding, name.span, "duplicate class member");
            }
            if self.kind(d) == HirKind::Function {
                self.make_function(
                    d,
                    ctx,
                    if self.modifier(member, K::Static) {
                        Receiver::Static
                    } else {
                        Receiver::ExplicitSelf
                    },
                );
            } else if let Form::Binding {
                annotation: Some(annotation),
                ..
            } = self.out.hir.node(d).form
            {
                // Fields are visible to methods regardless of declaration order.
                let ty = self.annotation(annotation, ctx);
                self.set_ty(d, ty);
            }
        }
        for &member in &children {
            if self.exhausted {
                break;
            }
            let d = self.unwrap(member);
            if self.kind(d) == HirKind::Function {
                self.prepare_function(
                    d,
                    ctx,
                    if self.modifier(member, K::Static) {
                        Receiver::Static
                    } else {
                        Receiver::ExplicitSelf
                    },
                );
            }
        }
        self.class = old_class;
    }
    pub(super) fn class_node(&mut self, n: NodeId, children: &[NodeId], ctx: &Context) -> TypeId {
        self.prepare_class(n, ctx);
        if let Form::Class {
            base: Some(base), ..
        } = self.out.hir.node(n).form
        {
            // Declaration execution, rather than signature preparation, owns
            // initialization and base-value diagnostics.
            let value = self.visit(base, ctx);
            if let Type::Class(parent) = self.out.types[value.0 as usize] {
                self.class_bases.insert(n, parent);
            }
            if matches!(self.out.types[value.0 as usize],Type::Class(class) if class==n) {
                self.error(
                    D::TypeMismatch,
                    self.span(base),
                    "class cannot inherit itself",
                );
            }
            if is_primitive(&self.out.types[value.0 as usize]) {
                self.error(D::TypeMismatch, self.span(base), "base must be a class");
            }
        }
        let old_class = self.class;
        self.class = Some((n, self.class_bases.contains_key(&n)));
        for &member in children {
            if !self.budget(member) {
                break;
            }
            let d = self.unwrap(member);
            match self.kind(d) {
                HirKind::Function => {
                    self.function(
                        d,
                        ctx,
                        if self.modifier(member, K::Static) {
                            Receiver::Static
                        } else {
                            Receiver::ExplicitSelf
                        },
                    );
                }
                HirKind::Let => {
                    let Form::Binding {
                        initializer,
                        annotation,
                        mutable,
                        ..
                    } = self.out.hir.node(d).form
                    else {
                        continue;
                    };
                    if let Some(init) = initializer {
                        let previous = self.scope;
                        let f = self.make_function(d, ctx, Receiver::None);
                        self.out.functions[f.0 as usize].role = FunctionRole::FieldInitializer;
                        self.out.functions[f.0 as usize].declared_context.no_wait = true;
                        self.out.functions[f.0 as usize].body = init;
                        self.scope = self.new_scope(Some(previous), Some(f), self.span(d));
                        let c = Context {
                            function: Some(f),
                            no_wait: true,
                            // This synthetic initializer is not a source fn.
                            default_return_boundary: ctx.default_return_boundary,
                            ..Default::default()
                        };
                        if !self.modifier(member, K::Static) {
                            let self_name = self.intern_name("self");
                            let instance = self.intern_type(Type::Instance(n));
                            if let Some(s) = self.declare(Declaration {
                                name: self_name,
                                span: self.span(d),
                                kind: SymbolKind::Receiver,
                                mutable: false,
                                ty: instance,
                                owner: Some(f),
                                iteration: false,
                            }) {
                                self.initialized[s.0 as usize] = true;
                                self.out.functions[f.0 as usize].parameters.push(Parameter {
                                    symbol: s,
                                    annotation: None,
                                    default: None,
                                    rest: false,
                                });
                            }
                        }
                        let annotation = annotation.map(|a| self.annotation(a, &c));
                        let t = self.visit_expected(init, &c, annotation.unwrap_or(ANY));
                        if let Some(expected) = annotation {
                            self.require(init, t, expected, "field initializer");
                        }
                        self.set_ty(d, annotation.unwrap_or(if mutable { ANY } else { t }));
                        self.scope = previous;
                    } else if let Some(a) = annotation {
                        self.annotation(a, ctx);
                    }
                }
                _ => (),
            }
        }
        self.class = old_class;
        if let Some(s) = self.out.facts[n.0 as usize].symbol {
            self.initialized[s.0 as usize] = true;
        }
        self.intern_type(Type::Class(n))
    }
    pub(super) fn import(&mut self, n: NodeId, public: bool, ctx: &Context) {
        if let Some(path) = self.children(n).first().copied() {
            self.import_path(n, path, &[], false, public, ctx);
        }
    }
    pub(super) fn import_path(
        &mut self,
        root: NodeId,
        n: NodeId,
        prefix: &[String],
        grouped: bool,
        public: bool,
        ctx: &Context,
    ) {
        if !self.budget(n) {
            return;
        }
        let mut path = prefix.to_vec();
        let mut alias = None;
        let mut after_as = false;
        let mut wildcard = false;
        for t in self.tokens(n) {
            match t.kind {
                K::As => after_as = true,
                K::Ident => {
                    let text = self.out.hir.name(t.name.unwrap()).to_owned();
                    if after_as {
                        alias = Some(text);
                    } else {
                        path.push(text);
                    }
                }
                K::Star => wildcard = true,
                _ => (),
            }
        }
        if let Some(group) = self
            .children(n)
            .into_iter()
            .find(|&c| self.kind(c) == HirKind::ImportGroup)
        {
            for child in self.children(group) {
                self.import_path(root, child, &path, true, public, ctx);
            }
            return;
        }
        if self.out.imports.len() >= self.options.limits.max_imports {
            self.error(D::BudgetExceeded, self.span(n), "import budget exceeded");
            self.exhausted = true;
            return;
        }
        let request = ImportRequest {
            from: self.options.module.clone(),
            path,
            kind: if wildcard {
                ImportKind::Wildcard
            } else if grouped {
                ImportKind::Symbol
            } else {
                ImportKind::NamespaceOrSymbol
            },
        };
        match self.resolve(&request) {
            Err(error) => self.error(D::ModuleResolution, self.span(n), error.to_string()),
            Ok(import) => {
                let mut names = Vec::new();
                for ext in import.bindings {
                    let local = alias.as_deref().unwrap_or(&ext.name);
                    let name = self.intern_name(local);
                    let ty = if ext.intrinsic == Some(Intrinsic::TypeAlias) {
                        ANY
                    } else {
                        self.external_type(&ext)
                    };
                    if let Some(s) = self.declare(Declaration {
                        name,
                        span: self.span(n),
                        kind: SymbolKind::Import,
                        mutable: false,
                        ty,
                        owner: ctx.function,
                        iteration: false,
                    }) {
                        self.initialized[s.0 as usize] = true;
                        self.external.insert(s, ExternalDescriptor::reference(ext));
                        names.push((name, s));
                        if public {
                            self.out.exports.push(s);
                        }
                    }
                }
                self.out.imports.push(Import {
                    node: root,
                    module: import.module,
                    version: import.version,
                    names,
                });
            }
        }
    }
    pub(super) fn effects(&mut self) {
        if self.exhausted {
            return;
        }
        let mut reverse = vec![Vec::new(); self.out.functions.len()];
        let mut queue = VecDeque::new();
        for (i, f) in self.out.functions.iter().enumerate() {
            if f.known_wait {
                queue.push_back(FunctionId(i as u32));
            }
        }
        let calls = std::mem::take(&mut self.calls);
        self.collect_effect_calls(&calls);
        for call in calls {
            if let Some(caller) = call.caller {
                if let Some(callee) = call.callee {
                    if self.out.functions[callee.0 as usize].kind == FunctionKind::Plain {
                        reverse[callee.0 as usize].push(caller);
                    }
                } else if call.effect == CallEffect::MayWait {
                    if !self.out.functions[caller.0 as usize].known_wait {
                        self.out.functions[caller.0 as usize].known_wait = true;
                        queue.push_back(caller);
                    }
                } else if call.effect == CallEffect::Unknown {
                    self.out.functions[caller.0 as usize].dynamic_calls = true;
                }
            }
            // MayWait is a possibility summary. Never reject a NoWait call from
            // this summary: its concrete path may return without external wait.
            if call.no_wait {
                self.check(call.node, RuntimeChecks::NO_WAIT);
            }
            self.validate_call(&call);
        }
        for (i, f) in self.out.functions.iter().enumerate() {
            if f.dynamic_calls {
                queue.push_back(FunctionId(i as u32));
            }
        }
        while let Some(f) = queue.pop_front() {
            let wait = self.out.functions[f.0 as usize].known_wait;
            let dynamic = self.out.functions[f.0 as usize].dynamic_calls;
            for &caller in &reverse[f.0 as usize] {
                let target = &mut self.out.functions[caller.0 as usize];
                if (wait && !target.known_wait) || (dynamic && !target.dynamic_calls) {
                    target.known_wait |= wait;
                    target.dynamic_calls |= dynamic;
                    queue.push_back(caller);
                }
            }
        }
    }

    pub(super) fn checked_signature(
        &mut self,
        call: &CallSite,
        callee: NodeId,
    ) -> Option<Arc<CheckedSignature>> {
        let descriptor = call
            .named_callee
            .and_then(|name| self.callable_types.get(&name).cloned().flatten())
            .or_else(|| self.external_nodes.get(&callee).cloned());
        let key = if let Some(function) = call.callee {
            SignatureKey::Function(function, call.bound)
        } else {
            SignatureKey::External(Arc::as_ptr(&descriptor.as_ref()?.symbol) as usize)
        };
        if let Some(signature) = self.signatures.get(&key) {
            return Some(signature.clone());
        }
        let count = if let Some(function) = call.callee {
            self.out.functions[function.0 as usize].parameters.len()
        } else {
            descriptor.as_ref()?.signature.as_ref()?.parameters.len()
        };
        let bound = usize::from(call.callee.is_some() && call.bound);
        if count == bound {
            return Some(
                self.empty_signature
                    .get_or_insert_with(|| {
                        Arc::new(CheckedSignature {
                            parameters: Vec::new(),
                            named: HashMap::new(),
                            rest: None,
                            required: Vec::new(),
                        })
                    })
                    .clone(),
            );
        }
        if !self.charge_work(call.node, count.saturating_mul(2)) {
            return None;
        }
        let parameters = if let Some(function) = call.callee {
            self.out.functions[function.0 as usize]
                .parameters
                .iter()
                .skip(usize::from(call.bound))
                .map(|parameter| {
                    let symbol = &self.out.symbols[parameter.symbol.0 as usize];
                    CheckedParameter {
                        name: symbol.name,
                        ty: symbol.ty,
                        required: parameter.default.is_none() && !parameter.rest,
                        rest: parameter.rest,
                    }
                })
                .collect::<Vec<_>>()
        } else {
            descriptor
                .as_ref()?
                .signature
                .as_ref()?
                .parameters
                .iter()
                .map(|parameter| CheckedParameter {
                    name: self.intern_name(&parameter.name),
                    ty: self.hint(&parameter.ty),
                    required: !parameter.default && !parameter.rest,
                    rest: parameter.rest,
                })
                .collect()
        };
        let mut named = HashMap::with_capacity(parameters.len());
        let mut required = Vec::new();
        let mut rest = None;
        for (i, parameter) in parameters.iter().enumerate() {
            if !parameter.rest {
                named.insert(parameter.name, i);
            }
            if parameter.required {
                required.push(i);
            }
            if parameter.rest {
                rest = Some(i);
            }
        }
        let signature = Arc::new(CheckedSignature {
            parameters,
            named,
            required,
            rest,
        });
        self.signatures.insert(key, signature.clone());
        Some(signature)
    }
    pub(super) fn validate_call(&mut self, call: &CallSite) {
        let Form::Call {
            callee,
            arguments,
            children: trailing,
            ..
        } = self.out.hir.node(call.node).form
        else {
            return;
        };
        let signature = self.checked_signature(call, callee);
        let mut callable_ty = self.ty(callee);
        if let Type::Union(types) = &self.out.types[callable_ty.0 as usize] {
            let mut candidates = types.iter().copied().filter(|&ty| ty != NULL);
            if let Some(inner) = candidates.next().filter(|_| candidates.next().is_none()) {
                callable_ty = inner;
            }
        }
        let positional = matches!(self.out.types[callable_ty.0 as usize], Type::Callable(..));
        if signature.is_none() && !positional {
            return;
        }
        let mut args = Vec::new();
        for part in arguments.into_iter().chain(trailing) {
            match self.kind(part) {
                HirKind::Arguments => {
                    for arg in self.children(part) {
                        let named = if self.kind(arg) == HirKind::NamedArgument {
                            self.name(arg).and_then(|t| t.name)
                        } else {
                            None
                        };
                        let ty = self.ty(arg);
                        if self.kind(arg) == HirKind::Spread {
                            if let Type::Tuple(elements) = &self.out.types[ty.0 as usize] {
                                let count = elements.len();
                                if !self.charge_work(arg, count) {
                                    return;
                                }
                                let Type::Tuple(elements) = &self.out.types[ty.0 as usize] else {
                                    unreachable!()
                                };
                                args.extend(elements.iter().map(|&ty| (arg, None, ty, false)));
                            } else {
                                args.push((arg, None, ty, true));
                            }
                        } else {
                            args.push((arg, named, ty, false));
                        }
                    }
                }
                HirKind::Children => {
                    let name = self.intern_name("children");
                    args.push((part, Some(name), ARRAY, false));
                }
                _ => (),
            }
        }
        if positional {
            let Type::Callable(parameters, _) = &self.out.types[callable_ty.0 as usize] else {
                unreachable!()
            };
            let parameter_count = parameters.len();
            let mut position = Some(0usize);
            let mut supplied = 0;
            let mut dynamic = false;
            for (arg, name, ty, spread) in args {
                if !self.budget(arg) {
                    return;
                }
                if name.is_some() {
                    self.error(
                        D::InvalidCall,
                        self.span(arg),
                        "function types take positional arguments",
                    );
                } else if spread {
                    dynamic = true;
                    position = None;
                } else {
                    supplied += 1;
                    if let Some(index) = position {
                        let Type::Callable(parameters, _) = &self.out.types[callable_ty.0 as usize]
                        else {
                            unreachable!()
                        };
                        if let Some(expected) = parameters.get(index).copied() {
                            self.require(arg, ty, expected, "call argument");
                        }
                        position = Some(index + 1);
                    }
                }
            }
            if supplied > parameter_count {
                self.error(
                    D::InvalidCall,
                    self.span(call.node),
                    "too many positional arguments",
                );
            } else if !dynamic && supplied < parameter_count {
                self.error(
                    D::InvalidCall,
                    self.span(call.node),
                    "missing required positional argument",
                );
            }
            return;
        }
        let signature = signature.expect("checked ordinary signature");
        let mut used = HashSet::new();
        let mut position = Some(0usize);
        let mut dynamic_spread = false;
        let mut required_supplied = 0usize;
        for (arg, name, ty, unknown_spread) in args {
            if !self.budget(arg) {
                return;
            }
            if unknown_spread {
                dynamic_spread = true;
                position = None;
                self.check(call.node, RuntimeChecks::CALL);
                continue;
            }
            let index = if let Some(name) = name {
                signature.named.get(&name).copied()
            } else if let Some(i) = position {
                position = Some(i + 1);
                if i < signature.parameters.len() {
                    Some(i)
                } else {
                    signature.rest
                }
            } else {
                // An unknown spread can occupy zero or more positions. The
                // concrete slot and collisions remain runtime binding work.
                self.check(call.node, RuntimeChecks::CALL);
                continue;
            };
            if let Some(i) = index {
                let parameter = &signature.parameters[i];
                if !parameter.rest {
                    if !used.insert(i) {
                        self.error(
                            D::InvalidCall,
                            self.span(arg),
                            "parameter supplied more than once",
                        );
                    } else if parameter.required {
                        required_supplied += 1;
                    }
                    self.require(arg, ty, parameter.ty, "call argument");
                }
            } else {
                self.error(
                    D::InvalidCall,
                    self.span(arg),
                    if name.is_some() {
                        "unknown named argument"
                    } else {
                        "too many positional arguments"
                    },
                );
            }
        }
        if !dynamic_spread && required_supplied < signature.required.len() {
            if !self.charge_work(call.node, signature.required.len()) {
                return;
            }
            for &i in &signature.required {
                if !used.contains(&i) {
                    self.error(
                        D::InvalidCall,
                        self.span(call.node),
                        format!(
                            "missing required argument {}",
                            self.out.hir.name(signature.parameters[i].name)
                        ),
                    );
                    if self.diagnostics.len() >= self.options.limits.max_diagnostics.max(1) {
                        break;
                    }
                }
            }
        }
    }
    pub(super) fn account(&mut self) {
        let mut bytes = self.out.hir.source.len()
            + self.out.hir.nodes.capacity() * size_of::<HirNode>()
            + self.out.hir.elements.capacity() * size_of::<HirElement>();
        bytes += self.out.hir.names.capacity() * size_of::<String>()
            + self
                .out
                .hir
                .names
                .iter()
                .map(|n| n.capacity())
                .sum::<usize>();
        bytes += self.out.facts.capacity() * size_of::<NodeFacts>()
            + self.out.symbols.capacity() * size_of::<Symbol>()
            + self.out.scopes.capacity() * size_of::<Scope>();
        bytes += self.out.functions.capacity() * size_of::<Function>()
            + self.out.types.capacity() * size_of::<Type>();
        for s in &self.out.scopes {
            bytes += s.symbols.capacity() * size_of::<SymbolId>();
        }
        for f in &self.out.functions {
            bytes += f.captures.capacity() * size_of::<Capture>()
                + f.parameters.capacity() * size_of::<Parameter>();
        }
        for f in &self.out.facts {
            if let Some(c) = &f.control {
                bytes += c.finally.capacity() * size_of::<NodeId>();
            }
        }
        for t in &self.out.types {
            match t {
                Type::Tuple(v) | Type::Union(v) | Type::Callable(v, _) => {
                    bytes += v.capacity() * size_of::<TypeId>()
                }
                _ => (),
            }
        }
        self.out.external = self
            .external
            .iter()
            .map(|(&symbol, descriptor)| ExternalBinding {
                symbol: Some(symbol),
                node: None,
                descriptor: descriptor.symbol.clone(),
                kind: descriptor.kind,
            })
            .chain(
                self.external_nodes
                    .iter()
                    .map(|(&node, descriptor)| ExternalBinding {
                        symbol: None,
                        node: Some(node),
                        descriptor: descriptor.symbol.clone(),
                        kind: descriptor.kind,
                    }),
            )
            .collect();
        self.out.external.sort_by_key(|e| (e.symbol, e.node));
        self.out.stats = AnalysisStats {
            nodes: self.out.hir.nodes.len(),
            symbols: self.out.symbols.len(),
            captures: self.capture_count,
            work_units: self.work,
            retained_bytes: bytes,
            symbol_bytes: self.out.symbols.capacity() * size_of::<Symbol>(),
            capture_bytes: self
                .out
                .functions
                .iter()
                .map(|f| f.captures.capacity() * size_of::<Capture>())
                .sum(),
            imports: self.out.imports.len(),
            signature_builds: self.signatures.len() + usize::from(self.empty_signature.is_some()),
            signature_parameters: self
                .signatures
                .values()
                .map(|signature| signature.parameters.len())
                .sum(),
        };
    }
}
