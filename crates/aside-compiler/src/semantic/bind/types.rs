//! Type annotations and compatibility checks.
use super::*;

impl Binder<'_> {
    pub(super) fn coalesced_type(&mut self, node: NodeId, left: TypeId, right: TypeId) -> TypeId {
        if left == NULL {
            return right;
        }
        if left == right || self.definitely_nonnull(left) {
            return left;
        }
        let mut pending: SmallVec<[(TypeId, bool); 4]> = smallvec![(left, false), (right, true)];
        let mut alternatives: SmallVec<[TypeId; 4]> = SmallVec::new();
        while let Some((ty, allow_null)) = pending.pop() {
            if ty == ANY || !self.budget(node) {
                return ANY;
            }
            match &self.out.types[ty.0 as usize] {
                Type::Union(members) => pending.extend(members.iter().map(|&ty| (ty, allow_null))),
                Type::Null if !allow_null => {}
                _ => alternatives.push(ty),
            }
        }
        alternatives.sort_unstable();
        alternatives.dedup();
        match alternatives.as_slice() {
            [] => NULL,
            [only] => *only,
            _ => self.intern_type(Type::Union(alternatives.into_vec())),
        }
    }

    pub(super) fn annotation(&mut self, n: NodeId, ctx: &Context) -> TypeId {
        if !self.budget(n) {
            return ANY;
        }
        let children = self.children(n);
        let result = match self.kind(n) {
            HirKind::StringType => {
                if let Some(&text) = children.first()
                    && let Some(text) = self.literal_text(text)
                {
                    self.intern_type(Type::StringLiteral(text))
                } else {
                    self.error(
                        D::InvalidType,
                        self.span(n),
                        "string type must be a literal without interpolation",
                    );
                    ANY
                }
            }
            HirKind::RecordType => {
                let mut fields = Vec::new();
                let mut seen = HashSet::new();
                for field in children {
                    if let (Some(name), Form::Single { child }) =
                        (self.name(field), self.out.hir.node(field).form)
                    {
                        let key = name.name.unwrap();
                        if !seen.insert(key) {
                            self.error(D::InvalidType, name.span, "duplicate record field");
                            continue;
                        }
                        fields.push((key, self.annotation(child, ctx)));
                    }
                }
                self.record_type(fields)
            }
            HirKind::NamedType => {
                if self
                    .out
                    .hir
                    .tokens(n)
                    .filter(|t| t.kind == K::Ident)
                    .count()
                    > 1
                {
                    return self.qualified_type(n, ctx);
                }
                let text = self
                    .tokens(n)
                    .iter()
                    .map(|t| self.out.hir.text(t.span))
                    .collect::<String>();
                if let Some(builtin) = super::super::types::builtin_type(&text) {
                    self.hint(&builtin)
                } else {
                    if let Some(t) = self.name(n) {
                        let name = t.name.unwrap();
                        if let Some(s) = self.lookup(name) {
                            self.out.facts[n.0 as usize].symbol = Some(s);
                            self.capture(s, t.span, ctx);
                            match self.out.types[self.out.symbols[s.0 as usize].ty.0 as usize] {
                                Type::Class(class) => self.intern_type(Type::Instance(class)),
                                Type::Named(name) => self.intern_type(Type::Named(name)),
                                Type::Any
                                    if matches!(
                                        self.out.symbols[s.0 as usize].kind,
                                        SymbolKind::Host | SymbolKind::Import
                                    ) =>
                                {
                                    self.intern_type(Type::Named(name))
                                }
                                _ => {
                                    self.error(
                                        D::InvalidType,
                                        t.span,
                                        "annotation name is not a class/native type",
                                    );
                                    ANY
                                }
                            }
                        } else {
                            self.error(D::InvalidType, t.span, format!("unknown type {text}"));
                            ANY
                        }
                    } else {
                        ANY
                    }
                }
            }
            HirKind::GroupType => children.first().map_or(ANY, |&c| self.annotation(c, ctx)),
            HirKind::TupleType => {
                let ts = children
                    .into_iter()
                    .map(|c| self.annotation(c, ctx))
                    .collect();
                self.intern_type(Type::Tuple(ts))
            }
            HirKind::UnionType => {
                let mut ts = Vec::new();
                for c in children {
                    let t = self.annotation(c, ctx);
                    match &self.out.types[t.0 as usize] {
                        Type::Union(v) => ts.extend(v),
                        _ => ts.push(t),
                    }
                }
                self.union_type(ts)
            }
            HirKind::FunctionType => {
                let mut ts: Vec<_> = children
                    .into_iter()
                    .map(|c| self.annotation(c, ctx))
                    .collect();
                let r = ts.pop().unwrap_or(ANY);
                self.intern_type(Type::Callable(ts, r))
            }
            _ => ANY,
        };
        self.set_ty(n, result)
    }

    pub(super) fn record_type(&mut self, mut fields: Vec<(NameId, TypeId)>) -> TypeId {
        fields.sort_unstable_by_key(|&(name, _)| name);
        self.intern_type(Type::Record(fields))
    }

    pub(super) fn union_type(&mut self, mut types: Vec<TypeId>) -> TypeId {
        types.sort_unstable();
        types.dedup();
        match types.as_slice() {
            [] => ANY,
            [only] => *only,
            _ => self.intern_type(Type::Union(types)),
        }
    }
    pub(super) fn string_like(&self, ty: TypeId) -> bool {
        match &self.out.types[ty.0 as usize] {
            Type::String | Type::StringLiteral(_) => true,
            Type::Union(types) => types.iter().all(|&ty| self.string_like(ty)),
            _ => false,
        }
    }
    pub(super) fn qualified_type(&mut self, n: NodeId, ctx: &Context) -> TypeId {
        let names = self
            .tokens(n)
            .into_iter()
            .filter_map(|t| t.name)
            .collect::<Vec<_>>();
        let Some(s) = names.first().and_then(|&name| self.lookup(name)) else {
            self.error(D::InvalidType, self.span(n), "unknown type namespace");
            return ANY;
        };
        self.out.facts[n.0 as usize].symbol = Some(s);
        self.capture(s, self.span(n), ctx);
        let mut descriptor = self.external.get(&s).cloned();
        for &name in names.iter().skip(1) {
            let Some(namespace) = descriptor.as_ref().and_then(|d| d.namespace.clone()) else {
                self.error(
                    D::InvalidType,
                    self.span(n),
                    "type path requires a namespace",
                );
                return ANY;
            };
            let mut path = namespace
                .module
                .split("::")
                .map(str::to_owned)
                .collect::<Vec<_>>();
            path.push(self.out.hir.name(name).to_owned());
            match self.resolve(&ImportRequest {
                from: self.options.module.clone(),
                path,
                kind: ImportKind::Symbol,
            }) {
                Ok(mut result) if result.version == namespace.version => {
                    descriptor = result.bindings.pop().map(ExternalDescriptor::reference)
                }
                Ok(_) => {
                    self.error(
                        D::ModuleResolution,
                        self.span(n),
                        "type namespace version changed",
                    );
                    return ANY;
                }
                Err(error) => {
                    self.error(D::ModuleResolution, self.span(n), error.to_string());
                    return ANY;
                }
            }
        }
        if let Some(descriptor) = descriptor {
            let ty = match &descriptor.ty {
                TypeHint::Named(_) => self.external_type(&descriptor),
                TypeHint::Any => ANY,
                _ => {
                    self.error(
                        D::InvalidType,
                        self.span(n),
                        "type path does not name a class/native type",
                    );
                    ANY
                }
            };
            self.external_nodes.insert(n, descriptor);
            self.set_ty(n, ty)
        } else {
            ANY
        }
    }
    // Any/union with any is uncertain, not incompatible. User-defined objects
    // may supply protocols; static rejection is limited to definite primitives.
    pub(super) fn compatible(&self, actual: TypeId, expected: TypeId) -> bool {
        if actual == ANY || expected == ANY || actual == expected {
            return true;
        }
        match (
            &self.out.types[actual.0 as usize],
            &self.out.types[expected.0 as usize],
        ) {
            (_, Type::Union(ts)) => ts.iter().any(|&t| self.compatible(actual, t)),
            (Type::Union(ts), _) => ts.iter().any(|&t| self.compatible(t, expected)),
            (Type::StringLiteral(_), Type::String) | (Type::String, Type::StringLiteral(_)) => true,
            (Type::Record(_), Type::Table) | (Type::Table, Type::Record(_)) => true,
            (Type::Record(a), Type::Record(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .zip(b)
                        .all(|(&(ak, a), &(bk, b))| ak == bk && self.compatible(a, b))
            }
            (Type::Tuple(a), Type::Tuple(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(&a, &b)| self.compatible(a, b))
            }
            (Type::Tuple(_), Type::Named(name)) | (Type::Named(name), Type::Tuple(_))
                if self.out.hir.name(*name) == "tuple" =>
            {
                true
            }
            (Type::Function(f) | Type::BoundMethod(f), Type::Callable(p, r)) => {
                let bound = matches!(self.out.types[actual.0 as usize], Type::BoundMethod(_));
                let f = &self.out.functions[f.0 as usize];
                let parameters = &f.parameters[usize::from(bound)..];
                let fixed = parameters
                    .iter()
                    .take_while(|parameter| !parameter.rest)
                    .count();
                let result = match f.kind {
                    FunctionKind::Plain => f.return_type,
                    FunctionKind::Async => FUTURE,
                    FunctionKind::Generator => GENERATOR,
                };
                (p.len() <= fixed || parameters.last().is_some_and(|parameter| parameter.rest))
                    && parameters
                        .iter()
                        .skip(p.len())
                        .all(|parameter| parameter.default.is_some() || parameter.rest)
                    && self.compatible(result, *r)
                    && parameters
                        .iter()
                        .take(fixed)
                        .zip(p)
                        .all(|(parameter, &ty)| {
                            self.compatible(ty, self.out.symbols[parameter.symbol.0 as usize].ty)
                        })
            }
            (Type::Callable(a, ar), Type::Callable(b, br)) => {
                a.len() == b.len()
                    && self.compatible(*ar, *br)
                    && a.iter()
                        .zip(b)
                        .all(|(&actual, &expected)| self.compatible(expected, actual))
            }
            (Type::Closure, Type::Callable(..)) => true,
            (Type::Function(_) | Type::BoundMethod(_) | Type::Callable(_, _), Type::Closure) => {
                true
            }
            (Type::Class(_), Type::Named(name)) if self.out.hir.name(*name) == "class" => true,
            (Type::Named(actual), Type::Named(expected))
                if self.out.hir.name(*actual) == "tuple"
                    || self.out.hir.name(*expected) == "tuple" =>
            {
                actual == expected
            }
            (Type::Named(actual), Type::Named(expected)) => self
                .resolver
                .is_named_subtype(self.out.hir.name(*actual), self.out.hir.name(*expected))
                .unwrap_or(true),
            (Type::Named(_) | Type::Instance(_), Type::Named(_) | Type::Instance(_)) => true,
            _ => false,
        }
    }
    pub(super) fn require(&mut self, n: NodeId, actual: TypeId, expected: TypeId, label: &str) {
        if self.has_value_contract(expected) {
            self.require_contract(n, actual, expected, label);
        } else {
            self.require_type(n, actual, expected, label);
        }
    }

    pub(super) fn require_type(
        &mut self,
        n: NodeId,
        actual: TypeId,
        expected: TypeId,
        label: &str,
    ) {
        if expected == ANY {
            return;
        }
        if self.incomplete_signature(actual) || self.incomplete_signature(expected) {
            self.pending_requirements.push(PendingRequirement {
                node: n,
                actual,
                expected,
                label: label.to_owned(),
            });
            self.check(n, RuntimeChecks::TYPE);
            return;
        }
        if !self.compatible(actual, expected) {
            self.error(
                D::TypeMismatch,
                self.span(n),
                format!(
                    "{label}: {} cannot satisfy {}",
                    self.out.display_type(actual),
                    self.out.display_type(expected)
                ),
            );
        } else if actual == ANY || actual != expected {
            self.check(n, RuntimeChecks::TYPE);
        }
    }
    pub(super) fn incomplete_signature(&self, ty: TypeId) -> bool {
        match &self.out.types[ty.0 as usize] {
            Type::Function(function) | Type::BoundMethod(function) => {
                !self.signature_ready.contains(function)
            }
            Type::Tuple(types) | Type::Union(types) => {
                types.iter().any(|&ty| self.incomplete_signature(ty))
            }
            Type::Record(fields) => fields.iter().any(|&(_, ty)| self.incomplete_signature(ty)),
            Type::Callable(types, result) => {
                types.iter().any(|&ty| self.incomplete_signature(ty))
                    || self.incomplete_signature(*result)
            }
            _ => false,
        }
    }
}
