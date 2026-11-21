//! Callable expectations flow into direct lambdas before binding their bodies.
use super::*;

impl Binder<'_> {
    pub(super) fn contextual_array(&self, ty: TypeId) -> Option<(TypeId, TypeId)> {
        match &self.out.types[ty.0 as usize] {
            Type::TypedArray(item) => Some((ty, *item)),
            Type::Union(types) => {
                let mut members = types.iter().copied().filter(|&ty| ty != NULL);
                let only = members.next()?;
                if members.next().is_none() && let Type::TypedArray(item) = self.out.types[only.0 as usize] {
                    Some((only, item))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn direct_contextual_argument(&mut self, mut node: NodeId) -> bool {
        loop {
            if !self.budget(node) {
                return false;
            }
            if matches!(self.kind(node), HirKind::Closure | HirKind::Array) {
                return true;
            }
            node = match self.out.hir.node(node).form {
                Form::Argument { value, .. } => value,
                Form::Single { child } if self.kind(node) == HirKind::Group => child,
                Form::Statement {
                    expression,
                    terminated: false,
                } => expression,
                Form::Block { tail: Some(tail) } => tail,
                _ => return false,
            };
        }
    }
    fn contextual_callable(&mut self, node: NodeId, ty: TypeId) -> Option<TypeId> {
        match &self.out.types[ty.0 as usize] {
            Type::Callable(..) => Some(ty),
            Type::Union(types) => {
                let count = types.len();
                if !self.charge_work(node, count) {
                    return None;
                }
                let Type::Union(types) = &self.out.types[ty.0 as usize] else {
                    unreachable!()
                };
                let mut candidates = types.iter().copied().filter(|&ty| ty != NULL);
                let candidate = candidates.next()?;
                (candidates.next().is_none()
                    && matches!(self.out.types[candidate.0 as usize], Type::Callable(..)))
                .then_some(candidate)
            }
            _ => None,
        }
    }

    pub(super) fn contextual_signature(
        &mut self,
        node: NodeId,
        function: FunctionId,
        expected: TypeId,
    ) {
        let Some(callable) = self.contextual_callable(node, expected) else {
            return;
        };
        let count = self.out.functions[function.0 as usize].parameters.len();
        if !self.charge_work(node, count) {
            return;
        }
        for index in 0..count {
            let Type::Callable(parameters, _) = &self.out.types[callable.0 as usize] else {
                unreachable!()
            };
            let Some(&ty) = parameters.get(index) else {
                break;
            };
            let parameter = &mut self.out.functions[function.0 as usize].parameters[index];
            if parameter.annotation.is_none() && !parameter.rest {
                parameter.annotation = Some(ty);
                let symbol = parameter.symbol;
                self.out.symbols[symbol.0 as usize].ty = ty;
                self.annotations.insert(symbol);
            }
        }
        if self.out.functions[function.0 as usize].kind == FunctionKind::Plain
            && matches!(
                self.out.hir.node(node).form,
                Form::Function { result: None, .. }
            )
        {
            let Type::Callable(_, result) = &self.out.types[callable.0 as usize] else {
                unreachable!()
            };
            self.out.functions[function.0 as usize].return_type = *result;
        }
    }

    pub(super) fn contextual_arguments(&mut self, call: &CallSite, ty: TypeId, ctx: &Context) {
        let Form::Call {
            callee,
            arguments,
            children: trailing,
            ..
        } = self.out.hir.node(call.node).form
        else {
            unreachable!()
        };
        let contextual = arguments.is_some_and(|args| {
            self.children(args)
                .into_iter()
                .any(|arg| self.direct_contextual_argument(arg))
        });
        if !contextual {
            for part in arguments.into_iter().chain(trailing) {
                self.visit(part, ctx);
            }
            return;
        }
        let signature = self.checked_signature(call, callee);
        let callable = self.contextual_callable(call.node, ty);
        let mut position = Some(0usize);
        if let Some(arguments) = arguments {
            if !self.budget(arguments) {
                return;
            }
            self.out.facts[arguments.0 as usize].scope = Some(self.scope);
            for argument in self.children(arguments) {
                let expected = match self.out.hir.node(argument).form {
                    Form::Argument { name, .. } => {
                        let index = if let Some(name) = name {
                            signature
                                .as_ref()
                                .and_then(|s| s.named.get(&name.name?).copied())
                        } else {
                            let index = position;
                            position = position.map(|index| index + 1);
                            index
                        };
                        index.and_then(|index| {
                            if let Some(signature) = &signature {
                                signature
                                    .parameters
                                    .get(index)
                                    .filter(|p| !p.rest)
                                    .map(|p| p.ty)
                            } else if let Some(callable) = callable {
                                let Type::Callable(parameters, _) =
                                    &self.out.types[callable.0 as usize]
                                else {
                                    unreachable!()
                                };
                                parameters.get(index).copied()
                            } else {
                                None
                            }
                        })
                    }
                    _ => None,
                };
                self.visit_expected(argument, ctx, expected.unwrap_or(ANY));
                if self.kind(argument) == HirKind::Spread {
                    position = match &self.out.types[self.ty(argument).0 as usize] {
                        Type::Tuple(elements) => position.map(|index| index + elements.len()),
                        _ => None,
                    };
                }
                if self.exhausted {
                    return;
                }
            }
            self.set_ty(arguments, ANY);
        }
        if let Some(trailing) = trailing {
            self.visit(trailing, ctx);
        }
    }
}
