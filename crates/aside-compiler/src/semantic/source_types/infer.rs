use super::*;
use crate::{
    SyntaxKind as K,
    hir::*,
    semantic::{AnalysisLimits, ModuleInterface, ModuleResolver},
};
use std::collections::{BTreeMap, HashSet};

#[derive(Clone)]
struct Value {
    info: Arc<SourceType>,
    object: Option<usize>,
    receiver: Option<usize>,
    captures: Option<Arc<[Value]>>,
}
impl Value {
    fn plain(info: Arc<SourceType>) -> Self {
        Self {
            info,
            object: None,
            receiver: None,
            captures: None,
        }
    }
    fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.info, &other.info)
            && self.object == other.object
            && self.receiver == other.receiver
            && match (&self.captures, &other.captures) {
                (None, None) => true,
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                _ => false,
            }
    }
}
#[derive(Default)]
struct TransferMemo {
    objects: HashMap<usize, Value>,
    captures: HashMap<usize, Arc<[Value]>>,
}
#[derive(Clone)]
struct Field {
    name: String,
    value: Value,
    definition: SourceDefinition,
}
#[derive(Clone)]
enum Object {
    Record(Vec<Field>),
    Array { values: Vec<Value>, exact: bool },
    Alternatives(Vec<Value>),
}
#[derive(Clone)]
struct State {
    bindings: HashMap<SymbolId, Value>,
    objects: Vec<Object>,
    cache: Vec<Option<(usize, Arc<SourceType>)>>,
    returns: Vec<Arc<SourceType>>,
    yields: Vec<Arc<SourceType>>,
    live: bool,
    imports: HashMap<(String, String), Value>,
    imported_graph: HashMap<usize, Value>,
    globals_dirty: bool,
    revision: usize,
    opaque: HashSet<usize>,
}
impl State {
    fn new() -> Self {
        Self {
            bindings: HashMap::new(),
            objects: Vec::new(),
            cache: Vec::new(),
            returns: Vec::new(),
            yields: Vec::new(),
            live: true,
            imports: HashMap::new(),
            imported_graph: HashMap::new(),
            globals_dirty: false,
            revision: 0,
            opaque: HashSet::new(),
        }
    }
    fn object(&mut self, info: Arc<SourceType>, object: Object) -> Value {
        let id = self.objects.len();
        self.objects.push(object);
        self.cache.push(None);
        Value {
            info,
            object: Some(id),
            receiver: None,
            captures: None,
        }
    }
}

enum Location {
    Symbol(SymbolId),
    Field {
        receiver: Value,
        key: Option<String>,
        index: Option<usize>,
    },
    Unknown,
}

struct Inference<'a> {
    modules: &'a [AnalyzedModule],
    resolver: &'a mut dyn ModuleResolver,
    facts: Vec<SourceTypes>,
    type_info: Vec<Vec<Arc<SourceType>>>,
    external_nodes: Vec<HashMap<NodeId, usize>>,
    external_symbols: Vec<HashMap<SymbolId, usize>>,
    initializers: Vec<HashMap<SymbolId, NodeId>>,
    module_names: Vec<Arc<str>>,
    exports: BTreeMap<(String, String), (usize, SymbolId)>,
    active: HashSet<(usize, FunctionId)>,
    active_globals: HashSet<(usize, SymbolId)>,
    unknown: Arc<SourceType>,
    limits: AnalysisLimits,
    work: usize,
    module_work: Vec<usize>,
    current_module: usize,
    exhausted: bool,
    visiting: usize,
}

