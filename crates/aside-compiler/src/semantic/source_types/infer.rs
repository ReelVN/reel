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
    // Bound work and the combined inference stack when the host uses
    // unrestricted compiler limits.
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

mod callbacks;
mod expressions;
mod flow;

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
            Type::StringLiteral(text) => TypeHint::StringLiteral(text.clone()),
            Type::Array => TypeHint::Array,
            Type::Table => TypeHint::Table,
            Type::Future => TypeHint::Future,
            Type::Generator => TypeHint::Generator,
            Type::Closure | Type::Function(_) | Type::BoundMethod(_) => TypeHint::Closure,
            Type::Named(name) => TypeHint::Named(module.hir.name(*name).into()),
            Type::Record(fields) => {
                if !self.charge(fields.len()) {
                    return TypeHint::Any;
                }
                let mut result = Vec::with_capacity(fields.len());
                for &(name, ty) in fields {
                    result.push((module.hir.name(name).into(), self.hint(m, ty, depth + 1)));
                    if self.exhausted {
                        return TypeHint::Any;
                    }
                }
                TypeHint::Record(result)
            }
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
        let result = self.infer_function_body(m, body, generator, declared, &mut state);
        self.facts[m].functions[id.0 as usize] = Some(result.clone());
        self.active.remove(&(m, id));
        self.current_module = previous;
        result
    }
    fn infer_function_body(
        &mut self,
        m: usize,
        body: NodeId,
        generator: bool,
        declared: Arc<SourceType>,
        state: &mut State,
    ) -> Arc<SourceType> {
        let tail = self.visit(m, body, state, 0);
        if state.live {
            let tail = self.snapshot(&tail, state);
            state.returns.push(tail);
        }
        let mut inferred = None;
        for value in state.returns.drain(..) {
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
        if generator {
            let mut element = None;
            for value in state.yields.drain(..) {
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
        }
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
}
