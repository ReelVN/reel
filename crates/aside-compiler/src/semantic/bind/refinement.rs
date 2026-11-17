//! Path-local types for stable values checked by type tests and null comparisons.
use super::*;

impl Binder<'_> {
    pub(super) fn refine_after_if(&mut self, statement: NodeId) {
        let node = match self.out.hir.node(statement).form {
            Form::Statement { expression, .. } => expression,
            _ => statement,
        };
        if self.kind(node) == HirKind::If
            && let Form::Condition {
                pattern: None,
                condition,
                body,
                otherwise,
            } = self.out.hir.node(node).form
        {
            let yes = self.falls_through(body);
            let no = otherwise.is_none_or(|body| self.falls_through(body));
            if yes != no {
                self.refine_condition(condition, yes);
            }
        }
    }

    pub(super) fn restore_refinements(&mut self, mark: usize) {
        while self.refinement_undo.len() > mark {
            let (symbol, previous) = self.refinement_undo.pop().unwrap();
            if let Some(ty) = previous {
                self.refinements.insert(symbol, ty);
            } else {
                self.refinements.remove(&symbol);
            }
        }
    }

    pub(super) fn refine_condition(&mut self, condition: NodeId, truth: bool) {
        let mut pending: SmallVec<[(NodeId, bool); 4]> = smallvec![(condition, truth)];
        while let Some((node, truth)) = pending.pop() {
            if !self.budget(node) {
                return;
            }
            match self.out.hir.node(node).form {
                Form::Single { child } if self.kind(node) == HirKind::Group => {
                    pending.push((child, truth));
                }
                Form::Unary { operator, value } if operator.kind == K::Bang => {
                    pending.push((value, !truth));
                }
                Form::Binary {
                    left,
                    operator,
                    right,
                } if (truth && operator.kind == K::AndAnd)
                    || (!truth && operator.kind == K::OrOr) =>
                {
                    pending.push((right, truth));
                    pending.push((left, truth));
                }
                Form::Binary {
                    left,
                    operator,
                    right,
                } if operator.kind == K::Is => self.refine_name(left, self.ty(right), truth),
                Form::Binary {
                    left,
                    operator,
                    right,
                } if matches!(operator.kind, K::EqEq | K::NotEq) => {
                    let left = self.ungroup_guard(left);
                    let right = self.ungroup_guard(right);
                    let equal = truth == (operator.kind == K::EqEq);
                    if self.is_null_literal(right) {
                        self.refine_name(left, NULL, equal);
                    } else if self.is_null_literal(left) {
                        self.refine_name(right, NULL, equal);
                    }
                }
                _ => {}
            }
        }
    }

    fn ungroup_guard(&mut self, mut node: NodeId) -> NodeId {
        while self.kind(node) == HirKind::Group
            && let Form::Single { child } = self.out.hir.node(node).form
            && self.budget(node)
        {
            node = child;
        }
        node
    }

    fn is_null_literal(&self, node: NodeId) -> bool {
        matches!(self.out.hir.node(node).form, Form::Literal { token } if token.kind == K::Null)
    }

    fn refine_name(&mut self, node: NodeId, tested: TypeId, truth: bool) {
        let node = self.ungroup_guard(node);
        if self.kind(node) != HirKind::Name {
            return;
        }
        let Some(symbol) = self.out.facts[node.0 as usize].symbol else {
            return;
        };
        let binding = &self.out.symbols[symbol.0 as usize];
        if binding.mutable {
            return;
        }
        let before = self.refinements.get(&symbol).copied().unwrap_or(binding.ty);
        let after = if truth {
            if self.proven_subtype(before, tested)
                || (tested == NULL && self.definitely_nonnull(before))
            {
                before
            } else {
                tested
            }
        } else if let Type::Union(members) = &self.out.types[before.0 as usize] {
            let retained: Vec<_> = members
                .iter()
                .copied()
                .filter(|&member| !self.proven_subtype(member, tested))
                .collect();
            match retained.len() {
                0 => before,
                1 => retained[0],
                count if count == members.len() => before,
                _ => self.intern_type(Type::Union(retained)),
            }
        } else {
            before
        };
        if after != before {
            let previous = self.refinements.insert(symbol, after);
            self.refinement_undo.push((symbol, previous));
        }
    }

    // Compatibility accepts uncertain dynamic values; path exclusion requires proof.
    fn proven_subtype(&self, actual: TypeId, expected: TypeId) -> bool {
        if actual == expected || expected == ANY {
            return true;
        }
        match (
            &self.out.types[actual.0 as usize],
            &self.out.types[expected.0 as usize],
        ) {
            (Type::Named(actual), Type::Named(expected)) => {
                self.resolver
                    .is_named_subtype(self.out.hir.name(*actual), self.out.hir.name(*expected))
                    == Some(true)
            }
            (Type::Instance(actual), Type::Instance(expected)) => {
                let mut class = *actual;
                for _ in 0..self.class_bases.len() {
                    let Some(&parent) = self.class_bases.get(&class) else {
                        break;
                    };
                    if parent == *expected {
                        return true;
                    }
                    class = parent;
                }
                false
            }
            (Type::Tuple(_), Type::Named(name)) => self.out.hir.name(*name) == "tuple",
            (Type::Class(_), Type::Named(name)) => self.out.hir.name(*name) == "class",
            (Type::Instance(_), Type::Named(name)) => self.out.hir.name(*name) == "instance",
            (Type::Function(_) | Type::BoundMethod(_) | Type::Callable(..), Type::Closure) => true,
            _ => false,
        }
    }
}
