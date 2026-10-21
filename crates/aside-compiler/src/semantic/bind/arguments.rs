//! Calls expand sequences positionally and own table entries by name.
use super::*;

impl Binder<'_> {
    fn source_constant(&self, node: NodeId) -> Option<aside_vm::bytecode::Const> {
        use aside_vm::bytecode::Const;
        if let Some(value) = self.constant_bool(node) {
            return Some(Const::Bool(value));
        }
        match self.out.hir.node(node).form {
            Form::Single { child } => self.source_constant(child),
            Form::Literal { token } if token.kind == K::Null => Some(Const::Null),
            Form::Literal { token } if token.kind == K::Number => {
                let text = self.out.hir.text(token.span).replace('_', "");
                let (radix, digits) = match text.get(..2) {
                    Some("0x" | "0X") => (16, &text[2..]),
                    Some("0o" | "0O") => (8, &text[2..]),
                    Some("0b" | "0B") => (2, &text[2..]),
                    _ => (10, text.as_str()),
                };
                i64::from_str_radix(digits, radix).ok().map(Const::Int)
            }
            _ => None,
        }
    }

    /// Resolve registered result metadata with the ordinary cached argument binder.
    /// Calls without metadata do not enter this path or allocate a binding map.
    pub(super) fn specialized_call_result(
        &mut self,
        call: &CallSite,
        callee: NodeId,
        arguments: Option<NodeId>,
        trailing: Option<NodeId>,
        external: &ExternalSignature,
    ) -> Option<TypeId> {
        let signature = self.checked_signature(call, callee)?;
        let mut values = HashMap::new();
        let mut position = 0;
        for part in arguments.into_iter().chain(trailing) {
            if self.kind(part) == HirKind::Children {
                let name = self.intern_name("children");
                if let Some(&index) = signature.named.get(&name) {
                    values.insert(index, part);
                }
                continue;
            }
            for argument in self.children(part) {
                if !self.budget(argument) {
                    return None;
                }
                let Form::Argument { name, value } = self.out.hir.node(argument).form else {
                    return None;
                };
                let index = if let Some(name) = name {
                    signature.named.get(&name.name?).copied()
                } else {
                    let index = position;
                    position += 1;
                    (index < signature.parameters.len()).then_some(index)
                };
                let index = index?;
                if values.insert(index, value).is_some() {
                    return None;
                }
            }
        }
        for case in &external.result_cases {
            if !self.budget(call.node) {
                return None;
            }
            let name = self.intern_name(&case.parameter);
            let index = *signature.named.get(&name)?;
            match values.get(&index) {
                Some(&value) => {
                    if self.source_constant(value)? == case.equals {
                        return Some(self.hint(&case.result));
                    }
                }
                None if case.when_omitted => return Some(self.hint(&case.result)),
                _ => {}
            }
        }
        let parameter = external.result_parameter.as_deref()?;
        let name = self.intern_name(parameter);
        let index = signature.named.get(&name)?;
        values.get(index).map(|&value| self.ty(value))
    }
    fn spread_type_possible(&mut self, node: NodeId, ty: TypeId) -> bool {
        if !self.budget(node) {
            return true;
        }
        match &self.out.types[ty.0 as usize] {
            Type::Any | Type::Named(_) | Type::Array | Type::Tuple(_) | Type::Table => true,
            Type::Union(types) => {
                let count = types.len();
                if !self.charge_work(node, count) {
                    return true;
                }
                for i in 0..count {
                    let Type::Union(types) = &self.out.types[ty.0 as usize] else {
                        unreachable!()
                    };
                    let child = types[i];
                    if self.spread_type_possible(node, child) {
                        return true;
                    }
                }
                false
            }
            _ => false,
        }
    }

    pub(super) fn check_argument_spread(&mut self, node: NodeId) {
        // Terminal transfers have no value to expand; their inferred null is
        // only a placeholder, not a source value supplied to ArgsSpread.
        if let Form::Spread { value } = self.out.hir.node(node).form
            && !self.falls_through(value)
        {
            return;
        }
        let ty = self.ty(node);
        if !self.spread_type_possible(node, ty) {
            self.error(
                D::TypeMismatch,
                self.span(node),
                "call spread requires array, tuple or table",
            );
        } else if !matches!(self.out.types[ty.0 as usize], Type::Array | Type::Tuple(_)) {
            self.check(node, RuntimeChecks::TYPE);
        }
    }
}
