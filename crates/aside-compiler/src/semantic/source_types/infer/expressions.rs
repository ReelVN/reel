//! Expression evaluation for source type inference.
use super::*;

impl Inference<'_> {
    pub(super) fn visit(&mut self, m: usize, n: NodeId, state: &mut State, depth: usize) -> Value {
        if self.visiting >= self.limits.max_depth {
            return Value::plain(self.unknown.clone());
        }
        self.visiting += 1;
        let result = self.visit_inner(m, n, state, depth);
        self.visiting -= 1;
        result
    }
    pub(super) fn visit_inner(
        &mut self,
        m: usize,
        n: NodeId,
        state: &mut State,
        depth: usize,
    ) -> Value {
        if !state.live || !self.charge(1) || depth >= self.limits.max_depth {
            return Value::plain(self.unknown.clone());
        }
        let module = &self.modules[m];
        let form = module.hir.node(n).form;
        let kind = module.hir.node(n).kind;
        let base = self.base(m, n);
        if !self.charge(module.hir.elements(n).len()) {
            return Value::plain(self.unknown.clone());
        }
        let children: Vec<_> = module.hir.children(n).collect();
        let mut result = Value::plain(base.clone());
        match form {
            Form::Function { .. } => {
                if kind == HirKind::Closure
                    && let Some(function) = module.facts[n.0 as usize].function
                {
                    result = Value {
                        info: self.callable(m, function),
                        object: None,
                        receiver: None,
                        captures: self.captured(m, function, state),
                    };
                }
            }
            Form::Name { .. } => {
                if let Some(external) = self.external(m, n) {
                    result = self.external_value(&external, state);
                } else if let Some(id) = module.facts[n.0 as usize].symbol {
                    result = self.symbol(m, id, state);
                    if module.facts[n.0 as usize].ty != module.symbols[id.0 as usize].ty
                        && result.object.is_none()
                        && self.base(m, n).ty != TypeHint::Any
                    {
                        result = Value::plain(self.base(m, n));
                    }
                }
            }
            Form::Binding {
                pattern,
                initializer,
                ..
            } => {
                let value = initializer
                    .map(|node| self.visit(m, node, state, depth + 1))
                    .unwrap_or_else(|| Value::plain(self.unknown.clone()));
                self.bind_pattern(m, pattern, value, state);
            }
            Form::Block { tail } => {
                for child in children {
                    if self.exhausted {
                        return Value::plain(self.unknown.clone());
                    }
                    let value = self.visit(m, child, state, depth + 1);
                    if Some(child) == tail {
                        result = value;
                    }
                    if !state.live {
                        break;
                    }
                }
            }
            Form::Statement {
                expression,
                terminated,
            } => {
                let value = self.visit(m, expression, state, depth + 1);
                if !terminated {
                    result = value;
                }
            }
            Form::Single { child } if kind == HirKind::Loop => {
                let Some(before) = self.clone_state(state) else {
                    return Value::plain(self.unknown.clone());
                };
                self.visit(m, child, state, depth + 1);
                let changes = self.loop_changes(&before, state);
                state.live = self.has_break(m, n);
                self.widen_loop(m, child, changes, state);
            }
            Form::Single { child }
            | Form::Spread { value: child }
            | Form::Argument { value: child, .. } => {
                result = self.visit(m, child, state, depth + 1);
            }
            Form::Modified { declaration, .. } => {
                result = self.visit(m, declaration, state, depth + 1);
            }
            Form::Member { object, key, .. } => {
                let value = self.visit(m, object, state, depth + 1);
                let info = self.snapshot(&value, state);
                if let Some(field) = self.field_info(&info, module.hir.text(key.span)) {
                    self.facts[m]
                        .definitions
                        .insert(n, field.definition.clone());
                    result = self
                        .member_value(&value, module.hir.text(key.span), state, 0)
                        .unwrap_or_else(|| self.materialize(field.value.clone(), state));
                } else if let TypeHint::Named(name) = &info.ty
                    && let Some(member) = self
                        .resolver
                        .resolve_type_member(name, module.hir.text(key.span))
                {
                    let symbol = Arc::new(member.symbol);
                    result = self.external_value(&symbol, state);
                    self.facts[m].external.insert(n, symbol);
                } else if let Some(external) = self.external(m, n) {
                    result = self.external_value(&external, state);
                    if external
                        .signature
                        .as_ref()
                        .is_none_or(|signature| signature.derived_safe != Some(true))
                    {
                        result.receiver = value.object;
                    }
                } else if let Some(member) = super::super::super::builtin_catalog()
                    .types
                    .iter()
                    .find(|group| group.receiver == info.ty)
                    .and_then(|group| {
                        group
                            .members
                            .iter()
                            .find(|member| member.symbol.name == module.hir.text(key.span))
                    })
                {
                    let external = Arc::new(member.symbol.clone());
                    result = self.external_value(&external, state);
                    if external
                        .signature
                        .as_ref()
                        .is_none_or(|signature| signature.derived_safe != Some(true))
                    {
                        result.receiver = value.object;
                    }
                    self.facts[m].external.insert(n, external);
                } else {
                    result.receiver = value.object;
                }
                if module.hir.text(key.span) == "next"
                    && let Some(SourceShape::Generator {
                        element,
                        result: returned,
                    }) = &info.shape
                {
                    let item = self.join(element.clone(), returned.clone());
                    let item = self.join(item, self.leaf(TypeHint::Null));
                    let done = self.leaf(TypeHint::Bool);
                    let pair = Arc::new(SourceType {
                        ty: TypeHint::Tuple(vec![TypeHint::Bool, item.ty.clone()]),
                        shape: Some(SourceShape::Tuple(vec![done, item].into())),
                        ..SourceType::new(TypeHint::Any)
                    });
                    result.info = Arc::new(SourceType {
                        call_result: Some(pair),
                        ..(*result.info).clone()
                    });
                    if let Some(external) = self.facts[m]
                        .external
                        .get(&n)
                        .cloned()
                        .or_else(|| self.external(m, n))
                    {
                        let mut external = (*external).clone();
                        external.source_type = Some(result.info.clone());
                        self.facts[m].external.insert(n, Arc::new(external));
                    }
                }
            }
            Form::Index { object, index, .. } => {
                let value = self.visit(m, object, state, depth + 1);
                self.visit(m, index, state, depth + 1);
                let info = self.snapshot(&value, state);
                if let Some(key) = self.key(m, index) {
                    if let Some(field) = self.field_info(&info, &key) {
                        self.facts[m]
                            .definitions
                            .insert(n, field.definition.clone());
                        result = self
                            .member_value(&value, &key, state, 0)
                            .unwrap_or_else(|| self.materialize(field.value.clone(), state));
                    }
                } else if matches!(&info.ty, TypeHint::Named(name) if name == "Bytes") {
                    result = self.materialize(self.leaf(TypeHint::Int), state);
                } else if matches!(info.shape, Some(SourceShape::Tuple(_)))
                    || info.element().is_some()
                {
                    result = self
                        .subscript(&value, self.array_index(m, index), state)
                        .unwrap_or_else(|| {
                            self.materialize(
                                info.element()
                                    .cloned()
                                    .unwrap_or_else(|| self.unknown.clone()),
                                state,
                            )
                        });
                }
            }
            Form::Call {
                callee,
                arguments,
                children: trailing,
                ..
            } => {
                let callable = self.visit(m, callee, state, depth + 1);
                let mut supplied = Vec::new();
                for argument in arguments
                    .into_iter()
                    .flat_map(|node| module.hir.children(node))
                    .chain(trailing)
                {
                    supplied.push(self.visit(m, argument, state, depth + 1));
                }
                if let Some(info) = &callable.info.call_result {
                    result = self.materialize(info.clone(), state);
                }
                let descriptor = self.facts[m]
                    .external
                    .get(&callee)
                    .cloned()
                    .or_else(|| self.external(m, callee));
                let pure = descriptor
                    .as_ref()
                    .and_then(|symbol| symbol.signature.as_ref())
                    .is_some_and(|signature| signature.derived_safe == Some(true));
                if !pure {
                    for value in supplied {
                        self.invalidated(&value, state);
                    }
                    if let Some(id) = callable.receiver {
                        self.invalidated(
                            &Value {
                                object: Some(id),
                                ..Value::plain(self.unknown.clone())
                            },
                            state,
                        );
                    }
                    self.invalidated(&callable, state);
                    if !self.charge(state.imports.len()) {
                        return Value::plain(self.unknown.clone());
                    }
                    let globals: Vec<_> = state
                        .imports
                        .values()
                        .filter(|value| value.object.is_some() || value.captures.is_some())
                        .cloned()
                        .collect();
                    for value in globals {
                        self.invalidated(&value, state);
                    }
                    if !self.charge(state.bindings.len()) {
                        return Value::plain(self.unknown.clone());
                    }
                    let globals: Vec<_> = state
                        .bindings
                        .iter()
                        .filter(|(id, _)| {
                            module.symbols[id.0 as usize].origin == BindingOrigin::ModuleGlobal
                        })
                        .map(|(_, value)| value.clone())
                        .collect();
                    for value in globals {
                        self.invalidated(&value, state);
                    }
                    state.globals_dirty = true;
                }
                if base.ty != TypeHint::Any
                    && result.info.ty != base.ty
                    && !matches!(result.info.shape, Some(SourceShape::Tuple(_)))
                {
                    result.info = Arc::new(SourceType {
                        ty: base.ty.clone(),
                        ..(*result.info).clone()
                    });
                }
            }
            Form::Transfer { value, .. } => {
                let value = value
                    .map(|node| self.visit(m, node, state, depth + 1))
                    .unwrap_or_else(|| Value::plain(self.leaf(TypeHint::Null)));
                if kind == HirKind::Return {
                    let value = self.snapshot(&value, state);
                    state.returns.push(value);
                }
                if kind == HirKind::Yield {
                    let info = self.snapshot(&value, state);
                    state.yields.push(info);
                    self.invalidated(&value, state);
                    result = Value::plain(self.leaf(TypeHint::Null));
                } else {
                    state.live = false;
                }
            }
            Form::Binary {
                left,
                operator,
                right,
            } if kind != HirKind::Assignment => {
                let a = self.visit(m, left, state, depth + 1);
                let b = self.visit(m, right, state, depth + 1);
                if result.info.ty == TypeHint::Any
                    && matches!(
                        operator.kind,
                        K::Plus
                            | K::Minus
                            | K::Star
                            | K::Slash
                            | K::Percent
                            | K::Shl
                            | K::Shr
                            | K::Amp
                            | K::Pipe
                            | K::Caret
                    )
                {
                    let a = self.snapshot(&a, state);
                    let b = self.snapshot(&b, state);
                    let kind = if operator.kind == K::Plus {
                        K::PlusEq
                    } else {
                        operator.kind
                    };
                    result = Value::plain(self.compound(kind, &a, &b));
                }
            }
            Form::Condition {
                condition,
                body,
                otherwise,
                ..
            } => {
                self.visit(m, condition, state, depth + 1);
                let Some(mut yes) = self.clone_state(state) else {
                    return Value::plain(self.unknown.clone());
                };
                let a = self.visit(m, body, &mut yes, depth + 1);
                let a = self.snapshot(&a, &mut yes);
                let Some(mut no) = self.clone_state(state) else {
                    return Value::plain(self.unknown.clone());
                };
                let b = otherwise
                    .map(|node| self.visit(m, node, &mut no, depth + 1))
                    .unwrap_or_else(|| Value::plain(self.leaf(TypeHint::Null)));
                let b = self.snapshot(&b, &mut no);
                let joined = if kind == HirKind::While {
                    base.clone()
                } else {
                    match (yes.live, no.live) {
                        (true, true) => self.join(a, b),
                        (true, false) => a,
                        (false, true) => b,
                        (false, false) => self.unknown.clone(),
                    }
                };
                let changes = (kind == HirKind::While).then(|| self.loop_changes(state, &yes));
                self.merge_states(state, yes, no);
                if let Some(changes) = changes {
                    self.widen_loop(m, body, changes, state);
                }
                result = self.materialize(joined, state);
            }
            Form::For {
                pattern,
                source,
                body,
                ..
            } => {
                let values = self.visit(m, source, state, depth + 1);
                let info = self.snapshot(&values, state);
                let value = self.subscript(&values, None, state).unwrap_or_else(|| {
                    self.materialize(
                        info.element()
                            .cloned()
                            .unwrap_or_else(|| self.unknown.clone()),
                        state,
                    )
                });
                self.bind_pattern(m, pattern, value, state);
                let Some(before) = self.clone_state(state) else {
                    return Value::plain(self.unknown.clone());
                };
                let Some(mut after) = self.clone_state(state) else {
                    return Value::plain(self.unknown.clone());
                };
                self.visit(m, body, &mut after, depth + 1);
                let changes = self.loop_changes(&before, &after);
                self.merge_states(state, before, after);
                self.widen_loop(m, body, changes, state);
                state.live = true;
            }
            Form::Try { .. } | Form::Match { .. } => {
                // Exceptional and pattern-dependent paths retain the bound HIR
                // contract until their separate flows can be proven together.
                let returns = state.returns.len();
                for child in children {
                    state.live = true;
                    self.visit(m, child, state, depth + 1);
                    if self.exhausted {
                        return Value::plain(self.unknown.clone());
                    }
                }
                if state.returns.len() != returns {
                    state.returns.push(self.unknown.clone());
                }
                state.live = true;
                if !self.charge(state.bindings.len()) {
                    return Value::plain(self.unknown.clone());
                }
                let values: Vec<_> = state.bindings.values().cloned().collect();
                for value in values {
                    self.invalidated(&value, state);
                }
                for value in state.bindings.values_mut() {
                    value.info = self.unknown.clone();
                }
                result = Value::plain(self.unknown.clone());
            }
            _ if kind == HirKind::Object => {
                let mut fields = Vec::new();
                let mut positions = HashMap::new();
                for child in children {
                    if self.exhausted {
                        return Value::plain(self.unknown.clone());
                    }
                    if let Form::Field { key, value } = module.hir.node(child).form {
                        let value = self.visit(m, value, state, depth + 1);
                        let field_info = self.snapshot(&value, state);
                        self.facts[m].values[child.0 as usize] = Some(field_info);
                        let (name, range) = match key {
                            FieldKey::Name(token) => {
                                (Some(module.hir.text(token.span).to_owned()), token.span)
                            }
                            FieldKey::Expression(node) => {
                                (self.key(m, node), module.hir.node(node).span)
                            }
                        };
                        if let Some(name) = name {
                            let definition = SourceDefinition {
                                module: self.module_names[m].clone(),
                                range,
                            };
                            self.facts[m].definitions.insert(child, definition.clone());
                            Self::insert_field(
                                &mut fields,
                                &mut positions,
                                Field {
                                    name,
                                    value,
                                    definition,
                                },
                            );
                        }
                    } else {
                        let spread_value = self.visit(m, child, state, depth + 1);
                        if let Some(id) = spread_value.object
                            && let Object::Record(members) = &state.objects[id]
                        {
                            if !self.charge(members.len()) {
                                return Value::plain(self.unknown.clone());
                            }
                            for field in members {
                                Self::insert_field(&mut fields, &mut positions, field.clone());
                            }
                            continue;
                        }
                        let spread = self.snapshot(&spread_value, state);
                        if let Some(SourceShape::Record(members)) = &spread.shape {
                            for field in members {
                                if !self.charge(1) {
                                    return Value::plain(self.unknown.clone());
                                }
                                let value = self
                                    .member_value(&spread_value, &field.name, state, 0)
                                    .unwrap_or_else(|| {
                                        self.materialize(field.value.clone(), state)
                                    });
                                Self::insert_field(
                                    &mut fields,
                                    &mut positions,
                                    Field {
                                        name: field.name.clone(),
                                        value,
                                        definition: field.definition.clone(),
                                    },
                                );
                            }
                        } else {
                            if !self.charge(fields.len()) {
                                return Value::plain(self.unknown.clone());
                            }
                            for field in &mut fields {
                                field.value = Value::plain(self.unknown.clone());
                            }
                        }
                    }
                }
                result = state.object(self.leaf(TypeHint::Table), Object::Record(fields));
            }
            _ if matches!(kind, HirKind::Array | HirKind::Tuple) => {
                let mut values = Vec::new();
                let mut exact = true;
                for child in children {
                    if self.exhausted {
                        return Value::plain(self.unknown.clone());
                    }
                    let value = self.visit(m, child, state, depth + 1);
                    if module.hir.node(child).kind == HirKind::Spread {
                        if let Some(id) = value.object
                            && let Object::Array {
                                values: items,
                                exact: known,
                            } = &state.objects[id]
                        {
                            if !self.charge(items.len()) {
                                return Value::plain(self.unknown.clone());
                            }
                            values.extend(items.iter().cloned());
                            exact &= *known;
                        } else {
                            values.push(Value::plain(self.unknown.clone()));
                            exact = false;
                        }
                    } else {
                        values.push(value);
                    }
                }
                let ty = if kind == HirKind::Tuple {
                    // The shape is a shared DAG. An owned TypeHint tuple tree
                    // would expand aliases such as t=(previous,previous).
                    TypeHint::Named("tuple".into())
                } else {
                    TypeHint::Array
                };
                result = state.object(self.leaf(ty), Object::Array { values, exact });
            }
            _ if kind == HirKind::Assignment => {
                if let Some((&target, rest)) = children.split_first() {
                    let (location, old) = self.location(m, target, state, depth + 1);
                    let mut rhs = rest
                        .first()
                        .map(|&node| self.visit(m, node, state, depth + 1))
                        .unwrap_or_else(|| Value::plain(self.unknown.clone()));
                    let operator = module
                        .hir
                        .tokens(n)
                        .next()
                        .map(|token| token.kind)
                        .unwrap_or(K::Eq);
                    if operator != K::Eq {
                        let info = self.snapshot(&rhs, state);
                        rhs = Value::plain(self.compound(operator, &old, &info));
                    }
                    match location {
                        Location::Symbol(id) => {
                            state.bindings.insert(id, rhs);
                        }
                        Location::Field {
                            receiver,
                            key,
                            index,
                        } => self.write_field(m, target, receiver, (key, index), rhs, state),
                        Location::Unknown => {}
                    }
                }
            }
            _ => {
                for child in children {
                    self.visit(m, child, state, depth + 1);
                }
            }
        }
        if self.exhausted {
            return Value::plain(self.unknown.clone());
        }
        let info = self.snapshot(&result, state);
        self.facts[m].values[n.0 as usize] = Some(info);
        result
    }
}
