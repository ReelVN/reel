use super::{Error, ErrorKind, ExternalTarget, Linkage, Metadata, Options, budget::Budget, ir::*, registers::Registers};
use crate::{SyntaxKind as K, TextRange, hir::{self as h, Form, HirKind as H}};
use aside_vm::bytecode as bc;

pub(super) fn lower<'a>(m: &'a h::AnalyzedModule, links: &'a Linkage<'a>, options: Options) -> Result<IrModule, Error> {
    let mut b = Budget::new(options.limits);
    b.span = m.hir().node(h::NodeId(0)).span;
    let mut package = if let Some(seed) = links.package {
        let stats = seed.stats();
        b.work(stats.records + stats.encoded_bytes as u64)?;
        // Clone admission happens before any clone allocation. Vec/String clones
        // retain at most their input capacities; this is a conservative seed charge.
        b.capacity(stats.owned_capacity_bytes)?;
        seed.package().clone()
    } else { bc::BytecodePackage::default() };
    package.content_version = options.content_version;
    let mut names = Vec::new();
    if let Some(seed) = links.package {
        b.work(seed.package().symbols.len() as u64)?;
        for (i, name) in seed.package().symbols.iter().enumerate() {
            b.push(&mut names, (name.as_str(), Some(bc::SymbolId(i as u32))))?;
        }
    }
    b.work(m.hir().names.len() as u64)?;
    for name in &m.hir().names { b.push(&mut names, (name.as_str(), None))?; }
    for name in [m.canonical_path(), "$module", "$closure", "$default"] {
        b.push(&mut names, (name, None))?;
    }
    sort_names(&mut names, &mut b)?;
    let mut index: Vec<(&str, bc::SymbolId)> = Vec::new();
    let names_capacity = names.capacity() * std::mem::size_of::<(&str, Option<bc::SymbolId>)>();
    for (name, seed) in names {
        if let Some((old, id)) = index.last_mut() {
            if b.compare(old, name)?.is_eq() {
                if let Some(seed) = seed { *id = seed; }
                continue;
            }
        }
        // Seed names sort before newly introduced duplicates by the heapsort tie rule.
        let id = match seed {
            Some(id) => id,
            None => {
                let id = bc::SymbolId(package.symbols.len() as u32);
                let text = b.string(name)?;
                b.push(&mut package.symbols, text)?;
                id
            }
        };
        b.push(&mut index, (name, id))?;
    }
    b.release_bytes(names_capacity);
    let symbol_count = m.symbols().len();
    let mut lower = Lowerer {
        m, links, options, b, package, names: index,
        type_cache: Vec::new(), contracts: Vec::new(),
        globals: Vec::new(), outputs: Vec::new(), defaults: Vec::new(),
        owned: Vec::new(), functions: Vec::new(), source: bc::SourceId(0), module: bc::ModuleId(0),
    };
    lower.type_cache = lower.b.filled(m.types().len(), None)?;
    lower.contracts = lower.b.filled(symbol_count, bc::TypeId(0))?;
    lower.globals = lower.b.filled(symbol_count, None)?;
    lower.owned = lower.b.filled(m.functions().len() + 1, Vec::new())?;
    let any = lower.builtin(bc::BuiltinType::Any)?;
    lower.contracts.fill(any);
    lower.b.work(symbol_count as u64)?;
    for (i, symbol) in m.symbols().iter().enumerate() {
        if !matches!(symbol.kind, h::SymbolKind::Host | h::SymbolKind::Import) {
            let owner = symbol.owner.map_or(m.functions().len(), |id| id.0 as usize);
            lower.b.push(&mut lower.owned[owner], h::SymbolId(i as u32))?;
        }
        if symbol.kind == h::SymbolKind::Function {
            lower.contracts[i] = lower.builtin(bc::BuiltinType::Closure)?;
        }
    }
    lower.b.work(m.hir().nodes.len() as u64)?;
    for (i, node) in m.hir().nodes.iter().enumerate() {
        if let Form::Binding { pattern, annotation: Some(_), .. } = node.form {
            if let Some(symbol) = m.facts()[pattern.0 as usize].symbol {
                lower.b.span = m.hir().node(h::NodeId(i as u32)).span;
                lower.contracts[symbol.0 as usize] = lower.ty(m.symbols()[symbol.0 as usize].ty, 0)?;
            }
        }
    }
    let path = lower.name(m.canonical_path())?;
    lower.source = bc::SourceId(lower.package.sources.len() as u32);
    let source = lower.b.string(&m.hir().source)?;
    lower.b.push(&mut lower.package.sources, bc::SourceFile { path, text: source })?;
    lower.module = bc::ModuleId(lower.package.modules.len() as u32);
    let mut module = bc::ModuleDef { path, imports: Vec::new(), exports: Vec::new(), globals: Vec::new(), initializer: None };
    for owned_index in 0..lower.owned[m.functions().len()].len() {
        let sym = lower.owned[m.functions().len()][owned_index];
        let s = &m.symbols()[sym.0 as usize];
        let id = bc::GlobalId(module.globals.len() as u32);
        lower.globals[sym.0 as usize] = Some(id);
        let name = lower.name(m.hir().name(s.name))?;
        lower.b.push(&mut module.globals, bc::GlobalDef { name, ty: lower.contracts[sym.0 as usize], mutable: s.mutable, linked_function: None })?;
    }
    lower.b.work(m.exports().len() as u64)?;
    for &sym in m.exports() {
        let global = lower.globals[sym.0 as usize]
            .ok_or_else(|| lower.b.error(ErrorKind::Unsupported, "reexports require full module linkage"))?;
        let name = lower.name(m.hir().name(m.symbols()[sym.0 as usize].name))?;
        lower.b.push(&mut module.exports, bc::Export { name, global })?;
    }
    lower.b.push(&mut lower.package.modules, module)?;
    lower.prepare_functions()?;
    lower.check_links()?;
    let module_output = lower.prepare_module()?;
    lower.package.modules[lower.module.index()].initializer = Some(module_output);
    lower.build_function(module_output, Origin::Module)?;
    for i in 0..lower.outputs.len() {
        let output = lower.outputs[i];
        lower.build_function(output, Origin::Function(h::FunctionId(i as u32)))?;
        for p in 0..m.functions()[i].parameters.len() {
            if let Some(default) = lower.defaults[i][p] {
                lower.build_function(default, Origin::Default { function: h::FunctionId(i as u32), parameter: p as u32 })?;
            }
        }
    }
    lower.b.release(lower.type_cache);
    lower.b.release(lower.contracts);
    lower.b.release(lower.globals);
    lower.b.release(lower.outputs);
    for values in lower.defaults.drain(..) { lower.b.release(values); }
    lower.b.release(lower.defaults);
    for values in lower.owned.drain(..) { lower.b.release(values); }
    lower.b.release(lower.owned);
    lower.b.release(lower.names);
    let canonical_path = lower.b.string(m.canonical_path())?;
    let source_version = lower.b.string(m.version())?;
    let mut imports = Vec::new();
    lower.b.work(m.imports().len() as u64)?;
    for import in m.imports() {
        let module = lower.b.string(&import.module)?;
        let version = lower.b.string(&import.version)?;
        let mut names = Vec::new();
        lower.b.work(import.names.len() as u64)?;
        lower.b.reserve(&mut names, import.names.len())?;
        names.extend_from_slice(&import.names);
        lower.b.push(&mut imports, h::Import { node: import.node, module, version, names })?;
    }
    let mut external_bindings = Vec::new();
    lower.b.work(m.external_bindings().len() as u64)?;
    lower.b.reserve(&mut external_bindings, m.external_bindings().len())?;
    external_bindings.extend_from_slice(m.external_bindings());
    let metadata = Metadata { canonical_path, source_version, imports, external_bindings };
    Ok(IrModule { package: lower.package, functions: lower.functions, source: lower.source, module: lower.module, stats: lower.b.stats, metadata, options, budget: lower.b })
}

