//! Control flow joins and inferred storage updates.
use super::*;

impl Inference<'_> {
    pub(super) fn has_break(&mut self, m: usize, loop_node: NodeId) -> bool {
        let mut pending = vec![loop_node];
        while let Some(node) = pending.pop() {
            if !self.charge(1) {
                return true;
            }
            let module = &self.modules[m];
            if module.hir.node(node).kind == HirKind::Break
                && module.facts[node.0 as usize]
                    .control
                    .as_ref()
                    .is_some_and(|control| control.target == Some(loop_node))
            {
                return true;
            }
            if node != loop_node
                && matches!(
                    module.hir.node(node).form,
                    Form::Function { .. } | Form::Class { .. }
                )
            {
                continue;
            }
            if !self.charge(module.hir.elements(node).len()) {
                return true;
            }
            pending.extend(module.hir.children(node));
        }
        false
    }
    pub(super) fn loop_changes(
        &mut self,
        before: &State,
        after: &State,
    ) -> (Vec<usize>, Vec<SymbolId>, bool) {
        let mut objects = Vec::new();
        let mut bindings = Vec::new();
        for (id, (a, b)) in before.objects.iter().zip(&after.objects).enumerate() {
            let width = |o: &Object| match o {
                Object::Record(f) => f.len(),
                Object::Array { values, .. } | Object::Alternatives(values) => values.len(),
            };
            if !self.charge(width(a) + width(b) + 1) {
                break;
            }
            let same = match (a, b) {
                (Object::Record(a), Object::Record(b)) => {
                    a.len() == b.len()
                        && a.iter()
                            .zip(b)
                            .all(|(a, b)| a.name == b.name && a.value.same(&b.value))
                }
                (
                    Object::Array {
                        values: a,
                        exact: x,
                    },
                    Object::Array {
                        values: b,
                        exact: y,
                    },
                ) => x == y && a.len() == b.len() && a.iter().zip(b).all(|(a, b)| a.same(b)),
                (Object::Alternatives(a), Object::Alternatives(b)) => {
                    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| a.same(b))
                }
                _ => false,
            };
            if !same {
                objects.push(id);
            }
        }
        if self.charge(before.bindings.len()) {
            for (&id, value) in &before.bindings {
                if after
                    .bindings
                    .get(&id)
                    .is_some_and(|other| !value.same(other))
                {
                    bindings.push(id);
                }
            }
        }
        (
            objects,
            bindings,
            after.returns.len() != before.returns.len(),
        )
    }
    pub(super) fn widen_loop(
        &mut self,
        m: usize,
        body: NodeId,
        changes: (Vec<usize>, Vec<SymbolId>, bool),
        state: &mut State,
    ) {
        let (objects, bindings, returns) = changes;
        if objects.is_empty() && bindings.is_empty() {
            return;
        }
        for id in objects {
            self.invalidated(
                &Value {
                    object: Some(id),
                    ..Value::plain(self.unknown.clone())
                },
                state,
            );
        }
        for symbol in bindings {
            if let Some(value) = state.bindings.get(&symbol).cloned() {
                self.invalidated(&value, state);
                state
                    .bindings
                    .insert(symbol, Value::plain(self.unknown.clone()));
            }
        }
        if returns {
            state.returns.push(self.unknown.clone());
        }
        let mut pending = vec![body];
        while let Some(node) = pending.pop() {
            if !self.charge(self.modules[m].hir.elements(node).len() + 1) {
                return;
            }
            self.facts[m].values[node.0 as usize] = None;
            if let Some(symbol) = self.modules[m].facts[node.0 as usize].symbol {
                self.facts[m].symbols[symbol.0 as usize] = None;
            }
            if matches!(self.modules[m].hir.node(node).form, Form::Function { .. })
                && let Some(function) = self.modules[m].facts[node.0 as usize].function
            {
                self.facts[m].functions[function.0 as usize] = Some(self.unknown.clone());
            }
            self.facts[m].external.remove(&node);
            pending.extend(self.modules[m].hir.children(node));
        }
    }
    pub(super) fn insert_field(
        fields: &mut Vec<Field>,
        positions: &mut HashMap<String, usize>,
        field: Field,
    ) {
        if let Some(&index) = positions.get(&field.name) {
            fields[index] = field;
        } else {
            positions.insert(field.name.clone(), fields.len());
            fields.push(field);
        }
    }
    pub(super) fn bind_pattern(&mut self, m: usize, n: NodeId, value: Value, state: &mut State) {
        if let Some(id) = self.modules[m].facts[n.0 as usize].symbol {
            let info = self.snapshot(&value, state);
            self.facts[m].symbols[id.0 as usize] = Some(info.clone());
            self.facts[m].values[n.0 as usize] = Some(info);
            state.bindings.insert(id, value);
        } else {
            for (index, child) in self.modules[m].hir.children(n).enumerate() {
                let item = if self.modules[m].hir.node(n).kind == HirKind::TuplePattern {
                    self.subscript(&value, Some(index), state)
                        .unwrap_or_else(|| Value::plain(self.unknown.clone()))
                } else {
                    Value::plain(self.unknown.clone())
                };
                self.bind_pattern(m, child, item, state);
            }
        }
    }
    pub(super) fn location(
        &mut self,
        m: usize,
        n: NodeId,
        state: &mut State,
        depth: usize,
    ) -> (Location, Arc<SourceType>) {
        let unknown = self.unknown.clone();
        match self.modules[m].hir.node(n).form {
            Form::Name { .. } => {
                if let Some(id) = self.modules[m].facts[n.0 as usize].symbol {
                    let value = self.symbol(m, id, state);
                    let info = self.snapshot(&value, state);
                    (Location::Symbol(id), info)
                } else {
                    (Location::Unknown, unknown)
                }
            }
            Form::Member { object, key, .. } => {
                let receiver = self.visit(m, object, state, depth);
                let key = self.modules[m].hir.text(key.span).to_owned();
                let old = self
                    .member_value(&receiver, &key, state, 0)
                    .map(|value| self.snapshot(&value, state))
                    .unwrap_or(unknown);
                (
                    Location::Field {
                        receiver,
                        key: Some(key),
                        index: None,
                    },
                    old,
                )
            }
            Form::Index { object, index, .. } => {
                let receiver = self.visit(m, object, state, depth);
                self.visit(m, index, state, depth);
                let key = self.key(m, index);
                let index = self.array_index(m, index);
                let old = if let Some(key) = &key {
                    self.member_value(&receiver, key, state, 0)
                } else {
                    self.subscript(&receiver, index, state)
                };
                let info = old
                    .map(|value| self.snapshot(&value, state))
                    .unwrap_or(unknown);
                (
                    Location::Field {
                        receiver,
                        key,
                        index,
                    },
                    info,
                )
            }
            _ => (Location::Unknown, unknown),
        }
    }
    pub(super) fn compound(
        &self,
        operator: K,
        old: &SourceType,
        rhs: &SourceType,
    ) -> Arc<SourceType> {
        let ty = match (&old.ty, &rhs.ty) {
            (TypeHint::Int, TypeHint::Int) => TypeHint::Int,
            (TypeHint::Int | TypeHint::Float, TypeHint::Int | TypeHint::Float) => TypeHint::Float,
            (TypeHint::String, TypeHint::String) if operator == K::PlusEq => TypeHint::String,
            _ => TypeHint::Any,
        };
        self.leaf(ty)
    }
    pub(super) fn write_field(
        &mut self,
        m: usize,
        n: NodeId,
        receiver: Value,
        access: (Option<String>, Option<usize>),
        value: Value,
        state: &mut State,
    ) {
        let (key, index) = access;
        let Some(id) = receiver.object else {
            return;
        };
        if state.opaque.contains(&id) {
            self.invalidated(&value, state);
            return;
        }
        match &mut state.objects[id] {
            Object::Record(fields) => {
                if let Some(key) = key {
                    if !self.charge(fields.len()) {
                        return;
                    }
                    if let Some(field) = fields.iter_mut().find(|field| field.name == key) {
                        field.value = value;
                    } else {
                        let range = match self.modules[m].hir.node(n).form {
                            Form::Member { key, .. } => key.span,
                            _ => self.modules[m].hir.node(n).span,
                        };
                        fields.push(Field {
                            name: key,
                            value,
                            definition: SourceDefinition {
                                module: self.module_names[m].clone(),
                                range,
                            },
                        });
                    }
                    self.changed(state);
                } else {
                    self.invalidated(&receiver, state);
                }
            }
            Object::Array { values, exact } => {
                if *exact && let Some(slot) = index.and_then(|index| values.get_mut(index)) {
                    *slot = value;
                    self.changed(state);
                } else {
                    self.invalidated(&receiver, state);
                }
            }
            Object::Alternatives(_) => self.invalidated(&receiver, state),
        }
    }
    pub(super) fn changed(&mut self, state: &mut State) {
        // A changed child invalidates cached parents without scanning every
        // object on each write. Snapshots already published to queries stay valid.
        state.revision += 1;
    }
    pub(super) fn clone_state(&mut self, state: &State) -> Option<State> {
        let slots = state.bindings.len()
            + state.cache.len()
            + state.returns.len()
            + state.yields.len()
            + state.imports.len()
            + state.imported_graph.len()
            + state.opaque.len()
            + state
                .objects
                .iter()
                .map(|object| match object {
                    Object::Record(fields) => fields.len(),
                    Object::Array { values, .. } | Object::Alternatives(values) => values.len(),
                })
                .sum::<usize>();
        self.charge(slots).then(|| state.clone())
    }
    pub(super) fn transfer(
        &mut self,
        value: &Value,
        origin: &mut State,
        into: &mut State,
        shared: usize,
        memo: &mut TransferMemo,
        depth: usize,
    ) -> Value {
        if depth >= self.limits.max_depth || !self.charge(1) {
            return Value::plain(self.unknown.clone());
        }
        let opaque = value.object.is_some_and(|id| origin.opaque.contains(&id));
        let mut result = if value.object.is_some_and(|id| id >= shared) {
            let info = self.snapshot(value, origin);
            self.materialize_shared(info, into, &mut memo.objects, 0)
        } else {
            value.clone()
        };
        if opaque && let Some(id) = result.object {
            into.opaque.insert(id);
        }
        if let Some(captures) = &value.captures {
            let key = captures.as_ptr() as usize;
            if let Some(captures) = memo.captures.get(&key) {
                result.captures = Some(captures.clone());
                return result;
            }
            if !self.charge(captures.len()) {
                return Value::plain(self.unknown.clone());
            }
            let mut values = Vec::with_capacity(captures.len());
            for value in captures.iter() {
                values.push(self.transfer(value, origin, into, shared, memo, depth + 1));
                if self.exhausted {
                    return Value::plain(self.unknown.clone());
                }
            }
            let captures: Arc<[Value]> = values.into();
            memo.captures.insert(key, captures.clone());
            result.captures = Some(captures);
        }
        result
    }
    pub(super) fn merge_value(
        &mut self,
        a: &Value,
        b: &Value,
        states: (&mut State, &mut State),
        into: &mut State,
        shared: usize,
        memos: (&mut TransferMemo, &mut TransferMemo),
    ) -> Value {
        let (left, right) = states;
        let (left_memo, right_memo) = memos;
        if a.same(b) && a.object.is_none_or(|id| id < shared) {
            return a.clone();
        }
        if a.object.is_some() && a.object == b.object && a.object.unwrap() < shared {
            return a.clone();
        }
        let ai = self.snapshot(a, left);
        let bi = self.snapshot(b, right);
        let info = self.join(ai, bi);
        let a = self.transfer(a, left, into, shared, left_memo, 0);
        let b = self.transfer(b, right, into, shared, right_memo, 0);
        if a.object.is_some() || b.object.is_some() {
            return into.object(info, Object::Alternatives(vec![a, b]));
        }
        let mut captures = Vec::new();
        let mut seen = HashSet::new();
        for values in a.captures.iter().chain(b.captures.iter()) {
            if !self.charge(values.len()) {
                return Value::plain(self.unknown.clone());
            }
            for value in values.iter() {
                let key = (
                    value.object,
                    value.receiver,
                    value.captures.as_ref().map(|v| v.as_ptr() as usize),
                );
                if seen.insert(key) {
                    captures.push(value.clone());
                }
            }
        }
        Value {
            info,
            object: None,
            receiver: None,
            captures: (!captures.is_empty()).then(|| captures.into()),
        }
    }
    pub(super) fn merge_states(&mut self, into: &mut State, mut a: State, mut b: State) {
        let shared = into.objects.len();
        let mut left_memo = TransferMemo::default();
        let mut right_memo = TransferMemo::default();
        let changed = a.revision != into.revision || b.revision != into.revision;
        for id in 0..if changed { shared } else { 0 } {
            let width = |object: &Object| match object {
                Object::Record(fields) => fields.len(),
                Object::Array { values, .. } | Object::Alternatives(values) => values.len(),
            };
            if !self.charge(width(&a.objects[id]) + width(&b.objects[id]) + 1) {
                return;
            }
            let merged = match (a.objects[id].clone(), b.objects[id].clone()) {
                (Object::Record(left), Object::Record(right)) => {
                    let mut fields = Vec::new();
                    for field in left {
                        if !self.charge(right.len()) {
                            break;
                        }
                        if let Some(other) = right.iter().find(|other| other.name == field.name) {
                            let value = self.merge_value(
                                &field.value,
                                &other.value,
                                (&mut a, &mut b),
                                into,
                                shared,
                                (&mut left_memo, &mut right_memo),
                            );
                            fields.push(Field { value, ..field });
                        }
                    }
                    Object::Record(fields)
                }
                (
                    Object::Array {
                        values: left,
                        exact: le,
                    },
                    Object::Array {
                        values: right,
                        exact: re,
                    },
                ) if le == re && left.len() == right.len() => {
                    let mut values = Vec::with_capacity(left.len());
                    for (left, right) in left.iter().zip(&right) {
                        if !self.charge(1) {
                            return;
                        }
                        values.push(self.merge_value(
                            left,
                            right,
                            (&mut a, &mut b),
                            into,
                            shared,
                            (&mut left_memo, &mut right_memo),
                        ));
                        if self.exhausted {
                            return;
                        }
                    }
                    Object::Array { values, exact: le }
                }
                _ => {
                    let original = Value {
                        object: Some(id),
                        ..Value::plain(self.unknown.clone())
                    };
                    let ai = self.snapshot(&original, &mut a);
                    let bi = self.snapshot(&original, &mut b);
                    let info = self.join(ai, bi);
                    let value = self.materialize(info, into);
                    match value.object {
                        Some(new) => into.objects[new].clone(),
                        None => Object::Alternatives(Vec::new()),
                    }
                }
            };
            into.objects[id] = merged;
        }
        if !self.charge(
            a.bindings.len()
                + b.bindings.len()
                + a.returns.len()
                + b.returns.len()
                + a.yields.len()
                + b.yields.len(),
        ) {
            return;
        }
        let symbols: HashSet<_> = a
            .bindings
            .keys()
            .chain(b.bindings.keys())
            .copied()
            .collect();
        for symbol in symbols {
            if !self.charge(1) {
                return;
            }
            let av = a.bindings.get(&symbol).cloned();
            let bv = b.bindings.get(&symbol).cloned();
            if let (Some(av), Some(bv)) = (av, bv) {
                let value = self.merge_value(
                    &av,
                    &bv,
                    (&mut a, &mut b),
                    into,
                    shared,
                    (&mut left_memo, &mut right_memo),
                );
                into.bindings.insert(symbol, value);
            }
        }
        into.returns = a.returns;
        let mut seen: HashSet<_> = into
            .returns
            .iter()
            .map(|v| Arc::as_ptr(v) as usize)
            .collect();
        for value in b.returns {
            if seen.insert(Arc::as_ptr(&value) as usize) {
                into.returns.push(value);
            }
        }
        into.yields = a.yields;
        let mut seen: HashSet<_> = into
            .yields
            .iter()
            .map(|v| Arc::as_ptr(v) as usize)
            .collect();
        for value in b.yields {
            if seen.insert(Arc::as_ptr(&value) as usize) {
                into.yields.push(value);
            }
        }
        if !self.charge(a.opaque.len() + b.opaque.len()) {
            return;
        }
        into.opaque
            .extend(a.opaque.into_iter().filter(|id| *id < shared));
        into.opaque
            .extend(b.opaque.into_iter().filter(|id| *id < shared));
        into.live = a.live || b.live;
        into.globals_dirty = a.globals_dirty || b.globals_dirty;
        if changed {
            self.changed(into);
        }
    }
}
