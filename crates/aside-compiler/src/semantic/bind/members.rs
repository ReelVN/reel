//! Member lookup and call binding.
use super::*;

impl Binder<'_> {
    pub(super) fn member(&mut self, n: NodeId, children: &[NodeId], object: TypeId) -> TypeId {
        let key = self
            .name(n)
            .map(|t| self.out.hir.name(t.name.unwrap()).to_owned());
        if let (Some(&base), Some(key)) = (children.first(), key) {
            if let Some(group) = super::super::builtin_members(&self.out.hir, &self.out.types, object)
                // A table's own named entry wins over a builtin method. Its
                // dynamic shape cannot promise this fallback's call contract.
                && group.receiver != TypeHint::Table
            {
                let Some(member) = group
                    .members
                    .iter()
                    .find(|member| member.symbol.name == key)
                else {
                    self.unknown_member(n, &key);
                    return ANY;
                };
                let symbol = member.symbol.clone();
                let ty = self.hint(&symbol.ty);
                self.member_write_types.insert(n, None);
                self.external_nodes
                    .insert(n, ExternalDescriptor::metadata(symbol));
                if matches!(self.out.types[object.0 as usize], Type::Union(_)) {
                    self.check(n, RuntimeChecks::TYPE);
                }
                return ty;
            }
            if let Some(class) = self
                .external_nodes
                .get(&base)
                .and_then(|symbol| symbol.class.clone())
            {
                if let Some(member) = class
                    .static_members
                    .iter()
                    .find(|member| member.symbol.name == key)
                    .cloned()
                {
                    let ty = self.hint(&member.symbol.ty);
                    let write = member.write_type.as_ref().map(|ty| self.hint(ty));
                    self.member_write_types.insert(n, write);
                    self.external_nodes
                        .insert(n, ExternalDescriptor::metadata(member.symbol));
                    return ty;
                }
                if class.bases.is_empty() {
                    self.unknown_member(n, &key);
                }
                self.check(n, RuntimeChecks::TYPE);
                return ANY;
            }
            let named = self.nullable_named_type(object);
            if let Some(name) = named
                && let Some(member) = self
                    .resolver
                    .resolve_type_member(self.out.hir.name(name), &key)
            {
                let ty = self.hint(&member.symbol.ty);
                let write = member.write_type.as_ref().map(|ty| self.hint(ty));
                self.member_write_types.insert(n, write);
                self.external_nodes
                    .insert(n, ExternalDescriptor::metadata(member.symbol));
                if matches!(self.out.types[object.0 as usize], Type::Union(_)) {
                    self.check(n, RuntimeChecks::TYPE);
                    if matches!(
                        self.out.hir.node(n).form,
                        Form::Member { optional: true, .. } | Form::Member { chain: true, .. }
                    ) {
                        return self.nullable_type(ty);
                    }
                }
                return ty;
            }
            if let Some(name) = named
                && self.resolver.has_closed_members(self.out.hir.name(name))
            {
                self.unknown_member(n, &key);
                return ANY;
            }
            if let Some(namespace) = self
                .external_nodes
                .get(&base)
                .and_then(|e| e.namespace.clone())
            {
                let mut path = namespace
                    .module
                    .split("::")
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                path.push(key.clone());
                let request = ImportRequest {
                    from: self.options.module.clone(),
                    path,
                    kind: ImportKind::Symbol,
                };
                match self.resolve(&request) {
                    Ok(mut result) if result.version == namespace.version => {
                        if let Some(symbol) = result.bindings.pop() {
                            if symbol.type_only {
                                self.error(
                                    D::InvalidType,
                                    self.span(n),
                                    "a type declaration cannot be read as a value",
                                );
                                self.external_nodes
                                    .insert(n, ExternalDescriptor::reference(symbol));
                                return ANY;
                            }
                            let ty = self.external_type(&symbol);
                            self.external_nodes
                                .insert(n, ExternalDescriptor::reference(symbol));
                            return ty;
                        }
                    }
                    Ok(_) => self.error(
                        D::ModuleResolution,
                        self.span(n),
                        "namespace version changed during analysis",
                    ),
                    Err(error) => self.error(D::ModuleResolution, self.span(n), error.to_string()),
                }
            }
            if let Type::Class(class) | Type::Instance(class) =
                self.out.types[object.0 as usize].clone()
            {
                let instance = matches!(self.out.types[object.0 as usize], Type::Instance(_));
                let name = self.intern_name(&key);
                if let Some(d) = self.find_class_member(class, name, !instance) {
                    if self.kind(d) == HirKind::Function
                        && let Some(f) = self.fn_nodes.get(&d).copied()
                    {
                        self.check(n, RuntimeChecks::RECEIVER);
                        self.out.facts[n.0 as usize].function = Some(f);
                        return self.intern_type(if instance {
                            Type::BoundMethod(f)
                        } else {
                            Type::Function(f)
                        });
                    }
                    return self.ty(d);
                }
                if self.class_has_closed_members(class) {
                    self.unknown_member(n, &key);
                }
            }
        }
        self.check(n, RuntimeChecks::TYPE);
        ANY
    }
    pub(super) fn unknown_member(&mut self, node: NodeId, name: &str) {
        let range = self.name(node).map_or(self.span(node), |token| token.span);
        self.error(
            D::UnknownMember,
            range,
            format!("unknown member `{name}` on this type"),
        );
    }
    pub(super) fn class_has_closed_members(&self, mut class: NodeId) -> bool {
        for _ in 0..self.options.limits.max_depth.max(1) {
            if let Some(base) = self.class_bases.get(&class) {
                class = *base;
            } else {
                return matches!(
                    self.out.hir.node(class).form,
                    Form::Class { base: None, .. }
                );
            }
        }
        false
    }
    pub(super) fn find_class_member(
        &self,
        mut class: NodeId,
        name: NameId,
        static_member: bool,
    ) -> Option<NodeId> {
        for _ in 0..self.options.limits.max_depth.max(1) {
            if let Some(&node) = self
                .class_members
                .get(&class)
                .and_then(|m| m.get(&(name, static_member)))
            {
                return Some(node);
            }
            class = *self.class_bases.get(&class)?;
        }
        None
    }
    pub(super) fn nullable_named_type(&self, ty: TypeId) -> Option<NameId> {
        match &self.out.types[ty.0 as usize] {
            Type::Named(name) => Some(*name),
            Type::Union(types) => {
                let mut name = None;
                for ty in types {
                    match self.out.types[ty.0 as usize] {
                        Type::Null => {}
                        Type::Named(candidate) if name.is_none_or(|name| name == candidate) => {
                            name = Some(candidate)
                        }
                        _ => return None,
                    }
                }
                name
            }
            _ => None,
        }
    }
    pub(super) fn nullable_type(&mut self, ty: TypeId) -> TypeId {
        match &self.out.types[ty.0 as usize] {
            Type::Null => ty,
            Type::Union(types) if types.contains(&NULL) => ty,
            _ => self.intern_type(Type::Union(vec![ty, NULL])),
        }
    }
    pub(super) fn call(&mut self, n: NodeId, ty: TypeId, ctx: &Context) -> TypeId {
        let Form::Call {
            callee: target,
            arguments,
            children: trailing,
            optional,
            chain,
        } = self.out.hir.node(n).form
        else {
            return ANY;
        };
        let mut descriptor = self.external_nodes.get(&target).cloned();
        let mut named_callee = None;
        if descriptor
            .as_ref()
            .is_none_or(|descriptor| descriptor.signature.is_none())
            && let Some(name) = self.nullable_named_type(ty)
        {
            if !self.callable_types.contains_key(&name) {
                let callable = self
                    .resolver
                    .resolve_type_call(self.out.hir.name(name))
                    .map(ExternalDescriptor::metadata);
                self.callable_types.insert(name, callable);
            }
            if let Some(callable) = &self.callable_types[&name] {
                descriptor = Some(callable.clone());
                named_callee = Some(name);
            }
        }
        if let Some(Intrinsic::Derive) = descriptor.as_ref().and_then(|d| d.intrinsic)
            && let Some(args) = arguments
        {
            for arg in self.children(args) {
                let mut value = self.children(arg).last().copied().unwrap_or(arg);
                while let Form::Single { child } = self.out.hir.node(value).form {
                    value = child;
                }
                if self.kind(value) == HirKind::Closure {
                    self.derived_functions.insert(value);
                }
            }
        }
        let mut actual_ctx = ctx.clone();
        if optional || chain {
            actual_ctx.conditional = true;
        }
        for c in arguments.into_iter().chain(trailing) {
            self.visit(c, &actual_ctx);
        }
        if let Some(arguments) = arguments {
            for arg in self.children(arguments) {
                if self.kind(arg) == HirKind::Spread {
                    self.check_argument_spread(arg);
                }
            }
        }
        if let Some(intrinsic) = descriptor.as_ref().and_then(|d| d.intrinsic) {
            self.out.facts[n.0 as usize].intrinsic = Some(intrinsic);
            if intrinsic == Intrinsic::Bind {
                let value = arguments
                    .and_then(|args| self.children(args).first().copied())
                    .and_then(|a| self.children(a).first().copied());
                if let Some(value) = value {
                    if let Form::Member { .. } | Form::Index { .. } = self.out.hir.node(value).form
                    {
                        // An ordinary table alias can be adopted by a model later.
                        // Observable identity is therefore checked at execution time.
                        self.check(value, RuntimeChecks::PROPERTY);
                    } else {
                        self.error(
                            D::TypeMismatch,
                            self.span(value),
                            "bind requires an observable property location",
                        );
                    }
                } else {
                    self.error(
                        D::InvalidCall,
                        self.span(n),
                        "bind requires a property location",
                    );
                }
            }
        }
        if ctx.derived {
            self.check(n, RuntimeChecks::DERIVED_SAFE);
        }
        if ctx.init {
            self.check(n, RuntimeChecks::INIT_SAFE);
        }
        if !ctx.conditional
            && let Some(signature) = descriptor.as_ref().and_then(|d| d.signature.as_ref())
            && ((ctx.derived && signature.derived_safe == Some(false))
                || (ctx.init && signature.init_safe == Some(false)))
        {
            self.error(
                D::InvalidEffect,
                self.span(n),
                "host function is not safe in this execution context",
            );
        }
        if ty == NULL && (optional || chain) {
            return NULL;
        }
        let bound = matches!(
            self.out.types[ty.0 as usize],
            Type::BoundMethod(_) | Type::Class(_)
        );
        let constructor = if let Type::Class(class) = self.out.types[ty.0 as usize] {
            let init = self.intern_name("init");
            self.find_class_member(class, init, false)
                .and_then(|n| self.fn_nodes.get(&n).copied())
        } else {
            None
        };
        let function = match self.out.types[ty.0 as usize] {
            Type::Function(f) | Type::BoundMethod(f) => Some(f),
            _ => constructor,
        };
        let external = descriptor.clone();
        let effect = external
            .as_ref()
            .and_then(|e| e.signature.as_ref())
            .map_or(CallEffect::Unknown, |s| s.effect);
        let call = CallSite {
            node: n,
            caller: ctx.function,
            callee: function,
            bound,
            effect,
            no_wait: ctx.no_wait,
            named_callee,
        };
        self.calls.push(call.clone());
        self.check(n, RuntimeChecks::CALL);
        if ctx.no_wait {
            self.check(n, RuntimeChecks::NO_WAIT);
        }
        if matches!(
            descriptor.as_ref().and_then(|symbol| symbol.intrinsic),
            Some(Intrinsic::Bind | Intrinsic::Derive)
        ) {
            let name = self.intern_name("Binding");
            return self.intern_type(Type::Named(name));
        }
        if let Some(external) = external
            && let Some(signature) = &external.signature
        {
            if (!signature.result_cases.is_empty() || signature.result_parameter.is_some())
                && let Some(result) =
                    self.specialized_call_result(&call, target, arguments, trailing, signature)
            {
                return if (optional || chain) && !self.definitely_nonnull(ty) {
                    self.nullable_type(result)
                } else {
                    result
                };
            }
            let result = self.hint(&signature.result);
            return if (optional || chain) && !self.definitely_nonnull(ty) {
                self.nullable_type(result)
            } else {
                result
            };
        }
        if let Type::Class(class) = self.out.types[ty.0 as usize] {
            self.check(n, RuntimeChecks::NO_WAIT);
            return self.intern_type(Type::Instance(class));
        }
        if let Some(f) = function {
            return match self.out.functions[f.0 as usize].kind {
                FunctionKind::Async => FUTURE,
                FunctionKind::Generator => GENERATOR,
                FunctionKind::Plain => self.out.functions[f.0 as usize].return_type,
            };
        }
        if let Type::Class(class) = self.out.types[ty.0 as usize] {
            return self.intern_type(Type::Instance(class));
        }
        let callable = match &self.out.types[ty.0 as usize] {
            Type::Callable(_, result) => Some(*result),
            Type::Union(types) => {
                let mut candidates = types.iter().filter(|&&ty| ty != NULL);
                match candidates.next().map(|ty| &self.out.types[ty.0 as usize]) {
                    Some(Type::Callable(_, result)) if candidates.next().is_none() => Some(*result),
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some(result) = callable {
            return if (optional || chain) && !self.definitely_nonnull(ty) {
                self.nullable_type(result)
            } else {
                result
            };
        }
        if is_primitive(&self.out.types[ty.0 as usize]) {
            self.error(
                D::InvalidCall,
                self.span(n),
                "primitive value is not callable",
            );
        }
        ANY
    }
}
