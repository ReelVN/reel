//! Use known literal values to diagnose explicit value contracts at their source.
use super::*;

impl Binder<'_> {
    pub(super) fn literal_text(&mut self, node: NodeId) -> Option<String> {
        self.charge_work(node, self.out.hir.text(self.span(node)).len())
            .then(|| crate::strings::static_text(&self.out.hir, node))
            .flatten()
    }

    pub(super) fn has_value_contract(&self, ty: TypeId) -> bool {
        self.value_contracts.contains(&ty)
    }

    pub(super) fn require_contract(
        &mut self,
        node: NodeId,
        actual: TypeId,
        expected: TypeId,
        label: &str,
    ) {
        if !self.budget(node) {
            return;
        }
        let form = self.out.hir.node(node).form;
        match form {
            Form::Block { tail: Some(tail) }
            | Form::Statement {
                expression: tail,
                terminated: false,
            }
            | Form::Argument { value: tail, .. } => {
                self.require_contract(tail, self.ty(tail), expected, label);
            }
            Form::Single { child } if self.kind(node) == HirKind::Group => {
                self.require_contract(child, self.ty(child), expected, label);
            }
            Form::Condition {
                condition,
                body,
                otherwise,
                pattern,
            } if self.kind(node) == HirKind::If => {
                let constant = pattern
                    .is_none()
                    .then(|| self.constant_bool(condition))
                    .flatten();
                if constant != Some(false) && self.falls_through(body) {
                    self.require_contract(body, self.ty(body), expected, label);
                }
                if constant != Some(true) {
                    if let Some(otherwise) = otherwise {
                        if self.falls_through(otherwise) {
                            self.require_contract(otherwise, self.ty(otherwise), expected, label);
                        }
                    } else {
                        self.require_type(node, NULL, expected, label);
                    }
                }
            }
            Form::Match { .. } => {
                for arm in self.children(node).into_iter().skip(1) {
                    if let Form::MatchArm { body, .. } = self.out.hir.node(arm).form
                        && self.falls_through(body)
                    {
                        self.require_contract(body, self.ty(body), expected, label);
                    }
                }
            }
            _ => {
                let known = self.known_contract_value(node).unwrap_or(actual);
                self.require_type(node, known, expected, label);
            }
        }
    }

    fn known_contract_value(&mut self, node: NodeId) -> Option<TypeId> {
        if !self.budget(node) {
            return None;
        }
        match self.kind(node) {
            HirKind::String => {
                let text = self.literal_text(node)?;
                Some(self.intern_type(Type::StringLiteral(text)))
            }
            HirKind::Object => {
                let mut fields = Vec::new();
                let mut seen = HashSet::new();
                for field in self.children(node) {
                    let Form::Field {
                        key: crate::hir::FieldKey::Name(name),
                        value,
                    } = self.out.hir.node(field).form
                    else {
                        return None;
                    };
                    let name = name.name?;
                    if !seen.insert(name) {
                        return None;
                    }
                    fields.push((
                        name,
                        self.known_contract_value(value).unwrap_or(self.ty(value)),
                    ));
                }
                Some(self.record_type(fields))
            }
            HirKind::Tuple => {
                let items = self
                    .children(node)
                    .into_iter()
                    .map(|item| self.known_contract_value(item).unwrap_or(self.ty(item)))
                    .collect();
                Some(self.intern_type(Type::Tuple(items)))
            }
            HirKind::Group => match self.out.hir.node(node).form {
                Form::Single { child } => self.known_contract_value(child),
                _ => None,
            },
            _ => None,
        }
    }
}
