//! Bind source enum namespaces without introducing runtime namespace objects.
use super::*;

impl Binder<'_> {
    pub(super) fn prepare_enums(&mut self, children: &[NodeId], ctx: &Context) {
        let mut enums = SmallVec::<[NodeId; 2]>::new();
        for &child in children {
            let node = self.unwrap(child);
            if self.kind(node) == HirKind::Enum {
                self.qualify_enum(node);
                enums.push(node);
            }
        }
        for node in enums {
            self.finish_enum(node, ctx);
        }
    }

    pub(super) fn declare_enum(&mut self, declaration: NodeId, node: NodeId, ctx: &Context) {
        if self.scope != self.root_scope || self.options.context != AnalysisContext::Module {
            self.error(
                D::InvalidType,
                self.span(node),
                "declare an enum at module scope",
            );
            return;
        }
        let Some(name) = self.name(node) else { return };
        let Some(descriptor) = super::super::enumerations::descriptor(
            &self.out.hir,
            node,
            &self.options.module,
            &self.options.version,
        ) else {
            return;
        };
        let Some(symbol) = self.declare(Declaration {
            name: name.name.expect("enum name"),
            span: name.span,
            kind: SymbolKind::Class,
            mutable: false,
            ty: ANY,
            owner: ctx.function,
            iteration: false,
        }) else {
            return;
        };
        self.initialized[symbol.0 as usize] = true;
        self.out.facts[node.0 as usize].symbol = Some(symbol);
        self.external
            .insert(symbol, ExternalDescriptor::reference(descriptor));
        if self.modifier(declaration, K::Pub) {
            self.out.exports.push(symbol);
        }
    }

    fn qualify_enum(&mut self, node: NodeId) {
        let Some(symbol) = self.out.facts[node.0 as usize].symbol else {
            return;
        };
        let mut descriptor = (*self.external[&symbol].symbol).clone();
        super::super::link::canonical_symbol(&mut descriptor, &|name| self.source_type_name(name));
        self.external
            .insert(symbol, ExternalDescriptor::reference(descriptor));
    }

    fn finish_enum(&mut self, node: NodeId, ctx: &Context) {
        let Some(symbol) = self.out.facts[node.0 as usize].symbol else {
            return;
        };
        let variants = self.children(node);
        if variants.is_empty() {
            self.error(
                D::InvalidType,
                self.span(node),
                "enum requires at least one case",
            );
            return;
        }
        let mut seen = HashSet::new();
        for variant in variants {
            if let Some(name) = self.name(variant)
                && !seen.insert(name.name)
            {
                self.error(D::DuplicateBinding, name.span, "duplicate enum case");
            }
            if let Some(payload) = super::super::enumerations::payload_type(&self.out.hir, variant)
            {
                self.annotation(payload, ctx);
            }
        }
        let descriptor = self.external[&symbol].clone();
        let ty = self.hint(&descriptor.ty);
        self.out.symbols[symbol.0 as usize].ty = ty;
        self.set_ty(node, ty);
    }

    pub(super) fn enum_namespace_access(&self, node: NodeId) -> bool {
        self.out.hir.node(node).parent.is_some_and(|parent| {
            matches!(
                self.out.hir.node(parent).form,
                Form::Member { object, optional: false, chain: false, .. } if object == node
            )
        })
    }

    pub(super) fn direct_enum_call(&self, node: NodeId) -> bool {
        self.out.hir.node(node).parent.is_some_and(|parent| {
            matches!(
                self.out.hir.node(parent).form,
                Form::Call { callee, optional: false, chain: false, .. } if callee == node
            )
        })
    }

    fn source_type_name(&self, text: &str) -> String {
        let (head, suffix) = text
            .split_once("::")
            .map_or((text, None), |(head, suffix)| (head, Some(suffix)));
        let Some(name) = self.find_type_name(head) else {
            return text.into();
        };
        let Some(symbol) = self.lookup(name) else {
            return text.into();
        };
        if let Some(descriptor) = self.external.get(&symbol) {
            if let Some(namespace) = &descriptor.namespace
                && let Some(suffix) = suffix
            {
                return format!("{}::{suffix}", namespace.module);
            }
            if descriptor.intrinsic == Some(Intrinsic::TypeAlias)
                && let Some(reference) = &descriptor.reference
                && let Some(export) = &reference.export
            {
                return format!("{}::{export}", reference.module);
            }
            if let TypeHint::Named(canonical)
            | TypeHint::Enum {
                name: canonical, ..
            } = &descriptor.ty
            {
                return canonical.clone();
            }
        }
        if matches!(
            self.out.types[self.out.symbols[symbol.0 as usize].ty.0 as usize],
            Type::Class(_)
        ) {
            return format!("{}::{head}", self.options.module);
        }
        text.into()
    }

    pub(super) fn find_type_name(&self, source: &str) -> Option<NameId> {
        self.names
            .find(intern_hash(source), |id| self.out.hir.name(*id) == source)
            .copied()
    }

    pub(super) fn enum_named_hint(&self, canonical: &str) -> Option<TypeHint> {
        let (module, name) = canonical.rsplit_once("::")?;
        if module == self.options.module
            && let Some(name) = self.find_type_name(name)
            && let Some(symbol) = self.maps[self.root_scope.0 as usize].get(&name)
            && let Some(descriptor) = self.external.get(symbol)
            && matches!(descriptor.ty, TypeHint::Enum { .. })
        {
            return Some(descriptor.ty.clone());
        }
        self.resolver.enum_type(canonical)
    }

    pub(super) fn enum_type_hint(&mut self, canonical: &str, alternatives: &[TypeHint]) -> TypeId {
        let name = self.intern_name(canonical);
        if let Some(ty) = self.enum_types.get(&name) {
            return *ty;
        }
        let location = self.span(NodeId(0));
        if self.resolving_enums.len() >= self.options.limits.max_depth {
            self.error(
                D::BudgetExceeded,
                location,
                "enum payload type nesting exceeds the analysis depth",
            );
            return ANY;
        }
        if !self.charge_at(location, alternatives.len()) {
            return ANY;
        }
        if !self.resolving_enums.insert(name) {
            self.error(
                D::InvalidType,
                location,
                format!("recursive enum payload for `{canonical}`"),
            );
            return ANY;
        }
        let cases = alternatives.iter().map(|ty| self.hint(ty)).collect();
        self.resolving_enums.remove(&name);
        let ty = self.intern_type(Type::Enum(name, cases));
        self.enum_types.insert(name, ty);
        ty
    }

    pub(super) fn enum_case_type(
        &mut self,
        node: NodeId,
        annotation: NodeId,
        variant: HirToken,
        payload: Option<NodeId>,
        ctx: &Context,
    ) {
        let ty = self.annotation(annotation, ctx);
        let Type::Enum(name, cases) = &self.out.types[ty.0 as usize] else {
            self.error(
                D::InvalidType,
                self.span(annotation),
                "qualified pattern requires an enum type",
            );
            return;
        };
        let case = self.out.hir.text(variant.span);
        let tag = format!("{}::{case}", self.out.hir.name(*name));
        let ty = cases.iter().find(|&&case| match &self.out.types[case.0 as usize] {
            Type::StringLiteral(text) => text == &tag,
            Type::Tuple(parts) => matches!(&self.out.types[parts[0].0 as usize], Type::StringLiteral(text) if text == &tag),
            _ => false,
        }).copied();
        let Some(ty) = ty else {
            self.error(
                D::UnknownMember,
                variant.span,
                format!("unknown enum case `{case}`"),
            );
            return;
        };
        if let Some(member) = self
            .external_nodes
            .get(&annotation)
            .and_then(|descriptor| descriptor.class.as_ref())
            .and_then(|class| {
                class
                    .static_members
                    .iter()
                    .find(|member| member.symbol.name == case)
            })
        {
            self.external_nodes
                .insert(node, ExternalDescriptor::metadata(member.symbol.clone()));
        }
        if let Some(payload) = payload {
            if let Type::Tuple(parts) = &self.out.types[ty.0 as usize] {
                let Type::Record(fields) = &self.out.types[parts[1].0 as usize] else {
                    unreachable!("enum payload record");
                };
                let open = self
                    .out
                    .hir
                    .children(payload)
                    .any(|field| self.kind(field) == HirKind::RestPattern);
                let incomplete = !open && self.out.hir.children(payload).count() != fields.len();
                let unknown: SmallVec<[_; 4]> = self
                    .out
                    .hir
                    .children(payload)
                    .filter_map(|field| {
                        let Form::PatternField { key, .. } = self.out.hir.node(field).form else {
                            return None;
                        };
                        (!fields.iter().any(|(name, _)| Some(*name) == key.name)).then_some(key)
                    })
                    .collect();
                if incomplete {
                    self.error(
                        D::PatternBindings,
                        self.span(payload),
                        "list every enum payload field or add `..` to ignore the remaining fields",
                    );
                }
                for key in unknown {
                    self.error(
                        D::UnknownMember,
                        key.span,
                        format!(
                            "unknown enum payload field `{}`",
                            self.out.hir.text(key.span)
                        ),
                    );
                }
            } else {
                self.error(
                    D::PatternBindings,
                    self.span(payload),
                    "this enum case carries no fields",
                );
            }
        }
        self.set_ty(node, ty);
    }

    pub(super) fn check_enum_match(&mut self, node: NodeId, value: TypeId, children: &[NodeId]) {
        let Type::Enum(_, cases) = &self.out.types[value.0 as usize] else {
            return;
        };
        let mut covered = HashSet::new();
        for &arm in children.iter().skip(1) {
            if let Form::MatchArm {
                pattern,
                guard: None,
                ..
            } = self.out.hir.node(arm).form
                && (self.covered_enum_pattern(pattern, value, &mut covered)
                    || (covered.len() >= cases.len()
                        && cases.iter().all(|case| covered.contains(case))))
            {
                self.check(arm, RuntimeChecks::LAST_ENUM_ARM);
                return;
            }
        }
        let missing: Vec<_> = cases
            .iter()
            .filter(|case| !covered.contains(case))
            .filter_map(|case| self.enum_tag(*case))
            .filter_map(|tag| tag.rsplit_once("::").map(|(_, name)| name))
            .collect();
        if !missing.is_empty() {
            let span = self
                .token(node, K::Match)
                .map_or(self.span(node), |token| token.span);
            let message = format!("match is missing enum cases: {}", missing.join(", "));
            self.error(D::NonExhaustiveMatch, span, message);
        }
    }

    fn enum_tag(&self, ty: TypeId) -> Option<&str> {
        match &self.out.types[ty.0 as usize] {
            Type::StringLiteral(text) => Some(text),
            Type::Tuple(parts) => self.enum_tag(parts[0]),
            _ => None,
        }
    }

    fn covered_enum_pattern(
        &self,
        pattern: NodeId,
        value: TypeId,
        covered: &mut HashSet<TypeId>,
    ) -> bool {
        match self.out.hir.node(pattern).form {
            Form::Name { .. } | Form::Wildcard => true,
            Form::Single { child } => self.covered_enum_pattern(child, value, covered),
            Form::TypePattern {
                pattern,
                annotation,
            } if self.ty(annotation) == value => self.covered_enum_pattern(pattern, value, covered),
            Form::Or { left, right } => {
                self.covered_enum_pattern(left, value, covered)
                    || self.covered_enum_pattern(right, value, covered)
            }
            Form::EnumCase { payload, .. } => {
                if payload.is_none_or(|payload| {
                    self.out.hir.children(payload).all(|field| {
                        match self.out.hir.node(field).form {
                            Form::Wildcard => true,
                            _ if self.kind(field) == HirKind::RestPattern => true,
                            Form::PatternField { pattern, .. } => pattern.is_none_or(|part| {
                                matches!(
                                    self.out.hir.node(part).form,
                                    Form::Name { .. } | Form::Wildcard
                                )
                            }),
                            _ => false,
                        }
                    })
                }) {
                    covered.insert(self.ty(pattern));
                }
                false
            }
            _ => false,
        }
    }
}
