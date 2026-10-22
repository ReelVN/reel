//! Path-local types for stable values checked by ordinary `is` expressions.
use super::*;

impl Binder<'_> {
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
                } if operator.kind == K::Is => {
                    let mut name = left;
                    while let Form::Single { child } = self.out.hir.node(name).form {
                        if self.kind(name) != HirKind::Group || !self.budget(name) {
                            break;
                        }
                        name = child;
                    }
                    if self.kind(name) != HirKind::Name {
                        continue;
                    }
                    let Some(symbol) = self.out.facts[name.0 as usize].symbol else {
                        continue;
                    };
                    let binding = &self.out.symbols[symbol.0 as usize];
                    if binding.mutable {
                        continue;
                    }
                    let before = self.refinements.get(&symbol).copied().unwrap_or(binding.ty);
                    let tested = self.ty(right);
                    let after = if truth {
                        if self.proven_subtype(before, tested) {
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
                _ => {}
            }
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