/// Fallible heapsort: comparisons charge work before examining text. A trusted
/// seed id wins ties, so duplicate source names retain the seed ABI identity.
fn sort_names(values: &mut [(&str, Option<bc::SymbolId>)], b: &mut Budget) -> Result<(), Error> {
    fn less(a: (&str, Option<bc::SymbolId>), c: (&str, Option<bc::SymbolId>), b: &mut Budget) -> Result<bool, Error> {
        Ok(b.compare(a.0, c.0)?.then_with(|| a.1.is_none().cmp(&c.1.is_none())).is_lt())
    }
    fn sift(values: &mut [(&str, Option<bc::SymbolId>)], mut root: usize, end: usize, b: &mut Budget) -> Result<(), Error> {
        while root * 2 + 1 < end {
            let mut child = root * 2 + 1;
            if child + 1 < end && less(values[child], values[child+1], b)? { child += 1; }
            if !less(values[root], values[child], b)? { break; }
            values.swap(root, child);
            root = child;
        }
        Ok(())
    }
    for root in (0..values.len()/2).rev() { sift(values, root, values.len(), b)?; }
    for end in (1..values.len()).rev() { values.swap(0, end); sift(values, 0, end, b)?; }
    Ok(())
}

struct Lowerer<'a> {
    m: &'a h::AnalyzedModule,
    links: &'a Linkage<'a>,
    options: Options,
    b: Budget,
    package: bc::BytecodePackage,
    names: Vec<(&'a str, bc::SymbolId)>,
    type_cache: Vec<Option<bc::TypeId>>,
    contracts: Vec<bc::TypeId>,
    globals: Vec<Option<bc::GlobalId>>,
    outputs: Vec<bc::FunctionId>,
    defaults: Vec<Vec<Option<bc::FunctionId>>>,
    owned: Vec<Vec<h::SymbolId>>,
    functions: Vec<Function>,
    source: bc::SourceId,
    module: bc::ModuleId,
}
impl Lowerer<'_> {
    fn name(&mut self, name: &str) -> Result<bc::SymbolId, Error> {
        let (mut lo, mut hi) = (0, self.names.len());
        while lo < hi {
            let mid = (lo + hi) / 2;
            match self.b.compare(self.names[mid].0, name)? {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Ok(self.names[mid].1),
            }
        }
        Err(self.b.error(ErrorKind::InvalidIr, "name absent from admitted symbol table"))
    }
    fn builtin(&mut self, ty: bc::BuiltinType) -> Result<bc::TypeId, Error> {
        self.b.work(self.package.types.len() as u64 + 1)?;
        if let Some(i) = self.package.types.iter().position(|t| *t == bc::TypeDef::Builtin(ty)) { return Ok(bc::TypeId(i as u32)); }
        let id = bc::TypeId(self.package.types.len() as u32);
        self.b.push(&mut self.package.types, bc::TypeDef::Builtin(ty))?;
        Ok(id)
    }
    fn ty(&mut self, id: h::TypeId, depth: usize) -> Result<bc::TypeId, Error> {
        self.b.work(1)?;
        if depth >= self.options.limits.max_depth.min(128) { return Err(self.b.error(ErrorKind::Limit, "type lowering depth exceeded")); }
        if let Some(id) = self.type_cache[id.0 as usize] { return Ok(id); }
        use bc::BuiltinType as B;
        let builtin = match &self.m.types()[id.0 as usize] {
            h::Type::Any => Some(B::Any), h::Type::Null => Some(B::Null),
            h::Type::Bool => Some(B::Bool), h::Type::Int => Some(B::Int), h::Type::Float => Some(B::Float),
            h::Type::String => Some(B::String), h::Type::Array => Some(B::Array), h::Type::Table => Some(B::Table),
            h::Type::Future => Some(B::Future), h::Type::Generator => Some(B::Generator),
            h::Type::Closure | h::Type::Function(_) | h::Type::BoundMethod(_) => Some(B::Closure),
            _ => None,
        };
        let out = if let Some(ty) = builtin { self.builtin(ty)? } else {
            let def = match &self.m.types()[id.0 as usize] {
                h::Type::Tuple(ids) | h::Type::Union(ids) => {
                    self.b.work(ids.len() as u64)?;
                    let mut out = Vec::new();
                    for &id in ids { let id = self.ty(id, depth+1)?; self.b.push(&mut out, id)?; }
                    if matches!(self.m.types()[id.0 as usize], h::Type::Tuple(_)) { bc::TypeDef::Tuple(out) } else { bc::TypeDef::Union(out) }
                }
                h::Type::Callable(parameters, result) => {
                    // Structural callable contracts require parameter names in B01.
                    // HIR v1 stores only types, so fail instead of inventing names.
                    let _ = (parameters, result);
                    return Err(self.b.error(ErrorKind::Unsupported, "explicit structural callable annotations await a shared name-free type contract"));
                }
                _ => return Err(self.b.error(ErrorKind::Unsupported, "this explicit type needs full named/class module linkage")),
            };
            let out = bc::TypeId(self.package.types.len() as u32);
            self.b.push(&mut self.package.types, def)?;
            out
        };
        self.type_cache[id.0 as usize] = Some(out);
        Ok(out)
    }
    fn signature(&mut self, parameters: Vec<bc::Parameter>, return_type: bc::TypeId, context: bc::Context) -> Result<bc::SignatureId, Error> {
        let id = bc::SignatureId(self.package.signatures.len() as u32);
        self.b.push(&mut self.package.signatures, bc::Signature { parameters, rest: None, return_type,
            effects: if context == bc::Context::Ordinary { bc::Effects::ALL } else { bc::Effects(bc::Effects::ALL.0 & !bc::Effects::WAIT) }, property_bag_tail: None })?;
        Ok(id)
    }
    fn empty_function(&mut self, name: &str, signature: bc::SignatureId, context: bc::Context, captures: Vec<bc::CaptureKind>) -> Result<bc::FunctionId, Error> {
        let name = self.name(name)?;
        let id = bc::FunctionId(self.package.functions.len() as u32);
        self.b.push(&mut self.package.functions, bc::FunctionDef { module: self.module, name, kind: bc::FunctionKind::Plain,
            signature, context, captures, register_count: 0, bindings: Vec::new(), descriptors: Vec::new(), code: Vec::new(), regions: Vec::new(), source_map: Vec::new() })?;
        Ok(id)
    }
    fn captures(&mut self, f: &h::Function) -> Result<Vec<bc::CaptureKind>, Error> {
        self.b.work(f.captures.len() as u64)?;
        let mut out = Vec::new();
        for capture in &f.captures { self.b.push(&mut out, match capture.mode { h::CaptureMode::Value => bc::CaptureKind::Value, h::CaptureMode::Cell => bc::CaptureKind::Cell })?; }
        Ok(out)
    }
    fn prepare_functions(&mut self) -> Result<(), Error> {
        for f in self.m.functions() {
            self.b.span = self.m.hir().node(f.node).span;
            if f.kind != h::FunctionKind::Plain || f.receiver != h::Receiver::None || f.role != h::FunctionRole::Body {
                return Err(self.b.error(ErrorKind::Unsupported, "async/generator/class functions belong to later lowering"));
            }
            let mut parameters = Vec::new();
            let mut defaults = Vec::new();
            self.b.work(f.parameters.len() as u64)?;
            for p in &f.parameters {
                if p.rest { return Err(self.b.error(ErrorKind::Unsupported, "rest parameters belong to full callable lowering")); }
                let symbol = &self.m.symbols()[p.symbol.0 as usize];
                let ty = p.annotation.map(|id| self.ty(id, 0)).transpose()?.unwrap_or(self.builtin(bc::BuiltinType::Any)?);
                self.contracts[p.symbol.0 as usize] = ty;
                let name = self.name(self.m.hir().name(symbol.name))?;
                self.b.push(&mut parameters, bc::Parameter { name, ty, mutable: symbol.mutable, default: None })?;
                self.b.push(&mut defaults, None)?;
            }
            let result = if matches!(self.m.hir().node(f.node).form, Form::Function { result: Some(_), .. }) { self.ty(f.return_type, 0)? } else { self.builtin(bc::BuiltinType::Any)? };
            let signature = self.signature(parameters, result, bc::Context::Ordinary)?;
            let name = match self.m.hir().node(f.node).form { Form::Function { name: Some(token), .. } => self.m.hir().name(token.name.expect("function name")), _ => "$closure" };
            let captures = self.captures(f)?;
            let output = self.empty_function(name, signature, bc::Context::Ordinary, captures)?;
            self.b.push(&mut self.outputs, output)?;
            self.b.push(&mut self.defaults, defaults)?;
        }
        for (i, f) in self.m.functions().iter().enumerate() {
            for (p, parameter) in f.parameters.iter().enumerate() {
                let Some(node) = parameter.default else { continue; };
                self.b.span = self.m.hir().node(node).span;
                self.b.work(p as u64)?;
                let body_signature = self.package.functions[self.outputs[i].index()].signature;
                let mut prefix = Vec::new();
                self.b.reserve(&mut prefix, p)?;
                for param in &self.package.signatures[body_signature.index()].parameters[..p] {
                    let mut param = param.clone();
                    param.default = None;
                    prefix.push(param);
                }
                let result = self.package.signatures[body_signature.index()].parameters[p].ty;
                let signature = self.signature(prefix, result, bc::Context::NoWait)?;
                let captures = self.captures(f)?;
                let default = self.empty_function("$default", signature, bc::Context::NoWait, captures)?;
                self.defaults[i][p] = Some(default);
                self.package.signatures[body_signature.index()].parameters[p].default = Some(default);
            }
        }
        Ok(())
    }
    fn prepare_module(&mut self) -> Result<bc::FunctionId, Error> {
        let result = self.builtin(bc::BuiltinType::Any)?;
        let signature = self.signature(Vec::new(), result, bc::Context::Init)?;
        self.empty_function("$module", signature, bc::Context::Init, Vec::new())
    }
    fn check_links(&mut self) -> Result<(), Error> {
        self.b.work(self.links.natives.len() as u64)?;
        for link in self.links.natives {
            if self.links.package.is_none() || link.native.index() >= self.package.natives.len() {
                return Err(self.b.error(ErrorKind::MissingLink, "native link has no trusted implementation"));
            }
            self.b.work(self.m.external_bindings().len() as u64)?;
            let valid = match link.target {
                ExternalTarget::Symbol(id) => self.m.external_bindings().iter().any(|e| e.symbol == Some(id)),
                ExternalTarget::Node(id) => self.m.external_bindings().iter().any(|e| e.node == Some(id)),
            };
            if !valid { return Err(self.b.error(ErrorKind::LinkContract, "native link does not identify an analyzed external binding")); }
            let external = self.m.external_bindings().iter().find(|e| match link.target {
                ExternalTarget::Symbol(id) => e.symbol == Some(id), ExternalTarget::Node(id) => e.node == Some(id),
            }).expect("checked identity");
            if external.descriptor.intrinsic.is_some() { return Err(self.b.error(ErrorKind::LinkContract, "compiler intrinsic cannot be replaced with a native registration")); }
            if let Some(signature) = &external.descriptor.signature {
                let native = &self.package.natives[link.native.index()];
                let trusted = &self.package.signatures[native.signature.index()];
                self.b.work(signature.parameters.len() as u64 + trusted.parameters.len() as u64 + 1)?;
                if signature.parameters.len() != trusted.parameters.len() + usize::from(trusted.rest.is_some()) {
                    return Err(self.b.error(ErrorKind::LinkContract, "analysis/native parameter count differs"));
                }
                for (i, parameter) in signature.parameters.iter().enumerate() {
                    let actual = trusted.parameters.get(i).or(trusted.rest.as_ref()).expect("checked native parameter count");
                    if !self.b.compare(&parameter.name, &self.package.symbols[actual.name.index()])?.is_eq()
                        || parameter.mutable != actual.mutable || parameter.default != actual.default.is_some()
                        || parameter.rest != (i == trusted.parameters.len())
                        || !hint_matches(&parameter.ty, actual.ty, &self.package, &mut self.b, 0)? {
                        return Err(self.b.error(ErrorKind::LinkContract, "analysis/native parameter contract differs"));
                    }
                }
                if !hint_matches(&signature.result, trusted.return_type, &self.package, &mut self.b, 0)?
                    || signature.effect == crate::semantic::CallEffect::NoWait && trusted.effects.0 & bc::Effects::WAIT != 0 {
                    return Err(self.b.error(ErrorKind::LinkContract, "analysis/native return/effect contract differs"));
                }
            }
        }
        Ok(())
    }
    fn native(&mut self, node: h::NodeId) -> Result<Option<bc::NativeId>, Error> {
        self.b.work(self.links.natives.len() as u64 + 1)?;
        let symbol = self.m.facts()[node.0 as usize].symbol;
        let mut result = None;
        for link in self.links.natives {
            if link.target == ExternalTarget::Node(node) || symbol.is_some_and(|s| link.target == ExternalTarget::Symbol(s)) {
                if result.is_some() { return Err(self.b.error(ErrorKind::LinkContract, "ambiguous native binding")); }
                result = Some(link.native);
            }
        }
        Ok(result)
    }
    fn build_function(&mut self, output: bc::FunctionId, origin: Origin) -> Result<(), Error> {
        let node = match origin {
            Origin::Module => h::NodeId(0),
            Origin::Function(id) => self.m.functions()[id.0 as usize].body,
            Origin::Default { function, parameter } => self.m.functions()[function.0 as usize].parameters[parameter as usize].default.expect("default entry"),
        };
        self.b.span = self.m.hir().node(node).span;
        let placeholder = bc::FunctionDef { module: self.module, name: bc::SymbolId(0), kind: bc::FunctionKind::Plain, signature: bc::SignatureId(0), context: bc::Context::Ordinary,
            captures: Vec::new(), register_count: 0, bindings: Vec::new(), descriptors: Vec::new(), code: Vec::new(), regions: Vec::new(), source_map: Vec::new() };
        let def = std::mem::replace(&mut self.package.functions[output.index()], placeholder);
        let mut builder = FunctionBuilder::new(self, def, output, origin, node)?;
        let value = builder.expr(node, 0)?;
        if builder.current.is_some() {
            let value = match value { Some(value) => value, None => builder.constant(bc::Const::Null, node)? };
            builder.terminate(Terminator::Return(value), node)?;
            builder.release(value)?;
        }
        builder.finish()
    }
}

