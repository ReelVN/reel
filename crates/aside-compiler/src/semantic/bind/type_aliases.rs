//! Bind alias declarations and reuse ordinary annotation types at each use.
use super::*;

#[derive(Clone, Copy)]
pub(super) enum AliasState {
    Pending(NodeId),
    Resolving,
    Resolved(TypeId),
}

impl Binder<'_> {
    pub(super) fn declare_type_alias(&mut self, declaration: NodeId, node: NodeId, ctx: &Context) {
        if self.scope != self.root_scope || self.options.context != AnalysisContext::Module {
            self.error(
                D::InvalidType,
                self.span(node),
                "declare a type alias at module scope",
            );
            return;
        }
        let Form::TypeAlias { name, target } = self.out.hir.node(node).form else {
            return;
        };
        let Some(id) = name.name else { return };
        if super::super::types::builtin_type(self.out.hir.name(id)).is_some() {
            self.error(
                D::DuplicateBinding,
                name.span,
                "a type alias cannot replace a built-in type",
            );
            return;
        }
        let Some(descriptor) = super::super::type_aliases::descriptor(
            &self.out.hir,
            node,
            &self.options.module,
            &self.options.version,
        ) else {
            return;
        };
        let Some(symbol) = self.declare(Declaration {
            name: id,
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
        self.type_aliases
            .insert(symbol, AliasState::Pending(target));
        if self.modifier(declaration, K::Pub) {
            self.out.exports.push(symbol);
        }
    }

    pub(super) fn prepare_type_aliases(&mut self, children: &[NodeId]) {
        for &child in children {
            let node = self.unwrap(child);
            if self.kind(node) == HirKind::TypeAlias
                && let Some(symbol) = self.out.facts[node.0 as usize].symbol
            {
                let ty = self.resolve_type_alias(symbol, self.span(node));
                self.set_ty(node, ty);
            }
        }
    }

    pub(super) fn resolve_type_alias(&mut self, symbol: SymbolId, span: TextRange) -> TypeId {
        let target = match self.type_aliases[&symbol] {
            AliasState::Resolved(ty) => return ty,
            AliasState::Resolving => {
                self.alias_cycle(span);
                return ANY;
            }
            AliasState::Pending(target) => target,
        };
        if !self.enter_alias(span) {
            return ANY;
        }
        self.type_aliases.insert(symbol, AliasState::Resolving);
        let scope = std::mem::replace(&mut self.scope, self.root_scope);
        let ty = self.annotation(target, &Context::default());
        self.scope = scope;
        self.alias_depth -= 1;
        self.type_aliases.insert(symbol, AliasState::Resolved(ty));
        self.out.symbols[symbol.0 as usize].ty = ty;
        ty
    }

    pub(super) fn named_alias_hint(&mut self, name: &str) -> Option<TypeId> {
        let (module, local_name) = name.rsplit_once("::")?;
        if self.aliases_ready
            && module == self.options.module
            && let Some(id) = self.find_type_name(local_name)
            && let Some(&symbol) = self.maps[self.root_scope.0 as usize].get(&id)
            && self.type_aliases.contains_key(&symbol)
        {
            return Some(
                self.resolve_type_alias(symbol, self.out.symbols[symbol.0 as usize].declaration),
            );
        }
        let span = self.span(NodeId(0));
        if let Some(ty) = self.external_aliases.get(name).copied() {
            if ty.is_none() {
                self.alias_cycle(span);
            }
            return Some(ty.unwrap_or(ANY));
        }
        let target = self.resolver.type_alias(name)?;
        if !self.enter_alias(span) {
            return Some(ANY);
        }
        self.external_aliases.insert(name.into(), None);
        let ty = self.hint(&target);
        self.alias_depth -= 1;
        self.external_aliases.insert(name.into(), Some(ty));
        Some(ty)
    }

    fn enter_alias(&mut self, span: TextRange) -> bool {
        if self.alias_depth >= self.options.limits.max_depth {
            self.error(
                D::BudgetExceeded,
                span,
                "type alias expansion depth exceeded",
            );
            return false;
        }
        if !self.charge_at(span, 1) {
            return false;
        }
        self.alias_depth += 1;
        true
    }

    fn alias_cycle(&mut self, span: TextRange) {
        self.error(D::InvalidType, span, "cyclic type alias");
    }
}