/// Infer each body once using bound declarations. Cross-module calls follow the
/// same export identities as the linker, without parsing or rebinding modules.
pub(crate) fn analyze(
    modules: &mut [AnalyzedModule],
    resolver: &mut dyn ModuleResolver,
    limits: AnalysisLimits,
) -> Result<usize, usize> {
    let admitted = modules
        .iter()
        .map(|m| {
            m.facts
                .len()
                .saturating_add(m.symbols.len())
                .saturating_add(m.functions.len())
                .saturating_add(m.types.len())
                .saturating_add(m.exports.len())
        })
        .fold(0usize, usize::saturating_add);
    if admitted > limits.max_work {
        return Err(admitted);
    }
    // Summaries are optional editor data. Bound both work and the combined
    // inference stack even when the host uses unrestricted compiler limits.
    let limits = AnalysisLimits {
        max_work: limits.max_work.min(
            admitted
                .saturating_mul(64)
                .saturating_add(4096)
                .min(16_000_000),
        ),
        max_depth: limits.max_depth.min(32),
        ..limits
    };
    if admitted > limits.max_work {
        return Err(admitted);
    }
    let mut engine = Inference {
        facts: modules
            .iter()
            .map(|module| SourceTypes {
                values: vec![None; module.facts.len()],
                symbols: vec![None; module.symbols.len()],
                functions: vec![None; module.functions.len()],
                ..Default::default()
            })
            .collect(),
        modules,
        resolver,
        type_info: Vec::new(),
        external_nodes: Vec::new(),
        external_symbols: Vec::new(),
        initializers: Vec::new(),
        module_names: modules
            .iter()
            .map(|module| Arc::from(module.canonical_path.as_str()))
            .collect(),
        exports: BTreeMap::new(),
        active: HashSet::new(),
        active_globals: HashSet::new(),
        unknown: Arc::new(SourceType::new(TypeHint::Any)),
        limits,
        work: admitted,
        module_work: modules
            .iter()
            .map(|m| {
                m.facts.len()
                    + m.symbols.len()
                    + m.functions.len()
                    + m.types.len()
                    + m.exports.len()
            })
            .collect(),
        current_module: 0,
        exhausted: false,
        visiting: 0,
    };
    for (index, module) in modules.iter().enumerate() {
        engine.current_module = index;
        if !engine.charge(module.external.len() + module.hir.nodes.len()) {
            return Err(engine.work);
        }
        let mut nodes = HashMap::new();
        let mut symbols = HashMap::new();
        let mut initializers = HashMap::new();
        for (i, binding) in module.external.iter().enumerate() {
            if let Some(node) = binding.node {
                nodes.insert(node, i);
            }
            if let Some(symbol) = binding.symbol {
                symbols.entry(symbol).or_insert(i);
            }
        }
        for node in &module.hir.nodes {
            if let Form::Binding {
                pattern,
                initializer: Some(value),
                ..
            } = node.form
                && let Some(symbol) = module.facts[pattern.0 as usize].symbol
            {
                initializers.insert(symbol, value);
            }
        }
        engine.external_nodes.push(nodes);
        engine.external_symbols.push(symbols);
        engine.initializers.push(initializers);
        let mut info = Vec::with_capacity(module.types.len());
        for id in 0..module.types.len() {
            let hint = engine.hint(index, TypeId(id as u32), 0);
            info.push(engine.leaf(hint));
            if engine.exhausted {
                return Err(engine.work);
            }
        }
        engine.type_info.push(info);
        for &id in &module.exports {
            let symbol = &module.symbols[id.0 as usize];
            let name = module.hir.name(symbol.name).to_owned();
            engine
                .exports
                .insert((module.canonical_path.clone(), name.clone()), (index, id));
        }
    }
    for (index, module) in modules.iter().enumerate() {
        if engine.exhausted {
            break;
        }
        engine.current_module = index;
        let mut state = State::new();
        engine.visit(index, NodeId(0), &mut state, 0);
        for function in 0..module.functions.len() {
            if engine.exhausted {
                break;
            }
            engine.function(index, FunctionId(function as u32));
        }
        if !engine.charge(state.bindings.len()) {
            break;
        }
        let bindings: Vec<_> = state
            .bindings
            .iter()
            .map(|(&symbol, value)| (symbol, value.clone()))
            .collect();
        for (symbol, value) in bindings {
            if engine.exhausted {
                break;
            }
            let info = engine.snapshot(&value, &mut state);
            engine.facts[index].symbols[symbol.0 as usize] = Some(info);
        }
    }
    for (index, module) in modules.iter().enumerate() {
        if engine.exhausted {
            break;
        }
        engine.current_module = index;
        if !engine.charge(module.hir.nodes.len() + module.symbols.len()) {
            break;
        }
        let mut exports =
            super::super::link::declarations(&module.hir, &module.canonical_path, &module.version);
        for export in &mut exports {
            if engine.exhausted {
                break;
            }
            if let Some(&(_, id)) = engine
                .exports
                .get(&(module.canonical_path.clone(), export.name.clone()))
            {
                let mut state = State::new();
                let value = engine.symbol(index, id, &mut state);
                export.source_type = Some(
                    engine.facts[index].symbols[id.0 as usize]
                        .clone()
                        .unwrap_or_else(|| engine.snapshot(&value, &mut state)),
                );
            }
        }
        engine.facts[index].interface = Some(ModuleInterface {
            canonical_path: module.canonical_path.clone(),
            version: module.version.clone(),
            exports,
        });
    }
    let work = engine.work;
    let exhausted = engine.exhausted;
    if exhausted {
        return Err(work);
    }
    for (facts, work) in engine.facts.iter_mut().zip(engine.module_work) {
        facts.work_units = work;
    }
    let facts = engine.facts;
    for (module, facts) in modules.iter_mut().zip(facts) {
        module.source_types = facts;
    }
    Ok(work)
}