struct FunctionBuilder<'b, 'a> {
    l: &'b mut Lowerer<'a>,
    def: bc::FunctionDef,
    ir: Function,
    current: Option<BlockId>,
    registers: Registers,
    locals: Vec<(h::SymbolId, bc::BindingId)>,
    captures: Vec<(h::SymbolId, bc::CaptureId)>,
    loops: Vec<(h::NodeId, BlockId, BlockId, usize)>,
    protected_depth: usize,
}

impl<'b, 'a> FunctionBuilder<'b, 'a> {
    fn new(l: &'b mut Lowerer<'a>, mut def: bc::FunctionDef, output: bc::FunctionId, origin: Origin, node: h::NodeId) -> Result<Self, Error> {
        let (owner, prefix) = match origin {
            Origin::Module => (None, 0),
            Origin::Function(id) => (Some(id), l.m.functions()[id.0 as usize].parameters.len()),
            Origin::Default { function, parameter } => (Some(function), parameter as usize),
        };
        let mut locals = Vec::new();
        let mut captures = Vec::new();
        if let Some(owner) = owner {
            let f = &l.m.functions()[owner.0 as usize];
            l.b.work(prefix as u64 + f.captures.len() as u64)?;
            for (i, param) in f.parameters.iter().take(prefix).enumerate() {
                let symbol = &l.m.symbols()[param.symbol.0 as usize];
                let name = l.name(l.m.hir().name(symbol.name))?;
                let id = bc::BindingId(def.bindings.len() as u32);
                l.b.push(&mut def.bindings, bc::BindingDef { name, ty: l.contracts[param.symbol.0 as usize], mutable: symbol.mutable,
                    storage: if symbol.storage == h::Storage::SharedCell || symbol.mutable { bc::BindingStorage::Cell(bc::Reg(i as u32)) } else { bc::BindingStorage::Value(bc::Reg(i as u32)) },
                    initialization: bc::BindingInitialization::Parameter(i as u32) })?;
                l.b.push(&mut locals, (param.symbol, id))?;
            }
            let span = l.m.hir().node(node).span;
            l.b.work(l.owned[owner.0 as usize].len() as u64)?;
            for owned_index in 0..l.owned[owner.0 as usize].len() {
                let sym = l.owned[owner.0 as usize][owned_index];
                let symbol = &l.m.symbols()[sym.0 as usize];
                if symbol.initialization == h::Initialization::Parameter { continue; }
                if matches!(origin, Origin::Default { .. }) && !span.contains_range(symbol.declaration) { continue; }
                let name = l.name(l.m.hir().name(symbol.name))?;
                let id = bc::BindingId(def.bindings.len() as u32);
                let reg = bc::Reg(id.0);
                l.b.push(&mut def.bindings, bc::BindingDef { name, ty: l.contracts[sym.0 as usize], mutable: symbol.mutable,
                    storage: if symbol.storage == h::Storage::SharedCell { bc::BindingStorage::Cell(reg) } else { bc::BindingStorage::Value(reg) },
                    initialization: bc::BindingInitialization::Explicit })?;
                l.b.push(&mut locals, (sym, id))?;
            }
            for (i, capture) in f.captures.iter().enumerate() { l.b.push(&mut captures, (capture.symbol, bc::CaptureId(i as u32)))?; }
        }
        let sort_work = (locals.len() + captures.len()) as u64 * (usize::BITS as u64);
        l.b.work(sort_work)?;
        locals.sort_unstable_by_key(|(s, _)| *s);
        captures.sort_unstable_by_key(|(s, _)| *s);
        let registers = Registers::new(def.bindings.len() as u32, &l.b)?;
        let span = l.m.hir().node(node).span;
        let mut out = Self { l, def, ir: Function { output, origin, span, blocks: Vec::new(), layout: Vec::new(), regions: Vec::new() }, current: None,
            registers, locals, captures, loops: Vec::new(), protected_depth: 0 };
        let entry = out.block()?;
        out.start(entry)?;
        Ok(out)
    }
    fn span(&self, n: h::NodeId) -> TextRange { self.l.m.hir().node(n).span }
    fn block(&mut self) -> Result<BlockId, Error> {
        if self.l.b.stats.blocks >= self.l.b.limits.max_blocks { return Err(self.l.b.error(ErrorKind::Limit, "basic block budget exceeded")); }
        self.l.b.stats.blocks += 1;
        let id = BlockId(self.ir.blocks.len() as u32);
        self.l.b.push(&mut self.ir.blocks, Block { operations: Vec::new(), terminator: None })?;
        Ok(id)
    }
    fn start(&mut self, block: BlockId) -> Result<(), Error> {
        if self.current.is_some() { return Err(self.l.b.error(ErrorKind::InvalidIr, "start before terminating the prior block")); }
        self.l.b.push(&mut self.ir.layout, block)?;
        self.current = Some(block);
        Ok(())
    }
    fn instruction(&mut self) -> Result<(), Error> {
        if self.l.b.stats.instructions >= self.l.b.limits.max_instructions { return Err(self.l.b.error(ErrorKind::Limit, "instruction budget exceeded")); }
        self.l.b.stats.instructions += 1;
        Ok(())
    }
    fn op(&mut self, opcode: bc::Opcode, a: u32, b: u32, c: u32, n: h::NodeId) -> Result<(), Error> {
        self.l.b.span = self.span(n);
        self.instruction()?;
        let current = self.current.ok_or_else(|| self.l.b.error(ErrorKind::InvalidIr, "operation after a terminator"))?;
        let span = self.span(n);
        self.l.b.push(&mut self.ir.blocks[current.0 as usize].operations, Operation { instruction: bc::Instruction::new(opcode, a, b, c), span })
    }
    fn terminate(&mut self, term: Terminator, n: h::NodeId) -> Result<(), Error> {
        self.l.b.span = self.span(n);
        self.instruction()?;
        self.l.b.record()?;
        let current = self.current.take().ok_or_else(|| self.l.b.error(ErrorKind::InvalidIr, "duplicate terminator"))?;
        self.ir.blocks[current.0 as usize].terminator = Some((term, self.span(n)));
        Ok(())
    }
    fn temp(&mut self) -> Result<bc::Reg, Error> { self.registers.temp(&mut self.l.b) }
    fn release(&mut self, reg: bc::Reg) -> Result<(), Error> {
        if let Some(current) = self.current {
            self.instruction()?;
            let span = self.l.b.span;
            self.l.b.push(&mut self.ir.blocks[current.0 as usize].operations, Operation {
                instruction: bc::Instruction::new(bc::Opcode::Clear, reg.0, 0, 0), span,
            })?;
        }
        self.registers.drop_temp(reg, &mut self.l.b)
    }
    fn descriptor(&mut self, descriptor: bc::Descriptor) -> Result<bc::DescriptorId, Error> {
        let id = bc::DescriptorId(self.def.descriptors.len() as u32);
        self.l.b.push(&mut self.def.descriptors, descriptor)?;
        Ok(id)
    }
    fn constant(&mut self, constant: bc::Const, n: h::NodeId) -> Result<bc::Reg, Error> {
        let id = bc::ConstId(self.l.package.constants.len() as u32);
        self.l.b.push(&mut self.l.package.constants, constant)?;
        let out = self.temp()?;
        self.op(bc::Opcode::LoadConst, out.0, id.0, 0, n)?;
        Ok(out)
    }
    fn local(&mut self, symbol: h::SymbolId) -> Result<Option<bc::BindingId>, Error> {
        self.l.b.work(usize::BITS as u64)?;
        Ok(self.locals.binary_search_by_key(&symbol, |(s, _)| *s).ok().map(|i| self.locals[i].1))
    }
    fn capture(&mut self, symbol: h::SymbolId) -> Result<Option<bc::CaptureId>, Error> {
        self.l.b.work(usize::BITS as u64)?;
        Ok(self.captures.binary_search_by_key(&symbol, |(s, _)| *s).ok().map(|i| self.captures[i].1))
    }
    fn read(&mut self, symbol: h::SymbolId, n: h::NodeId) -> Result<bc::Reg, Error> {
        let out = self.temp()?;
        if let Some(id) = self.local(symbol)? { self.op(bc::Opcode::BindRead, out.0, id.0, 0, n)?; }
        else if let Some(global) = self.l.globals[symbol.0 as usize] {
            let desc = self.descriptor(bc::Descriptor::Global { module: self.l.module, global })?;
            self.op(bc::Opcode::GlobalRead, out.0, desc.0, 0, n)?;
        } else if let Some(capture) = self.capture(symbol)? {
            self.op(bc::Opcode::CaptureRead, out.0, capture.0, 0, n)?;
            if self.def.captures[capture.index()] == bc::CaptureKind::Cell { self.op(bc::Opcode::CellRead, out.0, out.0, 0, n)?; }
        } else {
            return Err(self.l.b.error(ErrorKind::MissingLink, "external value is not a linked script binding; native values await unified callable support"));
        }
        Ok(out)
    }
    fn write(&mut self, symbol: h::SymbolId, value: bc::Reg, initialize: bool, n: h::NodeId) -> Result<(), Error> {
        if let Some(binding) = self.local(symbol)? { self.op(if initialize { bc::Opcode::BindInit } else { bc::Opcode::BindWrite }, binding.0, value.0, 0, n)?; }
        else if let Some(global) = self.l.globals[symbol.0 as usize] {
            let desc = self.descriptor(bc::Descriptor::Global { module: self.l.module, global })?;
            self.op(if initialize { bc::Opcode::GlobalInit } else { bc::Opcode::GlobalWrite }, value.0, desc.0, 0, n)?;
        } else if let Some(capture) = self.capture(symbol)? {
            if initialize || self.def.captures[capture.index()] != bc::CaptureKind::Cell { return Err(self.l.b.error(ErrorKind::InvalidIr, "write requires a shared captured Cell")); }
            let cell = self.temp()?;
            self.op(bc::Opcode::CaptureRead, cell.0, capture.0, 0, n)?;
            self.op(bc::Opcode::CellWrite, cell.0, value.0, 0, n)?;
            self.release(cell)?;
        } else { return Err(self.l.b.error(ErrorKind::MissingLink, "assignment needs a linked lexical binding")); }
        Ok(())
    }
    fn closure(&mut self, function: h::FunctionId, n: h::NodeId) -> Result<bc::Reg, Error> {
        let mut captures = Vec::new();
        let f = &self.l.m.functions()[function.0 as usize];
        self.l.b.work(f.captures.len() as u64)?;
        for capture in &f.captures {
            let source = if let Some(binding) = self.local(capture.symbol)? { bc::CaptureSource::Binding(binding) }
            else if let Some(capture) = self.capture(capture.symbol)? { bc::CaptureSource::Capture(capture) }
            else { return Err(self.l.b.error(ErrorKind::InvalidIr, "missing defining capture identity")); };
            self.l.b.push(&mut captures, source)?;
        }
        let descriptor = self.descriptor(bc::Descriptor::Closure { function: self.l.outputs[function.0 as usize], captures })?;
        let out = self.temp()?;
        self.op(bc::Opcode::MakeClosure, out.0, descriptor.0, 0, n)?;
        Ok(out)
    }
    fn enter_scope(&mut self, n: h::NodeId) -> Result<(), Error> {
        if let Some(scope) = self.l.m.facts()[n.0 as usize].scope {
            let symbols = &self.l.m.scopes()[scope.0 as usize].symbols;
            self.l.b.work(symbols.len() as u64)?;
            for &sym in symbols {
                if let Some(binding) = self.local(sym)? {
                    let binding_def = &self.def.bindings[binding.index()];
                    if binding_def.initialization == bc::BindingInitialization::Explicit {
                        match binding_def.storage {
                            bc::BindingStorage::Cell(_) => self.op(bc::Opcode::CellNew, binding.0, 0, 0, n)?,
                            bc::BindingStorage::Value(reg) => self.op(bc::Opcode::Clear, reg.0, 0, 0, n)?,
                        }
                    }
                }
            }
        }
        let children = self.l.m.hir().children(n);
        self.l.b.work(self.l.m.hir().elements(n).len() as u64)?;
        for child in children {
            let child = match self.l.m.hir().node(child).form { Form::Modified { declaration, .. } => declaration, _ => child };
            if self.l.m.hir().node(child).kind == H::Function {
                let fact = &self.l.m.facts()[child.0 as usize];
                let function = fact.function.ok_or_else(|| self.l.b.error(ErrorKind::InvalidIr, "function declaration lacks analysis identity"))?;
                let symbol = fact.symbol.ok_or_else(|| self.l.b.error(ErrorKind::InvalidIr, "function declaration lacks binding"))?;
                let value = self.closure(function, child)?;
                self.write(symbol, value, true, child)?;
                self.release(value)?;
            }
        }
        Ok(())
    }
    fn value(&mut self, n: h::NodeId, depth: usize) -> Result<bc::Reg, Error> {
        self.expr(n, depth)?.ok_or_else(|| self.l.b.error(ErrorKind::Unsupported, "value expression terminates control flow"))
    }
    fn expr(&mut self, n: h::NodeId, depth: usize) -> Result<Option<bc::Reg>, Error> {
        self.l.b.span = self.span(n);
        self.l.b.work(1)?;
        if depth >= self.l.b.limits.max_depth.min(128) { return Err(self.l.b.error(ErrorKind::Limit, "source lowering depth exceeded")); }
        let kind = self.l.m.hir().node(n).kind;
        let form = self.l.m.hir().node(n).form;
        let depth = depth + 1;
        match (kind, form) {
            (H::Module | H::Block, _) => {
                self.enter_scope(n)?;
                let tail = match form { Form::Block { tail } => tail, _ => None };
                let mut result = None;
                self.l.b.work(self.l.m.hir().elements(n).len() as u64)?;
                for child in self.l.m.hir().children(n) {
                    if self.current.is_none() { break; }
                    if let Some(old) = result.take() { self.release(old)?; }
                    let value = self.expr(child, depth)?;
                    if tail == Some(child) { result = value; }
                    else if let Some(value) = value { self.release(value)?; }
                }
                if self.current.is_some() && result.is_none() && kind == H::Block { result = Some(self.constant(bc::Const::Null, n)?); }
                Ok(result)
            }
            (H::ExpressionStatement, Form::Statement { expression, terminated }) => {
                let value = self.expr(expression, depth)?;
                if terminated { if let Some(value) = value { self.release(value)?; } Ok(None) }
                else { Ok(value) }
            }
            (H::Group, Form::Single { child }) | (H::ModifiedDeclaration, Form::Modified { declaration: child, .. }) => self.expr(child, depth),
            (H::Let, Form::Binding { pattern, initializer, .. }) => {
                let symbol = self.pattern_symbol(pattern)?;
                if let Some(initializer) = initializer {
                    let value = self.value(initializer, depth)?;
                    if let Some(symbol) = symbol { self.write(symbol, value, true, n)?; }
                    self.release(value)?;
                }
                Ok(None)
            }
            (H::Function, _) | (H::Import, _) => Ok(None),
            (H::Name, _) => {
                let symbol = self.l.m.facts()[n.0 as usize].symbol.ok_or_else(|| self.l.b.error(ErrorKind::MissingLink, "name lacks analyzed binding"))?;
                Ok(Some(self.read(symbol, n)?))
            }
            (H::Closure, _) => {
                let function = self.l.m.facts()[n.0 as usize].function.ok_or_else(|| self.l.b.error(ErrorKind::InvalidIr, "closure lacks analyzed identity"))?;
                Ok(Some(self.closure(function, n)?))
            }
            (H::Literal, Form::Literal { token }) => {
                let constant = match token.kind {
                    K::Null => bc::Const::Null, K::True => bc::Const::Bool(true), K::False => bc::Const::Bool(false),
                    K::Number => self.number(self.l.m.hir().text(token.span), false)?,
                    _ => return Err(self.l.b.error(ErrorKind::Unsupported, "unsupported scalar literal")),
                };
                Ok(Some(self.constant(constant, n)?))
            }
            (H::String, _) => {
                let text = self.string(n)?;
                Ok(Some(self.constant(bc::Const::Utf8(text), n)?))
            }
            (H::Prefix, Form::Unary { operator, value }) => {
                // Treat the negative int64 minimum as a single source constant.
                if operator.kind == K::Minus {
                    if let Form::Literal { token } = self.l.m.hir().node(value).form {
                        if token.kind == K::Number {
                            let constant = self.number(self.l.m.hir().text(token.span), true)?;
                            return Ok(Some(self.constant(constant, n)?));
                        }
                    }
                }
                let opcode = match operator.kind { K::Minus => bc::Opcode::Neg, K::Plus => bc::Opcode::Pos, K::Bang => bc::Opcode::Not, K::Tilde => bc::Opcode::BitNot,
                    _ => return Err(self.l.b.error(ErrorKind::Unsupported, "await/yield require later task lowering")) };
                let input = self.value(value, depth)?;
                let out = self.temp()?;
                self.op(opcode, out.0, input.0, 0, n)?;
                self.release(input)?;
                Ok(Some(out))
            }
            (H::Binary, Form::Binary { left, operator, right }) => {
                if matches!(operator.kind, K::AndAnd | K::OrOr | K::QuestionQuestion) {
                    return self.short_circuit(n, left, operator.kind, right, depth);
                }
                let opcode = binary(operator.kind).ok_or_else(|| self.l.b.error(ErrorKind::Unsupported, "binary operator awaits later lowering"))?;
                let left = self.value(left, depth)?;
                let right = self.value(right, depth)?;
                let out = self.temp()?;
                self.op(opcode, out.0, left.0, right.0, n)?;
                self.release(left)?;
                self.release(right)?;
                Ok(Some(out))
            }
            (H::Assignment, _) => { self.assignment(n, depth)?; Ok(None) }
            (H::If, Form::Condition { pattern: None, condition, body, otherwise }) => self.condition(n, condition, body, otherwise, depth),
            (H::Call, Form::Call { callee, arguments, children: None, optional: false, chain: false }) => {
                let native = self.l.native(callee)?;
                let callee_value = if native.is_none() { Some(self.value(callee, depth)?) } else { None };
                self.l.b.work(arguments.map_or(0, |args| self.l.m.hir().elements(args).len()) as u64)?;
                let count = arguments.map_or(0, |args| self.l.m.hir().children(args).count());
                let window = self.registers.get(u32::try_from(count).map_err(|_| self.l.b.error(ErrorKind::Limit, "call window length overflow"))?, &mut self.l.b)?;
                let mut names = Vec::new();
                if let Some(arguments) = arguments {
                    for (i, arg) in self.l.m.hir().children(arguments).enumerate() {
                        let Form::Argument { name, value } = self.l.m.hir().node(arg).form else { return Err(self.l.b.error(ErrorKind::Unsupported, "spread requires ordered argument builder lowering")); };
                        let value = self.value(value, depth)?;
                        self.op(bc::Opcode::Move, window.start.0 + i as u32, value.0, 0, arg)?;
                        self.release(value)?;
                        if let Some(name) = name {
                            let name = self.l.name(self.l.m.hir().name(name.name.expect("argument name")))?;
                            self.l.b.push(&mut names, bc::NamedArg { name, offset: i as u32 })?;
                        }
                    }
                }
                let target = match (native, callee_value) { (Some(native), _) => bc::CallTarget::Native(native), (_, Some(callee)) => bc::CallTarget::Register(callee), _ => unreachable!() };
                let descriptor = self.descriptor(bc::Descriptor::Call(bc::CallDesc { target, arguments: bc::Arguments::Window(window), names }))?;
                let out = self.temp()?;
                self.op(bc::Opcode::Call, out.0, descriptor.0, 0, n)?;
                if let Some(callee) = callee_value { self.release(callee)?; }
                self.l.b.work(window.len as u64)?;
                for i in 0..window.len { self.op(bc::Opcode::Clear, window.start.0 + i, 0, 0, n)?; }
                self.registers.release(window, &mut self.l.b)?;
                Ok(Some(out))
            }
            (H::Return, Form::Transfer { value, .. }) => {
                let value = match value { Some(value) => self.value(value, depth)?, None => self.constant(bc::Const::Null, n)? };
                self.terminate(Terminator::Return(value), n)?;
                self.release(value)?;
                Ok(None)
            }
            (H::Throw, Form::Transfer { value, .. }) => {
                if let Some(value) = value { let value = self.value(value, depth)?; self.terminate(Terminator::Throw(value), n)?; self.release(value)?; }
                else { self.terminate(Terminator::Rethrow, n)?; }
                Ok(None)
            }
            (H::Try, Form::Try { body, catch, finally }) => { self.try_node(n, body, catch, finally, depth)?; Ok(None) }
            (H::While, Form::Condition { pattern: None, condition, body, otherwise: None }) => { self.loop_node(n, Some(condition), body, depth)?; Ok(None) }
            (H::Loop, Form::Single { child }) => { self.loop_node(n, None, child, depth)?; Ok(None) }
            (H::Label, Form::Label { body, .. }) => self.expr(body, depth),
            (H::Break | H::Continue, _) => {
                let target = self.l.m.facts()[n.0 as usize].control.as_ref().and_then(|c| c.target)
                    .ok_or_else(|| self.l.b.error(ErrorKind::InvalidIr, "loop transfer lacks analyzed target"))?;
                self.l.b.work(self.loops.len() as u64)?;
                let (_, next, end, loop_depth) = self.loops.iter().rev().find(|(node, _, _, _)| *node == target).copied()
                    .ok_or_else(|| self.l.b.error(ErrorKind::InvalidIr, "analyzed loop target not active"))?;
                let target = if kind == H::Break { end } else { next };
                self.terminate(if self.protected_depth > loop_depth { Terminator::Leave(target) } else { Terminator::Jump(target) }, n)?;
                Ok(None)
            }
            _ => Err(self.l.b.error(ErrorKind::Unsupported, format!("{kind:?} is outside L04 lowering support"))),
        }
    }
    fn pattern_symbol(&mut self, n: h::NodeId) -> Result<Option<h::SymbolId>, Error> {
        self.l.b.span = self.span(n);
        self.l.b.work(1)?;
        match self.l.m.hir().node(n).form {
            Form::Name { .. } => Ok(self.l.m.facts()[n.0 as usize].symbol),
            Form::Wildcard => Ok(None),
            Form::Single { child } if self.l.m.hir().node(n).kind == H::GroupPattern => self.pattern_symbol(child),
            _ => Err(self.l.b.error(ErrorKind::Unsupported, "destructuring/pattern lowering belongs to later language work")),
        }
    }
    fn short_circuit(&mut self, n: h::NodeId, left: h::NodeId, operator: K, right: h::NodeId, depth: usize) -> Result<Option<bc::Reg>, Error> {
        let left = self.value(left, depth)?;
        let out = self.temp()?;
        self.op(bc::Opcode::Move, out.0, left.0, 0, n)?;
        let rhs = self.block()?;
        let join = self.block()?;
        let term = match operator {
            K::AndAnd => Terminator::BranchBool { value: left, yes: rhs, no: join },
            K::OrOr => Terminator::BranchBool { value: left, yes: join, no: rhs },
            _ => Terminator::BranchNull { value: left, null: rhs, value_block: join },
        };
        self.terminate(term, n)?;
        self.release(left)?;
        self.start(rhs)?;
        let right = self.value(right, depth)?;
        self.op(bc::Opcode::Move, out.0, right.0, 0, n)?;
        self.terminate(if operator == K::QuestionQuestion { Terminator::Jump(join) } else { Terminator::BranchBool { value: right, yes: join, no: join } }, n)?;
        self.release(right)?;
        self.start(join)?;
        Ok(Some(out))
    }
    fn condition(&mut self, n: h::NodeId, condition: h::NodeId, yes: h::NodeId, no: Option<h::NodeId>, depth: usize) -> Result<Option<bc::Reg>, Error> {
        let condition = self.value(condition, depth)?;
        let out = self.temp()?;
        let yes_block = self.block()?;
        let no_block = self.block()?;
        let join = self.block()?;
        self.terminate(Terminator::BranchBool { value: condition, yes: yes_block, no: no_block }, n)?;
        self.release(condition)?;
        let mut falls = false;
        for (block, branch) in [(yes_block, Some(yes)), (no_block, no)] {
            self.start(block)?;
            let value = match branch { Some(branch) => self.expr(branch, depth)?, None => None };
            if self.current.is_some() {
                let value = match value { Some(value) => value, None => self.constant(bc::Const::Null, n)? };
                self.op(bc::Opcode::Move, out.0, value.0, 0, n)?;
                self.release(value)?;
                self.terminate(Terminator::Jump(join), n)?;
                falls = true;
            }
        }
        if falls { self.start(join)?; Ok(Some(out)) }
        else { self.release(out)?; Ok(None) }
    }
    fn assignment(&mut self, n: h::NodeId, depth: usize) -> Result<(), Error> {
        let mut targets = Vec::new();
        let mut current = n;
        let (rhs, compound) = loop {
            self.l.b.work(1)?;
            let Form::Binary { left, operator, right } = self.l.m.hir().node(current).form else { return Err(self.l.b.error(ErrorKind::InvalidIr, "assignment form")); };
            let mut target = left;
            while let Form::Single { child } = self.l.m.hir().node(target).form { target = child; self.l.b.work(1)?; }
            if self.l.m.hir().node(target).kind != H::Name { return Err(self.l.b.error(ErrorKind::Unsupported, "member/index/destructuring assignment awaits position lowering")); }
            let symbol = self.l.m.facts()[target.0 as usize].symbol.ok_or_else(|| self.l.b.error(ErrorKind::InvalidIr, "assignment target identity"))?;
            self.l.b.push(&mut targets, (symbol, target))?;
            if operator.kind == K::Eq && self.l.m.hir().node(right).kind == H::Assignment { current = right; continue; }
            break (right, if operator.kind == K::Eq { None } else { Some(operator.kind) });
        };
        let value = if let Some(operator) = compound {
            let opcode = binary(operator).ok_or_else(|| self.l.b.error(ErrorKind::Unsupported, "compound operator"))?;
            let left = self.read(targets.last().expect("assignment target").0, n)?;
            let right = self.value(rhs, depth)?;
            let out = self.temp()?;
            self.op(opcode, out.0, left.0, right.0, n)?;
            self.release(left)?; self.release(right)?;
            out
        } else { self.value(rhs, depth)? };
        for &(symbol, target) in targets.iter().rev() { self.write(symbol, value, false, target)?; }
        self.release(value)?;
        self.l.b.release(targets);
        Ok(())
    }
    fn loop_node(&mut self, n: h::NodeId, condition: Option<h::NodeId>, body: h::NodeId, depth: usize) -> Result<(), Error> {
        let head = self.block()?;
        let body_block = self.block()?;
        let end = self.block()?;
        self.terminate(Terminator::Jump(head), n)?;
        self.start(head)?;
        if let Some(condition) = condition {
            let value = self.value(condition, depth)?;
            self.terminate(Terminator::BranchBool { value, yes: body_block, no: end }, n)?;
            self.release(value)?;
        } else { self.terminate(Terminator::Jump(body_block), n)?; }
        self.l.b.push(&mut self.loops, (n, head, end, self.protected_depth))?;
        self.start(body_block)?;
        if let Some(value) = self.expr(body, depth)? { self.release(value)?; }
        if self.current.is_some() { self.terminate(Terminator::Jump(head), n)?; }
        self.loops.pop();
        self.start(end)?;
        Ok(())
    }
    fn try_node(&mut self, n: h::NodeId, body: h::NodeId, catch: Option<h::NodeId>, finally: Option<h::NodeId>, depth: usize) -> Result<(), Error> {
        let region = bc::RegionId(self.ir.regions.len() as u32);
        self.l.b.push(&mut self.ir.regions, Region { protected: 0..0, catch: None, finally: None })?;
        let body_block = self.block()?;
        let after = self.block()?;
        self.terminate(Terminator::Jump(body_block), n)?;
        let begin = self.ir.layout.len() as u32;
        self.start(body_block)?;
        self.protected_depth += 1;
        let mut falls = false;
        if let Some(value) = self.expr(body, depth)? { self.release(value)?; }
        if self.current.is_some() { self.terminate(Terminator::Leave(after), n)?; falls = true; }
        self.protected_depth -= 1;
        self.ir.regions[region.index()].protected = begin..self.ir.layout.len() as u32;
        if let Some(catch) = catch {
            let Form::Catch { body, .. } = self.l.m.hir().node(catch).form else { return Err(self.l.b.error(ErrorKind::InvalidIr, "catch form")); };
            let sym = self.l.m.facts()[catch.0 as usize].symbol.ok_or_else(|| self.l.b.error(ErrorKind::InvalidIr, "catch binding identity"))?;
            let exception = self.temp()?;
            let block = self.block()?;
            let begin = self.ir.layout.len() as u32;
            self.start(block)?;
            if let Some(binding) = self.local(sym)? {
                if matches!(self.def.bindings[binding.index()].storage, bc::BindingStorage::Cell(_)) { self.op(bc::Opcode::CellNew, binding.0, 0, 0, catch)?; }
                else { self.op(bc::Opcode::Clear, self.def.bindings[binding.index()].storage.reg().0, 0, 0, catch)?; }
            }
            self.write(sym, exception, true, catch)?;
            self.release(exception)?;
            self.protected_depth += 1;
            if let Some(value) = self.expr(body, depth)? { self.release(value)?; }
            if self.current.is_some() { self.terminate(Terminator::Leave(after), catch)?; falls = true; }
            self.protected_depth -= 1;
            self.ir.regions[region.index()].catch = Some((begin..self.ir.layout.len() as u32, exception));
        }
        if let Some(finally) = finally {
            let Form::Single { child } = self.l.m.hir().node(finally).form else { return Err(self.l.b.error(ErrorKind::InvalidIr, "finally form")); };
            let block = self.block()?;
            let begin = self.ir.layout.len() as u32;
            self.start(block)?;
            if let Some(value) = self.expr(child, depth)? { self.release(value)?; }
            if self.current.is_none() { let end = self.block()?; self.start(end)?; }
            self.terminate(Terminator::EndFinally(region), finally)?;
            self.ir.regions[region.index()].finally = Some(begin..self.ir.layout.len() as u32);
        }
        if falls { self.start(after)?; }
        Ok(())
    }
    fn number(&mut self, text: &str, negative: bool) -> Result<bc::Const, Error> {
        let mut bytes = Vec::new();
        self.l.b.work(text.len() as u64)?;
        self.l.b.reserve(&mut bytes, text.len())?;
        bytes.extend(text.bytes().filter(|&c| c != b'_'));
        let text = std::str::from_utf8(&bytes).expect("numeric source UTF-8");
        let prefix = text.get(..2).unwrap_or("");
        let (radix, digits) = match prefix { "0x" | "0X" => (16, &text[2..]), "0o" | "0O" => (8, &text[2..]), "0b" | "0B" => (2, &text[2..]), _ => (10, text) };
        let value = if radix == 10 && text.bytes().any(|c| matches!(c, b'.' | b'e' | b'E')) {
            let float: f64 = text.parse().map_err(|_| self.l.b.error(ErrorKind::InvalidIr, "analyzed float literal"))?;
            bc::Const::FloatBits(if negative { -float } else { float }.to_bits())
        } else {
            let value = i128::from_str_radix(digits, radix).map_err(|_| self.l.b.error(ErrorKind::InvalidIr, "analyzed integer literal"))?;
            let value = if negative { -value } else { value };
            bc::Const::Int(i64::try_from(value).map_err(|_| self.l.b.error(ErrorKind::InvalidIr, "integer literal exceeds int64"))?)
        };
        self.l.b.release(bytes);
        Ok(value)
    }
    fn string(&mut self, n: h::NodeId) -> Result<String, Error> {
        if self.l.m.hir().children(n).next().is_some() { return Err(self.l.b.error(ErrorKind::Unsupported, "interpolation requires standard string conversion linkage")); }
        let mut bytes = Vec::new();
        self.l.b.reserve(&mut bytes, usize::from(self.span(n).len()))?;
        let raw = self.l.m.hir().tokens(n).next().is_some_and(|t| self.l.m.hir().text(t.span).starts_with('r'));
        let format = self.l.m.hir().tokens(n).next().is_some_and(|t| self.l.m.hir().text(t.span).starts_with('f'));
        for token in self.l.m.hir().tokens(n).filter(|t| t.kind == K::StringText) {
            let text = self.l.m.hir().text(token.span);
            self.l.b.work(text.len() as u64)?;
            if raw { bytes.extend_from_slice(text.as_bytes()); continue; }
            let mut chars = text.chars();
            while let Some(ch) = chars.next() {
                let ch = if ch == '\\' {
                    match chars.next() {
                        Some('n') => '\n', Some('r') => '\r', Some('t') => '\t', Some('0') => '\0', Some('"') => '"', Some('\\') => '\\',
                        Some('u') => {
                            if chars.next() != Some('{') { return Err(self.l.b.error(ErrorKind::InvalidIr, "analyzed unicode escape")); }
                            let mut value = 0u32;
                            loop { match chars.next() {
                                Some('}') => break,
                                Some(c) => { value = value.checked_mul(16).and_then(|v| c.to_digit(16).and_then(|c| v.checked_add(c))).ok_or_else(|| self.l.b.error(ErrorKind::InvalidIr, "unicode escape overflow"))?; },
                                None => return Err(self.l.b.error(ErrorKind::InvalidIr, "unterminated analyzed escape")),
                            } }
                            char::from_u32(value).ok_or_else(|| self.l.b.error(ErrorKind::InvalidIr, "analyzed unicode scalar"))?
                        }
                        _ => return Err(self.l.b.error(ErrorKind::InvalidIr, "analyzed string escape")),
                    }
                } else if format && matches!(ch, '{' | '}') { chars.next(); ch } else { ch };
                bytes.extend_from_slice(ch.encode_utf8(&mut [0;4]).as_bytes());
            }
        }
        Ok(String::from_utf8(bytes).expect("decoded unicode"))
    }
    fn finish(mut self) -> Result<(), Error> {
        self.def.register_count = self.registers.high;
        self.l.b.stats.registers += self.registers.high as u64;
        self.l.b.stats.max_function_registers = self.l.b.stats.max_function_registers.max(self.registers.high);
        self.registers.finish(&mut self.l.b);
        self.l.b.release(self.locals);
        self.l.b.release(self.captures);
        self.l.b.release(self.loops);
        self.l.package.functions[self.ir.output.index()] = self.def;
        self.l.b.push(&mut self.l.functions, self.ir)
    }
}

fn binary(operator: K) -> Option<bc::Opcode> {
    Some(match operator {
        K::Plus | K::PlusEq => bc::Opcode::Add, K::Minus | K::MinusEq => bc::Opcode::Sub,
        K::Star | K::StarEq => bc::Opcode::Mul, K::Slash | K::SlashEq => bc::Opcode::Div,
        K::Percent | K::PercentEq => bc::Opcode::Rem,
        K::Amp | K::AmpEq => bc::Opcode::BitAnd, K::Pipe | K::PipeEq => bc::Opcode::BitOr,
        K::Caret | K::CaretEq => bc::Opcode::BitXor, K::Shl | K::ShlEq => bc::Opcode::Shl,
        K::Shr | K::ShrEq => bc::Opcode::Shr, K::EqEq => bc::Opcode::Eq, K::NotEq => bc::Opcode::Ne,
        K::Lt => bc::Opcode::Lt, K::Le => bc::Opcode::Le, K::Gt => bc::Opcode::Gt, K::Ge => bc::Opcode::Ge,
        _ => return None,
    })
}

fn hint_matches(hint: &crate::semantic::TypeHint, id: bc::TypeId, p: &bc::BytecodePackage, b: &mut Budget, depth: usize) -> Result<bool, Error> {
    b.work(1)?;
    if depth >= b.limits.max_depth.min(128) { return Err(b.error(ErrorKind::Limit, "trusted native type comparison depth exceeded")); }
    use crate::semantic::TypeHint as T;
    use bc::BuiltinType as B;
    let builtin = match hint {
        T::Any => Some(B::Any), T::Closure => Some(B::Closure), T::Null => Some(B::Null),
        T::Bool => Some(B::Bool), T::Int => Some(B::Int), T::Float => Some(B::Float), T::String => Some(B::String),
        T::Array => Some(B::Array), T::Table => Some(B::Table), T::Future => Some(B::Future), T::Generator => Some(B::Generator), _ => None,
    };
    let actual = &p.types[id.index()];
    if let Some(builtin) = builtin { return Ok(*actual == bc::TypeDef::Builtin(builtin)); }
    match (hint, actual) {
        (T::Tuple(expected), bc::TypeDef::Tuple(actual)) | (T::Union(expected), bc::TypeDef::Union(actual)) => {
            if expected.len() != actual.len() { return Ok(false); }
            b.work(expected.len() as u64)?;
            for (expected, &actual) in expected.iter().zip(actual) {
                if !hint_matches(expected, actual, p, b, depth+1)? { return Ok(false); }
            }
            Ok(true)
        }
        (T::Named(expected), bc::TypeDef::Named { name, .. }) => Ok(b.compare(expected, &p.symbols[name.index()])?.is_eq()),
        _ => Ok(false),
    }
}