impl Inference<'_> {
    fn charge(&mut self, work: usize) -> bool {
        self.work = self.work.saturating_add(work);
        if let Some(module) = self.module_work.get_mut(self.current_module) {
            *module = module.saturating_add(work);
        }
        self.exhausted |= self.work > self.limits.max_work;
        !self.exhausted
    }
    fn leaf(&self, ty: TypeHint) -> Arc<SourceType> {
        if ty == TypeHint::Any {
            self.unknown.clone()
        } else {
            Arc::new(SourceType::new(ty))
        }
    }
    fn hint(&mut self, m: usize, ty: TypeId, depth: usize) -> TypeHint {
        if depth >= self.limits.max_depth || !self.charge(1) {
            return TypeHint::Any;
        }
        let module = &self.modules[m];
        match &module.types[ty.0 as usize] {
            Type::Any => TypeHint::Any,
            Type::Null => TypeHint::Null,
            Type::Bool => TypeHint::Bool,
            Type::Int => TypeHint::Int,
            Type::Float => TypeHint::Float,
            Type::String => TypeHint::String,
            Type::Array => TypeHint::Array,
            Type::Table => TypeHint::Table,
            Type::Future => TypeHint::Future,
            Type::Generator => TypeHint::Generator,
            Type::Closure | Type::Function(_) | Type::BoundMethod(_) => TypeHint::Closure,
            Type::Named(name) => TypeHint::Named(module.hir.name(*name).into()),
            // Bound tuple/callable types remain available directly from HIR. Expanding
            // their shared type DAG into owned TypeHint trees would duplicate it.
            Type::Tuple(_) => TypeHint::Any,
            Type::Callable(..) => TypeHint::Closure,
            Type::Union(elements) => {
                if !self.charge(elements.len()) {
                    return TypeHint::Any;
                }
                let mut result = Vec::with_capacity(elements.len());
                for &ty in elements {
                    result.push(self.hint(m, ty, depth + 1));
                    if self.exhausted {
                        return TypeHint::Any;
                    }
                }
                TypeHint::Union(result)
            }
            Type::Class(class) | Type::Instance(class) => match module.hir.node(*class).form {
                Form::Class {
                    name: Some(name), ..
                } => TypeHint::Named(format!(
                    "{}::{}",
                    module.canonical_path,
                    module.hir.text(name.span)
                )),
                _ => TypeHint::Any,
            },
        }
    }
    fn base(&self, m: usize, n: NodeId) -> Arc<SourceType> {
        self.type_info[m][self.modules[m].facts[n.0 as usize].ty.0 as usize].clone()
    }
    fn freeze(
        &mut self,
        value: &Value,
        state: &mut State,
        path: &mut Vec<usize>,
        depth: usize,
    ) -> Arc<SourceType> {
        let Some(id) = value.object else {
            return value.info.clone();
        };
        if path.contains(&id) || depth >= self.limits.max_depth || !self.charge(1) {
            return self.leaf(value.info.ty.clone());
        }
        if let Some((revision, cached)) = &state.cache[id]
            && *revision == state.revision
        {
            return cached.clone();
        }
        let width = match &state.objects[id] {
            Object::Record(fields) => fields.len(),
            Object::Array { values, .. } | Object::Alternatives(values) => values.len(),
        };
        if !self.charge(width) {
            return self.unknown.clone();
        }
        path.push(id);
        let shape = match state.objects[id].clone() {
            Object::Record(fields) => {
                let mut members = Vec::with_capacity(fields.len());
                for field in fields {
                    let value = self.freeze(&field.value, state, path, depth + 1);
                    if self.exhausted {
                        return self.unknown.clone();
                    }
                    members.push(SourceMember {
                        name: field.name,
                        definition: field.definition,
                        value,
                    });
                }
                SourceShape::Record(members)
            }
            Object::Array { values, exact } => {
                let mut items = Vec::with_capacity(values.len());
                for value in values {
                    items.push(self.freeze(&value, state, path, depth + 1));
                    if self.exhausted {
                        return self.unknown.clone();
                    }
                }
                let mut element = None;
                for value in &items {
                    element = Some(match element {
                        Some(old) => self.join(old, value.clone()),
                        None => value.clone(),
                    });
                    if self.exhausted {
                        return self.unknown.clone();
                    }
                }
                let element = element.unwrap_or_else(|| self.unknown.clone());
                if exact
                    && (matches!(&value.info.ty, TypeHint::Tuple(_))
                        || matches!(&value.info.ty, TypeHint::Named(name) if name == "tuple"))
                {
                    SourceShape::Tuple(items.into())
                } else {
                    SourceShape::Array {
                        element,
                        values: exact.then(|| items.into()),
                    }
                }
            }
            Object::Alternatives(values) => {
                let mut result = None;
                for value in values {
                    let value = self.freeze(&value, state, path, depth + 1);
                    result = Some(match result {
                        Some(old) => self.join(old, value),
                        None => value,
                    });
                    if self.exhausted {
                        return self.unknown.clone();
                    }
                }
                path.pop();
                let result = result.unwrap_or_else(|| self.unknown.clone());
                state.cache[id] = Some((state.revision, result.clone()));
                return result;
            }
        };
        path.pop();
        let result = Arc::new(SourceType {
            shape: Some(shape),
            escaped: state.opaque.contains(&id),
            ..(*value.info).clone()
        });
        state.cache[id] = Some((state.revision, result.clone()));
        result
    }
    fn snapshot(&mut self, value: &Value, state: &mut State) -> Arc<SourceType> {
        self.freeze(value, state, &mut Vec::new(), 0)
    }
    fn materialize(&mut self, info: Arc<SourceType>, state: &mut State) -> Value {
        self.materialize_shared(info, state, &mut HashMap::new(), 0)
    }
    fn materialize_shared(
        &mut self,
        info: Arc<SourceType>,
        state: &mut State,
        memo: &mut HashMap<usize, Value>,
        depth: usize,
    ) -> Value {
        if !self.charge(1) || depth >= self.limits.max_depth {
            return Value::plain(self.unknown.clone());
        }
        let key = Arc::as_ptr(&info) as usize;
        if let Some(value) = memo.get(&key) {
            return value.clone();
        }
        let value = match &info.shape {
            Some(SourceShape::Record(fields)) => {
                if !self.charge(fields.len()) {
                    return Value::plain(self.unknown.clone());
                }
                let mut values = Vec::with_capacity(fields.len());
                for field in fields {
                    let value =
                        self.materialize_shared(field.value.clone(), state, memo, depth + 1);
                    if self.exhausted {
                        return Value::plain(self.unknown.clone());
                    }
                    values.push(Field {
                        name: field.name.clone(),
                        definition: field.definition.clone(),
                        value,
                    });
                }
                state.object(info.clone(), Object::Record(values))
            }
            Some(SourceShape::Array { element, values }) => {
                let exact = values.is_some();
                let values = values
                    .as_deref()
                    .unwrap_or_else(|| std::slice::from_ref(element));
                if !self.charge(values.len()) {
                    return Value::plain(self.unknown.clone());
                }
                let mut items = Vec::with_capacity(values.len());
                for value in values {
                    items.push(self.materialize_shared(value.clone(), state, memo, depth + 1));
                    if self.exhausted {
                        return Value::plain(self.unknown.clone());
                    }
                }
                state.object(
                    info.clone(),
                    Object::Array {
                        values: items,
                        exact,
                    },
                )
            }
            Some(SourceShape::Tuple(values)) => {
                if !self.charge(values.len()) {
                    return Value::plain(self.unknown.clone());
                }
                let mut items = Vec::with_capacity(values.len());
                for value in values.iter() {
                    items.push(self.materialize_shared(value.clone(), state, memo, depth + 1));
                    if self.exhausted {
                        return Value::plain(self.unknown.clone());
                    }
                }
                state.object(
                    info.clone(),
                    Object::Array {
                        values: items,
                        exact: true,
                    },
                )
            }
            Some(SourceShape::Generator { .. }) | None => Value::plain(info.clone()),
        };
        if let Some(id) = value.object {
            if info.escaped {
                state.opaque.insert(id);
            }
            memo.insert(key, value.clone());
        }
        value
    }
    fn join(&mut self, a: Arc<SourceType>, b: Arc<SourceType>) -> Arc<SourceType> {
        self.join_at(a, b, 0)
    }
    fn join_at(&mut self, a: Arc<SourceType>, b: Arc<SourceType>, depth: usize) -> Arc<SourceType> {
        if Arc::ptr_eq(&a, &b) {
            return a;
        }
        if depth >= self.limits.max_depth || !self.charge(1) {
            return self.unknown.clone();
        }
        let ty = if a.ty == b.ty {
            a.ty.clone()
        } else if a.ty == TypeHint::Any || b.ty == TypeHint::Any {
            TypeHint::Any
        } else {
            let mut types = Vec::new();
            for ty in [&a.ty, &b.ty] {
                for ty in match ty {
                    TypeHint::Union(types) => types.as_slice(),
                    ty => std::slice::from_ref(ty),
                } {
                    if !self.charge(types.len() + 1) {
                        return self.unknown.clone();
                    }
                    if !types.contains(ty) {
                        types.push(ty.clone());
                    }
                }
            }
            TypeHint::Union(types)
        };
        let shape = match (&a.shape, &b.shape) {
            (Some(SourceShape::Record(left)), Some(SourceShape::Record(right))) => {
                let mut fields = Vec::new();
                for field in left {
                    if !self.charge(right.len() + 1) {
                        return self.unknown.clone();
                    }
                    if let Some(other) = right.iter().find(|other| other.name == field.name) {
                        let value =
                            self.join_at(field.value.clone(), other.value.clone(), depth + 1);
                        if self.exhausted {
                            return self.unknown.clone();
                        }
                        fields.push(SourceMember {
                            value,
                            ..field.clone()
                        });
                    }
                }
                Some(SourceShape::Record(fields))
            }
            (
                Some(SourceShape::Array { element: a, .. }),
                Some(SourceShape::Array { element: b, .. }),
            ) => Some(SourceShape::Array {
                element: self.join_at(a.clone(), b.clone(), depth + 1),
                values: None,
            }),
            (Some(SourceShape::Tuple(a)), Some(SourceShape::Tuple(b))) if a.len() == b.len() => {
                if !self.charge(a.len()) {
                    return self.unknown.clone();
                }
                Some(SourceShape::Tuple(
                    a.iter()
                        .zip(b.iter())
                        .map(|(a, b)| self.join_at(a.clone(), b.clone(), depth + 1))
                        .collect(),
                ))
            }
            (
                Some(SourceShape::Generator {
                    element: a,
                    result: ar,
                }),
                Some(SourceShape::Generator {
                    element: b,
                    result: br,
                }),
            ) => Some(SourceShape::Generator {
                element: self.join_at(a.clone(), b.clone(), depth + 1),
                result: self.join_at(ar.clone(), br.clone(), depth + 1),
            }),
            _ => None,
        };
        let call_result = a
            .call_result
            .as_ref()
            .zip(b.call_result.as_ref())
            .map(|(a, b)| self.join_at(a.clone(), b.clone(), depth + 1));
        Arc::new(SourceType {
            ty,
            escaped: a.escaped || b.escaped,
            shape,
            call_result,
        })
    }
    fn function(&mut self, m: usize, id: FunctionId) -> Arc<SourceType> {
        if let Some(result) = &self.facts[m].functions[id.0 as usize] {
            return result.clone();
        }
        let f = &self.modules[m].functions[id.0 as usize];
        let body = f.body;
        let generator = f.kind == FunctionKind::Generator;
        let declared = self.type_info[m][f.return_type.0 as usize].clone();
        if self.active.len() + self.active_globals.len() >= self.limits.max_depth
            || !self.active.insert((m, id))
            || self.exhausted
        {
            return if generator {
                self.leaf(TypeHint::Generator)
            } else {
                declared
            };
        }
        let previous = std::mem::replace(&mut self.current_module, m);
        let mut state = State::new();
        let tail = self.visit(m, body, &mut state, 0);
        if state.live {
            let tail = self.snapshot(&tail, &mut state);
            state.returns.push(tail);
        }
        let mut inferred = None;
        for value in state.returns {
            inferred = Some(match inferred {
                Some(old) => self.join(old, value),
                None => value,
            });
            if self.exhausted {
                break;
            }
        }
        let inferred = inferred.unwrap_or_else(|| self.unknown.clone());
        let result = if declared.ty == TypeHint::Any || declared.ty == inferred.ty {
            inferred
        } else {
            declared
        };
        let result = if generator {
            let mut element = None;
            for value in state.yields {
                element = Some(match element {
                    Some(old) => self.join(old, value),
                    None => value,
                });
                if self.exhausted {
                    break;
                }
            }
            Arc::new(SourceType {
                ty: TypeHint::Generator,
                shape: Some(SourceShape::Generator {
                    element: element.unwrap_or_else(|| self.unknown.clone()),
                    result,
                }),
                ..SourceType::new(TypeHint::Generator)
            })
        } else {
            result
        };
        self.facts[m].functions[id.0 as usize] = Some(result.clone());
        self.active.remove(&(m, id));
        self.current_module = previous;
        result
    }
    fn callable(&mut self, m: usize, id: FunctionId) -> Arc<SourceType> {
        Arc::new(SourceType {
            ty: TypeHint::Closure,
            escaped: false,
            shape: None,
            call_result: Some(self.function(m, id)),
        })
    }
    fn external(&self, m: usize, n: NodeId) -> Option<Arc<ExternalSymbol>> {
        let symbol = self.modules[m].facts[n.0 as usize].symbol;
        let index = self.external_nodes[m]
            .get(&n)
            .copied()
            .or_else(|| symbol.and_then(|id| self.external_symbols[m].get(&id).copied()))?;
        Some(self.modules[m].external[index].descriptor.clone())
    }

    fn captured(&mut self, m: usize, function: FunctionId, state: &State) -> Option<Arc<[Value]>> {
        if !self.charge(
            self.modules[m].functions[function.0 as usize]
                .captures
                .len(),
        ) {
            return None;
        }
        let values: Vec<_> = self.modules[m].functions[function.0 as usize]
            .captures
            .iter()
            .filter_map(|capture| state.bindings.get(&capture.symbol))
            .filter(|value| value.object.is_some() || value.captures.is_some())
            .cloned()
            .collect();
        (!values.is_empty()).then(|| values.into())
    }
    fn symbol(&mut self, m: usize, id: SymbolId, state: &mut State) -> Value {
        if let Some(value) = state.bindings.get(&id) {
            return value.clone();
        }
        if let Some(info) = &self.facts[m].symbols[id.0 as usize] {
            let info = info.clone();
            if self.modules[m].symbols[id.0 as usize].origin == BindingOrigin::ModuleGlobal {
                let mut memo = std::mem::take(&mut state.imported_graph);
                let value = self.materialize_shared(info, state, &mut memo, 0);
                state.imported_graph = memo;
                if state.globals_dirty {
                    self.invalidated(&value, state);
                }
                state.bindings.insert(id, value.clone());
                return value;
            }
            return self.materialize(info, state);
        }
        let symbol = &self.modules[m].symbols[id.0 as usize];
        if let Type::Function(function) | Type::BoundMethod(function) =
            self.modules[m].types[symbol.ty.0 as usize]
        {
            return Value {
                info: self.callable(m, function),
                object: None,
                receiver: None,
                captures: self.captured(m, function, state),
            };
        }
        if symbol.origin == BindingOrigin::ModuleGlobal
            && self.active.len() + self.active_globals.len() < self.limits.max_depth
            && self.active_globals.insert((m, id))
        {
            let initializer = self.initializers[m].get(&id).copied();
            if let Some(initializer) = initializer {
                let mut origin = State::new();
                let value = self.visit(m, initializer, &mut origin, 0);
                let info = self.snapshot(&value, &mut origin);
                self.facts[m].symbols[id.0 as usize] = Some(info.clone());
                self.active_globals.remove(&(m, id));
                return self.materialize(info, state);
            }
            self.active_globals.remove(&(m, id));
        }
        Value::plain(self.type_info[m][symbol.ty.0 as usize].clone())
    }
    fn external_value(&mut self, symbol: &ExternalSymbol, state: &mut State) -> Value {
        if let Some(reference) = &symbol.reference
            && let Some(export) = &reference.export
            && let Some(&(module, id)) = self
                .exports
                .get(&(reference.module.clone(), export.clone()))
        {
            let key = (reference.module.clone(), export.clone());
            if let Some(value) = state.imports.get(&key) {
                return value.clone();
            }
            let info = if let Some(info) = &self.facts[module].symbols[id.0 as usize] {
                info.clone()
            } else {
                let mut origin = State::new();
                let value = self.symbol(module, id, &mut origin);
                self.snapshot(&value, &mut origin)
            };
            let mut memo = std::mem::take(&mut state.imported_graph);
            let value = self.materialize_shared(info, state, &mut memo, 0);
            state.imported_graph = memo;
            if state.globals_dirty {
                self.invalidated(&value, state);
            }
            state.imports.insert(key, value.clone());
            return value;
        }
        if let Some(info) = &symbol.source_type {
            if let Some(reference) = &symbol.reference
                && let Some(export) = &reference.export
            {
                let key = (reference.module.clone(), export.clone());
                if let Some(value) = state.imports.get(&key) {
                    return value.clone();
                }
                let mut memo = std::mem::take(&mut state.imported_graph);
                let value = self.materialize_shared(info.clone(), state, &mut memo, 0);
                state.imported_graph = memo;
                if state.globals_dirty {
                    self.invalidated(&value, state);
                }
                state.imports.insert(key, value.clone());
                return value;
            }
            return self.materialize(info.clone(), state);
        }
        Value::plain(Arc::new(SourceType {
            ty: symbol.ty.clone(),
            escaped: false,
            shape: None,
            call_result: symbol
                .signature
                .as_ref()
                .map(|signature| self.leaf(signature.result.clone())),
        }))
    }
    fn field_info(&mut self, info: &SourceType, key: &str) -> Option<SourceMember> {
        if let Some(SourceShape::Record(fields)) = &info.shape {
            if !self.charge(fields.len()) {
                return None;
            }
            return fields.iter().find(|field| field.name == key).cloned();
        }
        None
    }
    fn member_value(
        &mut self,
        value: &Value,
        key: &str,
        state: &mut State,
        depth: usize,
    ) -> Option<Value> {
        if depth >= self.limits.max_depth || !self.charge(1) {
            return None;
        }
        let id = value.object?;
        if let Object::Record(fields) = &state.objects[id] {
            if !self.charge(fields.len()) {
                return None;
            }
            return fields
                .iter()
                .find(|field| field.name == key)
                .map(|field| field.value.clone());
        }
        let Object::Alternatives(values) = &state.objects[id] else {
            return None;
        };
        if !self.charge(values.len()) {
            return None;
        }
        let values = values.clone();
        let mut candidates = Vec::new();
        for value in values {
            if let Some(value) = self.member_value(&value, key, state, depth + 1) {
                candidates.push(value);
            }
        }
        (!candidates.is_empty())
            .then(|| state.object(self.unknown.clone(), Object::Alternatives(candidates)))
    }
    fn subscript(
        &mut self,
        value: &Value,
        index: Option<usize>,
        state: &mut State,
    ) -> Option<Value> {
        let id = value.object?;
        let Object::Array { values, exact } = &state.objects[id] else {
            return None;
        };
        if *exact && let Some(index) = index {
            return values.get(index).cloned();
        }
        if !self.charge(values.len()) {
            return None;
        }
        let values = values.clone();
        Some(state.object(self.unknown.clone(), Object::Alternatives(values)))
    }
    fn array_index(&self, m: usize, node: NodeId) -> Option<usize> {
        let hir = &self.modules[m].hir;
        let Form::Literal { token } = hir.node(node).form else {
            return None;
        };
        (token.kind == K::Number)
            .then(|| hir.text(token.span).replace('_', "").parse().ok())
            .flatten()
    }
    fn invalidated(&mut self, value: &Value, state: &mut State) {
        if value.object.is_none() && value.captures.is_none() {
            return;
        }
        let mut pending = vec![value.clone()];
        let mut objects = HashSet::new();
        let mut closures = HashSet::new();
        let mut changed = false;
        while let Some(value) = pending.pop() {
            if !self.charge(1) {
                return;
            }
            if let Some(captures) = &value.captures
                && closures.insert(captures.as_ptr() as usize)
            {
                if !self.charge(captures.len()) {
                    return;
                }
                pending.extend(captures.iter().cloned());
            }
            let Some(id) = value.object else {
                continue;
            };
            if state.opaque.contains(&id) || !objects.insert(id) {
                continue;
            }
            state.opaque.insert(id);
            changed = true;
            match &mut state.objects[id] {
                Object::Record(fields) => {
                    if !self.charge(fields.len()) {
                        return;
                    }
                    for field in fields {
                        pending.push(field.value.clone());
                        field.value = Value::plain(self.unknown.clone());
                    }
                }
                Object::Array { values, .. } | Object::Alternatives(values) => {
                    if !self.charge(values.len()) {
                        return;
                    }
                    for value in values {
                        pending.push(value.clone());
                        *value = Value::plain(self.unknown.clone());
                    }
                }
            }
        }
        if changed {
            self.changed(state);
        }
    }
    fn key(&self, m: usize, node: NodeId) -> Option<String> {
        let hir = &self.modules[m].hir;
        match hir.node(node).form {
            _ if hir.node(node).kind == HirKind::String => {
                let text = hir.text(hir.node(node).span);
                let inner = text.strip_prefix('"')?.strip_suffix('"')?;
                (!inner.contains(['\\', '{'])).then(|| inner.to_owned())
            }
            _ => None,
        }
    }
    fn visit(&mut self, m: usize, n: NodeId, state: &mut State, depth: usize) -> Value {
        if self.visiting >= self.limits.max_depth {
            return Value::plain(self.unknown.clone());
        }
        self.visiting += 1;
        let result = self.visit_inner(m, n, state, depth);
        self.visiting -= 1;
        result
    }
    fn visit_inner(&mut self, m: usize, n: NodeId, state: &mut State, depth: usize) -> Value {
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
                } else if let Some(member) = super::super::builtin_catalog()
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
    fn has_break(&mut self, m: usize, loop_node: NodeId) -> bool {
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
    fn loop_changes(&mut self, before: &State, after: &State) -> (Vec<usize>, Vec<SymbolId>, bool) {
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
    fn widen_loop(
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
    fn insert_field(fields: &mut Vec<Field>, positions: &mut HashMap<String, usize>, field: Field) {
        if let Some(&index) = positions.get(&field.name) {
            fields[index] = field;
        } else {
            positions.insert(field.name.clone(), fields.len());
            fields.push(field);
        }
    }
    fn bind_pattern(&mut self, m: usize, n: NodeId, value: Value, state: &mut State) {
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
    fn location(
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
    fn compound(&self, operator: K, old: &SourceType, rhs: &SourceType) -> Arc<SourceType> {
        let ty = match (&old.ty, &rhs.ty) {
            (TypeHint::Int, TypeHint::Int) => TypeHint::Int,
            (TypeHint::Int | TypeHint::Float, TypeHint::Int | TypeHint::Float) => TypeHint::Float,
            (TypeHint::String, TypeHint::String) if operator == K::PlusEq => TypeHint::String,
            _ => TypeHint::Any,
        };
        self.leaf(ty)
    }
    fn write_field(
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
    fn changed(&mut self, state: &mut State) {
        // A changed child invalidates cached parents without scanning every
        // object on each write. Snapshots already published to queries stay valid.
        state.revision += 1;
    }
    fn clone_state(&mut self, state: &State) -> Option<State> {
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
    fn transfer(
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
    fn merge_value(
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
    fn merge_states(&mut self, into: &mut State, mut a: State, mut b: State) {
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
