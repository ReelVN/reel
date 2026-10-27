mod arguments;
mod chains;
mod classes;
mod collections;
mod composition;
mod expansions;
mod expressions;
mod literals;
mod locations;
mod native_values;
mod patterns;

use super::project::{PackageState, ProjectLinks};
use super::{
    Error, ErrorKind, ExternalTarget, Linkage, Metadata, Options, budget::Budget, ir::*,
    registers::Registers,
};
use crate::{
    SyntaxKind as K, TextRange,
    hir::{self as h, Form, HirKind as H},
};
use aside_vm::bytecode as bc;
use std::borrow::Cow;

/// Admit the VM's editable clone and its temporary PC-remapping workspace.
/// The returned scratch reservation ends as soon as `to_unpacked` returns.
pub(super) fn admit_seed(seed: &bc::VerifiedProgram, b: &mut Budget) -> Result<usize, Box<Error>> {
    b.work(seed.package().functions.len() as u64)?;
    let cost = seed
        .editable_clone_cost()
        .map_err(|error| b.error(ErrorKind::Limit, error.to_string()))?;
    b.work(cost.work_units)?;
    let peak = cost
        .retained_capacity_bytes
        .checked_add(cost.scratch_peak_bytes)
        .ok_or_else(|| b.error(ErrorKind::Limit, "editable seed clone capacity overflow"))?;
    b.capacity(peak)?;
    Ok(cost.scratch_peak_bytes)
}

pub(super) fn clone_seed(
    seed: &bc::VerifiedProgram,
    b: &mut Budget,
) -> Result<bc::BytecodePackage, Box<Error>> {
    let scratch = admit_seed(seed, b)?;
    let package = seed.to_unpacked();
    b.release_bytes(scratch);
    package.map_err(|error| {
        b.error(
            match error.kind {
                bc::ErrorKind::Limit => ErrorKind::Limit,
                bc::ErrorKind::Allocation => ErrorKind::Allocation,
                _ => ErrorKind::Bytecode,
            },
            error.to_string(),
        )
    })
}

pub(super) fn lower<'a>(
    m: &'a h::AnalyzedModule,
    links: &'a Linkage<'a>,
    options: Options,
) -> Result<IrModule, Box<Error>> {
    let mut b = Budget::new(options.limits);
    b.span = m.hir().node(h::NodeId(0)).span;
    preflight(m, links, None, &mut b)?;
    b.span = m.hir().node(h::NodeId(0)).span;
    let mut package = if let Some(seed) = links.package {
        let stats = seed.stats();
        let records = usize::try_from(stats.records)
            .map_err(|_| b.error(ErrorKind::Limit, "trusted linkage record count overflow"))?;
        if records > b.limits.max_records.min(u32::MAX as usize) {
            return Err(b.error(
                ErrorKind::Limit,
                "trusted linkage exceeds lowering record budget",
            ));
        }
        b.stats.records = records;
        clone_seed(seed, &mut b)?
    } else {
        bc::BytecodePackage::default()
    };
    if links.package.is_some() && package.content_version != options.content_version {
        return Err(b.error(
            ErrorKind::LinkContract,
            "trusted linkage package must use the requested fixed content version",
        ));
    }
    package.content_version = options.content_version;
    let mut names = Vec::new();
    if let Some(seed) = links.package {
        b.work(seed.package().symbols.len() as u64)?;
        for (i, name) in seed.package().symbols.iter().enumerate() {
            b.push(&mut names, (name.as_str(), Some(bc::SymbolId(i as u32))))?;
        }
    }
    b.work(m.hir().names.len() as u64)?;
    for name in &m.hir().names {
        b.push(&mut names, (name.as_str(), None))?;
    }
    for expansion in links.expansions {
        for argument in &expansion.arguments {
            b.push(&mut names, (argument.name.as_str(), None))?;
        }
    }
    for name in [
        m.canonical_path(),
        m.source_path(),
        "$module",
        "$closure",
        "$default",
        "children",
    ] {
        b.push(&mut names, (name, None))?;
    }
    sort_names(&mut names, &mut b)?;
    let mut index: Vec<(&str, bc::SymbolId)> = Vec::new();
    let names_capacity = names.capacity() * std::mem::size_of::<(&str, Option<bc::SymbolId>)>();
    for (name, seed) in names {
        if let Some((old, id)) = index.last_mut()
            && b.compare(old, name)?.is_eq()
        {
            if let Some(seed) = seed {
                *id = seed;
            }
            continue;
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
    lower_prepared(
        m,
        links,
        options,
        PackageState { package, budget: b },
        Cow::Owned(index),
        None,
    )
}

pub(super) fn lower_prepared<'a>(
    m: &'a h::AnalyzedModule,
    links: &'a Linkage<'a>,
    options: Options,
    state: PackageState,
    names: Cow<'a, [(&'a str, bc::SymbolId)]>,
    project: Option<&'a ProjectLinks>,
) -> Result<IrModule, Box<Error>> {
    let PackageState {
        package,
        mut budget,
    } = state;
    if project.is_some() {
        preflight(m, links, project, &mut budget)?;
    }
    let b = budget;
    let module_id = project.map_or(bc::ModuleId(package.modules.len() as u32), |p| p.module);
    let symbol_count = m.symbols().len();
    let mut lower = Lowerer {
        m,
        links,
        options,
        b,
        package,
        names,
        project,
        type_cache: Vec::new(),
        contracts: Vec::new(),
        globals: Vec::new(),
        outputs: Vec::new(),
        defaults: Vec::new(),
        owned: Vec::new(),
        functions: Vec::new(),
        binding_identities: Vec::new(),
        native_identities: Vec::new(),
        native_values: Vec::new(),
        native_nodes: Vec::new(),
        native_symbols: Vec::new(),
        source: bc::SourceId(0),
        module: module_id,
    };
    lower.type_cache = lower.b.filled(m.types().len(), None)?;
    lower.native_values = lower.b.filled(lower.package.natives.len(), None)?;
    lower.b.work(lower.package.functions.len() as u64)?;
    for (index, function) in lower.package.functions.iter().enumerate() {
        if let bc::FunctionKind::Native(native) = function.kind {
            lower.native_values[native.index()] = Some(bc::FunctionId(index as u32));
        }
    }
    if !links.natives.is_empty() {
        lower.native_nodes = lower.b.filled(m.hir().nodes.len(), None)?;
        lower.native_symbols = lower.b.filled(symbol_count, None)?;
    }
    lower.contracts = lower.b.filled(symbol_count, bc::TypeId(0))?;
    lower.globals = lower.b.filled(symbol_count, None)?;
    lower.owned = lower.b.filled(m.functions().len() + 1, Vec::new())?;
    let any = lower.builtin(bc::BuiltinType::Any)?;
    lower.contracts.fill(any);
    lower.b.work(symbol_count as u64)?;
    for (i, symbol) in m.symbols().iter().enumerate() {
        lower.b.span = symbol.declaration;
        let owner = match symbol.origin {
            h::BindingOrigin::FunctionLocal(id) => Some(id.0 as usize),
            h::BindingOrigin::EntryLocal => Some(m.functions().len()),
            h::BindingOrigin::ModuleGlobal | h::BindingOrigin::External => None,
        };
        if let Some(owner) = owner {
            lower
                .b
                .push(&mut lower.owned[owner], h::SymbolId(i as u32))?;
        }
        if symbol.kind == h::SymbolKind::Function {
            lower.contracts[i] = lower.builtin(bc::BuiltinType::Closure)?;
        }
    }
    lower.b.work(m.hir().nodes.len() as u64)?;
    for (i, node) in m.hir().nodes.iter().enumerate() {
        if let Form::Binding {
            pattern,
            annotation: Some(_),
            ..
        } = node.form
            && let Some(symbol) = m.facts()[pattern.0 as usize].symbol
        {
            lower.b.span = m.hir().node(h::NodeId(i as u32)).span;
            lower.contracts[symbol.0 as usize] = lower.ty(m.symbols()[symbol.0 as usize].ty, 0)?;
        }
    }
    let path = lower.name(m.canonical_path())?;
    let source_path = lower.name(m.source_path())?;
    lower.source = bc::SourceId(lower.package.sources.len() as u32);
    let source = lower.b.string(&m.hir().source)?;
    lower.b.push(
        &mut lower.package.sources,
        bc::SourceFile {
            path: source_path,
            text: source,
        },
    )?;
    lower.module = project.map_or(bc::ModuleId(lower.package.modules.len() as u32), |p| {
        p.module
    });
    let mut imports = Vec::new();
    if let Some(project) = project {
        lower.b.reserve(&mut imports, project.imports.len())?;
        imports.extend_from_slice(&project.imports);
    }
    let mut module = bc::ModuleDef {
        path,
        imports,
        exports: Vec::new(),
        globals: Vec::new(),
        initializer: None,
    };
    lower.b.work(symbol_count as u64)?;
    for (index, s) in m.symbols().iter().enumerate() {
        if s.origin != h::BindingOrigin::ModuleGlobal {
            continue;
        }
        lower.b.span = s.declaration;
        let sym = h::SymbolId(index as u32);
        let id = bc::GlobalId(module.globals.len() as u32);
        lower.globals[sym.0 as usize] = Some(id);
        let name = lower.name(m.hir().name(s.name))?;
        lower.b.push(
            &mut module.globals,
            bc::GlobalDef {
                name,
                ty: lower.contracts[sym.0 as usize],
                mutable: s.mutable,
                linked_function: None,
            },
        )?;
    }
    lower.b.work(m.exports().len() as u64)?;
    for &sym in m.exports() {
        if project.is_some_and(|p| p.namespaces[sym.0 as usize]) {
            continue;
        }
        let global = lower.globals[sym.0 as usize]
            .or_else(|| project.and_then(|p| p.export_global(sym)))
            .ok_or_else(|| {
                lower.b.error(
                    ErrorKind::Unsupported,
                    "reexports require full module linkage",
                )
            })?;
        let name = lower.name(m.hir().name(m.symbols()[sym.0 as usize].name))?;
        lower
            .b
            .push(&mut module.exports, bc::Export { name, global })?;
    }
    if let Some(project) = project {
        let old = std::mem::replace(&mut lower.package.modules[project.module.index()], module);
        lower.b.release(old.globals);
        lower.b.release(old.imports);
        lower.b.release(old.exports);
    } else {
        lower.b.push(&mut lower.package.modules, module)?;
    }
    lower.prepare_functions()?;
    if project.is_some() {
        for (i, function) in m.functions().iter().enumerate() {
            if let Some(symbol) = m.facts()[function.node.0 as usize].symbol
                && m.symbols()[symbol.0 as usize].origin == h::BindingOrigin::ModuleGlobal
                && let Some(global) = lower.globals[symbol.0 as usize]
            {
                lower.package.modules[lower.module.index()].globals[global.index()]
                    .linked_function = Some(lower.outputs[i]);
            }
        }
    }
    lower.check_links()?;
    lower.check_imports()?;
    let entry = if matches!(
        m.entry_context(),
        crate::semantic::AnalysisContext::Function | crate::semantic::AnalysisContext::Generator
    ) {
        let id = m
            .functions()
            .iter()
            .position(|f| f.node == h::NodeId(0))
            .ok_or_else(|| {
                lower.b.error(
                    ErrorKind::InvalidIr,
                    "analyzed fragment lacks entry function",
                )
            })?;
        lower.outputs[id]
    } else {
        let output = lower.prepare_module()?;
        if m.entry_context() == crate::semantic::AnalysisContext::Module {
            lower.package.modules[lower.module.index()].initializer = Some(output);
        }
        lower.build_function(output, Origin::Module)?;
        output
    };
    for i in 0..lower.outputs.len() {
        let output = lower.outputs[i];
        lower.build_function(output, Origin::Function(h::FunctionId(i as u32)))?;
        for p in 0..m.functions()[i].parameters.len() {
            if let Some(default) = lower.defaults[i][p] {
                lower.build_function(
                    default,
                    Origin::Default {
                        function: h::FunctionId(i as u32),
                        parameter: p as u32,
                    },
                )?;
            }
        }
    }
    let mut declarations = Vec::new();
    lower.b.work(m.functions().len() as u64)?;
    for (index, function) in m.functions().iter().enumerate() {
        let Some(source_symbol) = m.facts()[function.node.0 as usize].symbol else {
            continue;
        };
        let symbol = &m.symbols()[source_symbol.0 as usize];
        if symbol.kind != h::SymbolKind::Function || symbol.origin != h::BindingOrigin::ModuleGlobal
        {
            continue;
        }
        lower.b.span = symbol.declaration;
        let global = lower.globals[source_symbol.0 as usize].ok_or_else(|| {
            lower.b.error(
                ErrorKind::InvalidIr,
                "module function lacks compiler global mapping",
            )
        })?;
        let name = lower.name(m.hir().name(symbol.name))?;
        lower.b.push(
            &mut declarations,
            super::DeclarationMetadata {
                name,
                source_symbol,
                source_function: h::FunctionId(index as u32),
                global,
                output: lower.outputs[index],
                span: symbol.declaration,
            },
        )?;
    }
    lower.b.release(lower.type_cache);
    lower.b.release(lower.contracts);
    let mut global_identities = Vec::new();
    lower.b.release(lower.native_values);
    lower.b.release(lower.native_nodes);
    lower.b.release(lower.native_symbols);
    lower.b.work(lower.globals.len() as u64)?;
    for (i, &global) in lower.globals.iter().enumerate() {
        if let Some(global) = global {
            lower.b.push(
                &mut global_identities,
                super::GlobalIdentity {
                    global,
                    source: h::SymbolId(i as u32),
                },
            )?;
        }
    }
    lower.b.release(lower.globals);
    for values in lower.defaults.drain(..) {
        lower.b.release(values);
    }
    lower.b.release(lower.defaults);
    for values in lower.owned.drain(..) {
        lower.b.release(values);
    }
    lower.b.release(lower.owned);
    if let Cow::Owned(names) = lower.names {
        lower.b.release(names);
    }
    let metadata_before = lower.b.stats.capacity_bytes;
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
        lower.b.push(
            &mut imports,
            h::Import {
                node: import.node,
                module,
                version,
                names,
            },
        )?;
    }
    let mut external_bindings = Vec::new();
    lower.b.work(m.external_bindings().len() as u64)?;
    lower
        .b
        .reserve(&mut external_bindings, m.external_bindings().len())?;
    external_bindings.extend_from_slice(m.external_bindings());
    let mut function_metadata = Vec::new();
    lower.b.work(m.functions().len() as u64)?;
    lower
        .b
        .reserve(&mut function_metadata, m.functions().len())?;
    function_metadata.extend(m.functions().iter().enumerate().map(|(i, f)| {
        super::FunctionMetadata {
            output: lower.outputs[i],
            declared_context: f.declared_context,
            known_wait: f.known_wait,
            dynamic_calls: f.dynamic_calls,
        }
    }));
    let outputs_capacity = lower.outputs.capacity() * std::mem::size_of::<bc::FunctionId>();
    lower.b.release(lower.outputs);
    let metadata_capacity = lower.b.stats.capacity_bytes - metadata_before
        + outputs_capacity
        + lower.binding_identities.capacity() * std::mem::size_of::<super::BindingIdentity>()
        + global_identities.capacity() * std::mem::size_of::<super::GlobalIdentity>()
        + declarations.capacity() * std::mem::size_of::<super::DeclarationMetadata>();
    let metadata_capacity = metadata_capacity
        + lower.native_identities.capacity() * std::mem::size_of::<super::NativeIdentity>();
    let metadata = Metadata {
        entry_context: m.entry_context(),
        functions: function_metadata,
        declarations,
        canonical_path,
        source_version,
        imports,
        external_bindings,
        bindings: lower.binding_identities,
        globals: global_identities,
        natives: lower.native_identities,
        symbol_count: m.symbols().len() as u32,
        source_span: m.hir().node(h::NodeId(0)).span,
        capacity_bytes: metadata_capacity,
    };
    Ok(IrModule {
        package: lower.package,
        functions: lower.functions,
        source: lower.source,
        module: lower.module,
        entry,
        stats: lower.b.stats,
        metadata,
        options,
        budget: lower.b,
    })
}

/// Fallible heapsort: comparisons charge work before examining text. A trusted
/// seed id wins ties, so duplicate source names retain the seed ABI identity.
pub(super) fn sort_names<T: Copy>(
    values: &mut [(&str, Option<T>)],
    b: &mut Budget,
) -> Result<(), Box<Error>> {
    fn less<T: Copy>(
        a: (&str, Option<T>),
        c: (&str, Option<T>),
        b: &mut Budget,
    ) -> Result<bool, Box<Error>> {
        Ok(b.compare(a.0, c.0)?
            .then_with(|| a.1.is_none().cmp(&c.1.is_none()))
            .is_lt())
    }
    fn sift<T: Copy>(
        values: &mut [(&str, Option<T>)],
        mut root: usize,
        end: usize,
        b: &mut Budget,
    ) -> Result<(), Box<Error>> {
        while root * 2 + 1 < end {
            let mut child = root * 2 + 1;
            if child + 1 < end && less(values[child], values[child + 1], b)? {
                child += 1;
            }
            if !less(values[root], values[child], b)? {
                break;
            }
            values.swap(root, child);
            root = child;
        }
        Ok(())
    }
    for root in (0..values.len() / 2).rev() {
        sift(values, root, values.len(), b)?;
    }
    for end in (1..values.len()).rev() {
        values.swap(0, end);
        sift(values, 0, end, b)?;
    }
    Ok(())
}

struct Lowerer<'a> {
    m: &'a h::AnalyzedModule,
    links: &'a Linkage<'a>,
    options: Options,
    b: Budget,
    package: bc::BytecodePackage,
    names: Cow<'a, [(&'a str, bc::SymbolId)]>,
    project: Option<&'a ProjectLinks>,
    type_cache: Vec<Option<bc::TypeId>>,
    contracts: Vec<bc::TypeId>,
    globals: Vec<Option<bc::GlobalId>>,
    outputs: Vec<bc::FunctionId>,
    defaults: Vec<Vec<Option<bc::FunctionId>>>,
    owned: Vec<Vec<h::SymbolId>>,
    functions: Vec<Function>,
    binding_identities: Vec<super::BindingIdentity>,
    native_identities: Vec<super::NativeIdentity>,
    native_values: Vec<Option<bc::FunctionId>>,
    native_nodes: Vec<Option<bc::NativeId>>,
    native_symbols: Vec<Option<bc::NativeId>>,
    source: bc::SourceId,
    module: bc::ModuleId,
}
mod linking;

impl Lowerer<'_> {
    fn name(&mut self, name: &str) -> Result<bc::SymbolId, Box<Error>> {
        let (mut lo, mut hi) = (0, self.names.len());
        while lo < hi {
            let mid = (lo + hi) / 2;
            match self.b.compare(self.names[mid].0, name)? {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Ok(self.names[mid].1),
            }
        }
        Err(self.b.error(
            ErrorKind::InvalidIr,
            "name absent from admitted symbol table",
        ))
    }
    fn builtin(&mut self, ty: bc::BuiltinType) -> Result<bc::TypeId, Box<Error>> {
        self.b.work(self.package.types.len() as u64 + 1)?;
        if let Some(i) = self
            .package
            .types
            .iter()
            .position(|t| *t == bc::TypeDef::Builtin(ty))
        {
            return Ok(bc::TypeId(i as u32));
        }
        let id = bc::TypeId(self.package.types.len() as u32);
        self.b
            .push(&mut self.package.types, bc::TypeDef::Builtin(ty))?;
        Ok(id)
    }
    fn ty(&mut self, id: h::TypeId, depth: usize) -> Result<bc::TypeId, Box<Error>> {
        self.b.work(1)?;
        if depth >= self.options.limits.max_depth {
            return Err(self
                .b
                .error(ErrorKind::Limit, "type lowering depth exceeded"));
        }
        if let Some(id) = self.type_cache[id.0 as usize] {
            return Ok(id);
        }
        use bc::BuiltinType as B;
        let builtin = match &self.m.types()[id.0 as usize] {
            h::Type::Any => Some(B::Any),
            h::Type::Null => Some(B::Null),
            h::Type::Bool => Some(B::Bool),
            h::Type::Int => Some(B::Int),
            h::Type::Float => Some(B::Float),
            h::Type::String => Some(B::String),
            h::Type::Array => Some(B::Array),
            h::Type::Table => Some(B::Table),
            h::Type::Future => Some(B::Future),
            h::Type::Generator => Some(B::Generator),
            h::Type::Named(name) if self.m.hir().name(*name) == "Bytes" => Some(B::Bytes),
            h::Type::Named(name) if self.m.hir().name(*name) == "Binding" => Some(B::PropertyRef),
            h::Type::Named(name) if self.m.hir().name(*name) == "tuple" => Some(B::Tuple),
            h::Type::Class(_) => Some(B::Class),
            h::Type::Closure | h::Type::Function(_) | h::Type::BoundMethod(_) => Some(B::Closure),
            _ => None,
        };
        let out = if let Some(ty) = builtin {
            self.builtin(ty)?
        } else {
            let def = match &self.m.types()[id.0 as usize] {
                h::Type::Instance(node) => {
                    let ty = self.class_type(*node)?;
                    self.type_cache[id.0 as usize] = Some(ty);
                    return Ok(ty);
                }
                h::Type::Named(name) => {
                    let ty = self.external_class_type(*name)?;
                    self.type_cache[id.0 as usize] = Some(ty);
                    return Ok(ty);
                }
                h::Type::Tuple(ids) | h::Type::Union(ids) => {
                    self.b.work(ids.len() as u64)?;
                    let mut out = Vec::new();
                    for &id in ids {
                        let id = self.ty(id, depth + 1)?;
                        self.b.push(&mut out, id)?;
                    }
                    if matches!(self.m.types()[id.0 as usize], h::Type::Tuple(_)) {
                        bc::TypeDef::Tuple(out)
                    } else {
                        bc::TypeDef::Union(out)
                    }
                }
                h::Type::Callable(parameters, result) => {
                    self.b.work(parameters.len() as u64)?;
                    let mut out = Vec::new();
                    for &parameter in parameters {
                        let ty = self.ty(parameter, depth + 1)?;
                        self.b.push(&mut out, ty)?;
                    }
                    bc::TypeDef::Function {
                        parameters: out,
                        return_type: self.ty(*result, depth + 1)?,
                    }
                }
                _ => {
                    return Err(self.b.error(
                        ErrorKind::Unsupported,
                        "this explicit type needs full named/class module linkage",
                    ));
                }
            };
            let out = bc::TypeId(self.package.types.len() as u32);
            self.b.push(&mut self.package.types, def)?;
            out
        };
        self.type_cache[id.0 as usize] = Some(out);
        Ok(out)
    }
    fn signature(
        &mut self,
        parameters: Vec<bc::Parameter>,
        return_type: bc::TypeId,
        context: bc::Context,
    ) -> Result<bc::SignatureId, Box<Error>> {
        let id = bc::SignatureId(self.package.signatures.len() as u32);
        self.b.push(
            &mut self.package.signatures,
            bc::Signature {
                parameters,
                rest: None,
                return_type,
                effects: if context == bc::Context::Ordinary {
                    bc::Effects::ALL
                } else {
                    bc::Effects(bc::Effects::ALL.0 & !bc::Effects::WAIT)
                },
                property_bag_tail: None,
            },
        )?;
        Ok(id)
    }
    fn empty_function(
        &mut self,
        name: &str,
        signature: bc::SignatureId,
        context: bc::Context,
        captures: Vec<bc::CaptureKind>,
    ) -> Result<bc::FunctionId, Box<Error>> {
        let name = self.name(name)?;
        let id = bc::FunctionId(self.package.functions.len() as u32);
        self.b.push(
            &mut self.package.functions,
            bc::FunctionDef {
                module: self.module,
                name,
                kind: bc::FunctionKind::Plain,
                signature,
                context,
                captures,
                register_count: 0,
                bindings: Vec::new(),
                descriptors: Vec::new(),
                code: Vec::new(),
                regions: Vec::new(),
                source_map: Vec::new(),
            },
        )?;
        Ok(id)
    }
    fn captures(&mut self, f: &h::Function) -> Result<Vec<bc::CaptureKind>, Box<Error>> {
        self.b.work(f.captures.len() as u64)?;
        let mut out = Vec::new();
        for capture in &f.captures {
            self.b.push(
                &mut out,
                match capture.mode {
                    h::CaptureMode::Value => bc::CaptureKind::Value,
                    h::CaptureMode::Cell => bc::CaptureKind::Cell,
                },
            )?;
        }
        Ok(out)
    }
    fn prepare_functions(&mut self) -> Result<(), Box<Error>> {
        for f in self.m.functions() {
            self.b.span = self.m.hir().node(f.node).span;
            if f.kind == h::FunctionKind::Async {
                return Err(self.b.error(
                    ErrorKind::Unsupported,
                    "async functions belong to later lowering",
                ));
            }
            let mut parameters = Vec::new();
            let mut rest = None;
            let mut defaults = Vec::new();
            self.b.work(f.parameters.len() as u64)?;
            for p in &f.parameters {
                let symbol = &self.m.symbols()[p.symbol.0 as usize];
                let ty = p
                    .annotation
                    .map(|id| self.ty(id, 0))
                    .transpose()?
                    .unwrap_or(self.builtin(bc::BuiltinType::Any)?);
                self.contracts[p.symbol.0 as usize] = ty;
                let name = self.name(self.m.hir().name(symbol.name))?;
                let parameter = bc::Parameter {
                    name,
                    ty,
                    mutable: symbol.mutable,
                    default: None,
                };
                if p.rest {
                    self.b.record()?;
                    rest = Some(parameter);
                } else {
                    self.b.push(&mut parameters, parameter)?;
                }
                self.b.push(&mut defaults, None)?;
            }
            let result = if matches!(
                self.m.hir().node(f.node).form,
                Form::Function {
                    result: Some(_),
                    ..
                }
            ) {
                self.ty(f.return_type, 0)?
            } else {
                self.builtin(bc::BuiltinType::Any)?
            };
            let context = if f.declared_context.derived && f.declared_context.init {
                return Err(self.b.error(
                    ErrorKind::Unsupported,
                    "B01 Context cannot encode combined Derived+Init declarations",
                ));
            } else if f.declared_context.derived {
                bc::Context::Derived
            } else if f.declared_context.init {
                bc::Context::Init
            } else if f.declared_context.no_wait {
                bc::Context::NoWait
            } else {
                bc::Context::Ordinary
            };
            let signature = self.signature(parameters, result, context)?;
            self.package.signatures[signature.index()].rest = rest;
            let name = match self.m.hir().node(f.node).form {
                Form::Function {
                    name: Some(token), ..
                } => self.m.hir().name(token.name.expect("function name")),
                _ => "$closure",
            };
            let captures = self.captures(f)?;
            let output = self.empty_function(name, signature, context, captures)?;
            if f.kind == h::FunctionKind::Generator {
                self.package.functions[output.index()].kind = bc::FunctionKind::Generator;
            }
            self.b.push(&mut self.outputs, output)?;
            self.b.push(&mut self.defaults, defaults)?;
        }
        for (i, f) in self.m.functions().iter().enumerate() {
            for (p, parameter) in f.parameters.iter().enumerate() {
                let Some(node) = parameter.default else {
                    continue;
                };
                self.b.span = self.m.hir().node(node).span;
                let body_signature = self.package.functions[self.outputs[i].index()].signature;
                if let Some(value) = self.literal_constant(node)? {
                    let mut existing = None;
                    for (index, constant) in self.package.constants.iter().enumerate() {
                        if source_constant_matches(constant, &value, &mut self.b)? {
                            existing = Some(bc::ConstId(index as u32));
                            break;
                        }
                    }
                    let id = if let Some(id) = existing {
                        self.b.release_bytes(match &value {
                            bc::Const::Utf8(s) => s.capacity(),
                            _ => 0,
                        });
                        id
                    } else {
                        let id = bc::ConstId(self.package.constants.len() as u32);
                        self.b.push(&mut self.package.constants, value)?;
                        id
                    };
                    self.package.signatures[body_signature.index()].parameters[p].default =
                        Some(bc::ParameterDefault::Constant(id));
                    continue;
                }
                self.b.work(p as u64)?;
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
                let default =
                    self.empty_function("$default", signature, bc::Context::NoWait, captures)?;
                self.defaults[i][p] = Some(default);
                self.package.signatures[body_signature.index()].parameters[p].default =
                    Some(bc::ParameterDefault::Function(default));
            }
        }
        Ok(())
    }
    fn prepare_module(&mut self) -> Result<bc::FunctionId, Box<Error>> {
        let result = self.builtin(bc::BuiltinType::Any)?;
        let context = match self.m.entry_context() {
            crate::semantic::AnalysisContext::Module => bc::Context::Init,
            crate::semantic::AnalysisContext::Function => bc::Context::Ordinary,
            crate::semantic::AnalysisContext::NoWait => bc::Context::NoWait,
            crate::semantic::AnalysisContext::Generator => bc::Context::NoWait,
        };
        let signature = self.signature(Vec::new(), result, context)?;
        let output = self.empty_function("$module", signature, context, Vec::new())?;
        if self.m.entry_context() == crate::semantic::AnalysisContext::Generator {
            self.package.functions[output.index()].kind = bc::FunctionKind::Generator;
        }
        Ok(output)
    }

    fn build_function(&mut self, output: bc::FunctionId, origin: Origin) -> Result<(), Box<Error>> {
        let node = match origin {
            Origin::Module => h::NodeId(0),
            Origin::Function(id) => self.m.functions()[id.0 as usize].body,
            Origin::Default {
                function,
                parameter,
            } => self.m.functions()[function.0 as usize].parameters[parameter as usize]
                .default
                .expect("default entry"),
        };
        self.b.span = self.m.hir().node(node).span;
        let placeholder = bc::FunctionDef {
            module: self.module,
            name: bc::SymbolId(0),
            kind: bc::FunctionKind::Plain,
            signature: bc::SignatureId(0),
            context: bc::Context::Ordinary,
            captures: Vec::new(),
            register_count: 0,
            bindings: Vec::new(),
            descriptors: Vec::new(),
            code: Vec::new(),
            regions: Vec::new(),
            source_map: Vec::new(),
        };
        let def = std::mem::replace(&mut self.package.functions[output.index()], placeholder);
        let mut builder = FunctionBuilder::new(self, def, output, origin, node)?;
        let value = builder.expr(node, 0)?;
        if builder.current.is_some() {
            let value = match value {
                Some(value) => value,
                None => builder.constant(bc::Const::Null, node)?,
            };
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
    loops: Vec<LoopTarget>,
    collector: Option<bc::Reg>,
    no_wait: bool,
    protected_depth: usize,
}

#[derive(Clone, Copy)]
struct LoopTarget {
    node: h::NodeId,
    next: BlockId,
    end: BlockId,
    protected_depth: usize,
    next_protected_depth: usize,
    has_break: bool,
}

impl<'b, 'a> FunctionBuilder<'b, 'a> {
    fn new(
        l: &'b mut Lowerer<'a>,
        mut def: bc::FunctionDef,
        output: bc::FunctionId,
        origin: Origin,
        node: h::NodeId,
    ) -> Result<Self, Box<Error>> {
        let (owner, prefix) = match origin {
            Origin::Module => (None, 0),
            Origin::Function(id) => (Some(id), l.m.functions()[id.0 as usize].parameters.len()),
            Origin::Default {
                function,
                parameter,
            } => (Some(function), parameter as usize),
        };
        let owned = owner.map_or(l.m.functions().len(), |id| id.0 as usize);
        let span = l.m.hir().node(node).span;
        // Admit fixed slots before allocating their BindingDef/local tables.
        let mut fixed = prefix;
        l.b.work(l.owned[owned].len() as u64)?;
        for &sym in &l.owned[owned] {
            let symbol = &l.m.symbols()[sym.0 as usize];
            if body_binding(symbol, &origin, span) {
                l.b.span = symbol.declaration;
                fixed += 1;
                if fixed > l.b.limits.max_registers as usize {
                    return Err(l
                        .b
                        .error(ErrorKind::Limit, "fixed register slots exceed limit"));
                }
            }
        }
        let fixed = u32::try_from(fixed).map_err(|_| {
            l.b.error(ErrorKind::Limit, "fixed register slot count overflow")
        })?;
        let registers = Registers::new(fixed, &l.b)?;
        let mut locals = Vec::new();
        let mut captures = Vec::new();
        if let Some(owner) = owner {
            let f = &l.m.functions()[owner.0 as usize];
            l.b.work(prefix as u64 + f.captures.len() as u64)?;
            for (i, param) in f.parameters.iter().take(prefix).enumerate() {
                let symbol = &l.m.symbols()[param.symbol.0 as usize];
                let name = l.name(l.m.hir().name(symbol.name))?;
                let id = bc::BindingId(def.bindings.len() as u32);
                l.b.push(
                    &mut def.bindings,
                    bc::BindingDef {
                        name,
                        ty: l.contracts[param.symbol.0 as usize],
                        mutable: symbol.mutable,
                        storage: if symbol.storage == h::Storage::SharedCell || symbol.mutable {
                            bc::BindingStorage::Cell(bc::Reg(i as u32))
                        } else {
                            bc::BindingStorage::Value(bc::Reg(i as u32))
                        },
                        initialization: bc::BindingInitialization::Parameter(i as u32),
                    },
                )?;
                l.b.push(&mut locals, (param.symbol, id))?;
            }
            for (i, capture) in f.captures.iter().enumerate() {
                l.b.push(&mut captures, (capture.symbol, bc::CaptureId(i as u32)))?;
            }
        }
        l.b.work(l.owned[owned].len() as u64)?;
        for owned_index in 0..l.owned[owned].len() {
            let sym = l.owned[owned][owned_index];
            let symbol = &l.m.symbols()[sym.0 as usize];
            if !body_binding(symbol, &origin, span) {
                continue;
            }
            l.b.span = symbol.declaration;
            let name = l.name(l.m.hir().name(symbol.name))?;
            let id = bc::BindingId(def.bindings.len() as u32);
            let reg = bc::Reg(id.0);
            l.b.push(
                &mut def.bindings,
                bc::BindingDef {
                    name,
                    ty: l.contracts[sym.0 as usize],
                    mutable: symbol.mutable,
                    storage: if symbol.storage == h::Storage::SharedCell {
                        bc::BindingStorage::Cell(reg)
                    } else {
                        bc::BindingStorage::Value(reg)
                    },
                    initialization: bc::BindingInitialization::Explicit,
                },
            )?;
            l.b.push(&mut locals, (sym, id))?;
        }
        let sort_work = (locals.len() + captures.len()) as u64 * (usize::BITS as u64);
        l.b.work(sort_work)?;
        locals.sort_unstable_by_key(|(s, _)| *s);
        captures.sort_unstable_by_key(|(s, _)| *s);
        debug_assert_eq!(def.bindings.len(), fixed as usize);
        let mut out = Self {
            l,
            def,
            ir: Function {
                output,
                origin,
                span,
                blocks: Vec::new(),
                layout: Vec::new(),
                regions: Vec::new(),
            },
            current: None,
            registers,
            locals,
            captures,
            loops: Vec::new(),
            collector: None,
            no_wait: false,
            protected_depth: 0,
        };
        let entry = out.block()?;
        out.start(entry)?;
        if let Some(owner) = owner {
            for index in 0..out.l.owned[owner.0 as usize].len() {
                let sym = out.l.owned[owner.0 as usize][index];
                if out.l.m.symbols()[sym.0 as usize].kind == h::SymbolKind::Super {
                    let value = out.temp()?;
                    out.op(bc::Opcode::SuperRef, value.0, 0, 0, node)?;
                    if let Some(binding) = out.local(sym)? {
                        if matches!(
                            out.def.bindings[binding.index()].storage,
                            bc::BindingStorage::Cell(_)
                        ) {
                            out.op(bc::Opcode::CellNew, binding.0, 0, 0, node)?;
                        }
                        out.op(bc::Opcode::BindInit, binding.0, value.0, 0, node)?;
                    }
                    out.release(value)?;
                }
            }
        }
        Ok(out)
    }
    fn span(&self, n: h::NodeId) -> TextRange {
        self.l.m.hir().node(n).span
    }
    fn block(&mut self) -> Result<BlockId, Box<Error>> {
        if self.l.b.stats.blocks >= self.l.b.limits.max_blocks {
            return Err(self
                .l
                .b
                .error(ErrorKind::Limit, "basic block budget exceeded"));
        }
        self.l.b.stats.blocks += 1;
        let id = BlockId(self.ir.blocks.len() as u32);
        self.l.b.push(
            &mut self.ir.blocks,
            Block {
                operations: Vec::new(),
                terminator: None,
            },
        )?;
        Ok(id)
    }
    fn start(&mut self, block: BlockId) -> Result<(), Box<Error>> {
        if self.current.is_some() {
            return Err(self.l.b.error(
                ErrorKind::InvalidIr,
                "start before terminating the prior block",
            ));
        }
        self.l.b.push(&mut self.ir.layout, block)?;
        self.current = Some(block);
        Ok(())
    }
    fn instruction(&mut self) -> Result<(), Box<Error>> {
        if self.l.b.stats.instructions >= self.l.b.limits.max_instructions {
            return Err(self
                .l
                .b
                .error(ErrorKind::Limit, "instruction budget exceeded"));
        }
        self.l.b.stats.instructions += 1;
        Ok(())
    }
    fn op(
        &mut self,
        opcode: bc::Opcode,
        a: u32,
        b: u32,
        c: u32,
        n: h::NodeId,
    ) -> Result<(), Box<Error>> {
        self.l.b.span = self.span(n);
        self.instruction()?;
        let current = self.current.ok_or_else(|| {
            self.l
                .b
                .error(ErrorKind::InvalidIr, "operation after a terminator")
        })?;
        let span = self.span(n);
        self.l.b.push(
            &mut self.ir.blocks[current.0 as usize].operations,
            Operation {
                instruction: bc::Instruction::new(opcode, a, b, c),
                span,
            },
        )
    }
    fn terminate(&mut self, term: Terminator, n: h::NodeId) -> Result<(), Box<Error>> {
        self.l.b.span = self.span(n);
        self.instruction()?;
        self.l.b.record()?;
        let current = self
            .current
            .take()
            .ok_or_else(|| self.l.b.error(ErrorKind::InvalidIr, "duplicate terminator"))?;
        self.ir.blocks[current.0 as usize].terminator = Some((term, self.span(n)));
        Ok(())
    }
    fn temp(&mut self) -> Result<bc::Reg, Box<Error>> {
        self.registers.temp(&mut self.l.b)
    }
    fn release(&mut self, reg: bc::Reg) -> Result<(), Box<Error>> {
        if let Some(current) = self.current {
            self.instruction()?;
            let span = self.l.b.span;
            self.l.b.push(
                &mut self.ir.blocks[current.0 as usize].operations,
                Operation {
                    instruction: bc::Instruction::new(bc::Opcode::Clear, reg.0, 0, 0),
                    span,
                },
            )?;
        }
        self.registers.drop_temp(reg, &mut self.l.b)
    }
    fn release_window(&mut self, window: bc::RegWindow, n: h::NodeId) -> Result<(), Box<Error>> {
        self.l.b.span = self.span(n);
        self.l.b.work(window.len as u64)?;
        if self.current.is_some() {
            for offset in 0..window.len {
                self.op(bc::Opcode::Clear, window.start.0 + offset, 0, 0, n)?;
            }
        }
        self.registers.release(window, &mut self.l.b)
    }
    fn descriptor(&mut self, descriptor: bc::Descriptor) -> Result<bc::DescriptorId, Box<Error>> {
        let id = bc::DescriptorId(self.def.descriptors.len() as u32);
        self.l.b.push(&mut self.def.descriptors, descriptor)?;
        Ok(id)
    }
    fn constant(&mut self, constant: bc::Const, n: h::NodeId) -> Result<bc::Reg, Box<Error>> {
        let id = bc::ConstId(self.l.package.constants.len() as u32);
        self.l.b.push(&mut self.l.package.constants, constant)?;
        let out = self.temp()?;
        self.op(bc::Opcode::LoadConst, out.0, id.0, 0, n)?;
        Ok(out)
    }
    fn local(&mut self, symbol: h::SymbolId) -> Result<Option<bc::BindingId>, Box<Error>> {
        self.l.b.work(usize::BITS as u64)?;
        Ok(self
            .locals
            .binary_search_by_key(&symbol, |(s, _)| *s)
            .ok()
            .map(|i| self.locals[i].1))
    }
    fn capture(&mut self, symbol: h::SymbolId) -> Result<Option<bc::CaptureId>, Box<Error>> {
        self.l.b.work(usize::BITS as u64)?;
        Ok(self
            .captures
            .binary_search_by_key(&symbol, |(s, _)| *s)
            .ok()
            .map(|i| self.captures[i].1))
    }
    fn read(&mut self, symbol: h::SymbolId, n: h::NodeId) -> Result<bc::Reg, Box<Error>> {
        let out = self.temp()?;
        if let Some(id) = self.local(symbol)? {
            self.op(bc::Opcode::BindRead, out.0, id.0, 0, n)?;
        } else if let Some(global) = self.l.globals[symbol.0 as usize] {
            let desc = self.descriptor(bc::Descriptor::Global {
                module: self.l.module,
                global,
            })?;
            self.op(bc::Opcode::GlobalRead, out.0, desc.0, 0, n)?;
        } else if let Some(capture) = self.capture(symbol)? {
            self.op(bc::Opcode::CaptureRead, out.0, capture.0, 0, n)?;
            if self.def.captures[capture.index()] == bc::CaptureKind::Cell {
                self.op(bc::Opcode::CellRead, out.0, out.0, 0, n)?;
            }
        } else if let Some(target) = self.l.project.and_then(|p| p.symbol(symbol)) {
            let desc = self.descriptor(bc::Descriptor::Global {
                module: target.module,
                global: target.global,
            })?;
            self.op(bc::Opcode::GlobalRead, out.0, desc.0, 0, n)?;
        } else {
            return Err(self.l.b.error(
                ErrorKind::MissingLink,
                "external value has no linked binding",
            ));
        }
        Ok(out)
    }
    fn write(
        &mut self,
        symbol: h::SymbolId,
        value: bc::Reg,
        initialize: bool,
        n: h::NodeId,
    ) -> Result<(), Box<Error>> {
        if let Some(binding) = self.local(symbol)? {
            self.op(
                if initialize {
                    bc::Opcode::BindInit
                } else {
                    bc::Opcode::BindWrite
                },
                binding.0,
                value.0,
                0,
                n,
            )?;
        } else if let Some(global) = self.l.globals[symbol.0 as usize] {
            let desc = self.descriptor(bc::Descriptor::Global {
                module: self.l.module,
                global,
            })?;
            self.op(
                if initialize {
                    bc::Opcode::GlobalInit
                } else {
                    bc::Opcode::GlobalWrite
                },
                value.0,
                desc.0,
                0,
                n,
            )?;
        } else if let Some(capture) = self.capture(symbol)? {
            if initialize || self.def.captures[capture.index()] != bc::CaptureKind::Cell {
                return Err(self.l.b.error(
                    ErrorKind::InvalidIr,
                    "write requires a shared captured Cell",
                ));
            }
            let cell = self.temp()?;
            self.op(bc::Opcode::CaptureRead, cell.0, capture.0, 0, n)?;
            self.op(bc::Opcode::CellWrite, cell.0, value.0, 0, n)?;
            self.release(cell)?;
        } else {
            return Err(self.l.b.error(
                ErrorKind::MissingLink,
                "assignment needs a linked lexical binding",
            ));
        }
        Ok(())
    }
    fn capture_sources(
        &mut self,
        function: h::FunctionId,
    ) -> Result<Vec<bc::CaptureSource>, Box<Error>> {
        let mut captures = Vec::new();
        let f = &self.l.m.functions()[function.0 as usize];
        self.l.b.work(f.captures.len() as u64)?;
        for capture in &f.captures {
            let source = if let Some(binding) = self.local(capture.symbol)? {
                bc::CaptureSource::Binding(binding)
            } else if let Some(capture) = self.capture(capture.symbol)? {
                bc::CaptureSource::Capture(capture)
            } else {
                return Err(self
                    .l
                    .b
                    .error(ErrorKind::InvalidIr, "missing defining capture identity"));
            };
            self.l.b.push(&mut captures, source)?;
        }
        Ok(captures)
    }
    fn closure(&mut self, function: h::FunctionId, n: h::NodeId) -> Result<bc::Reg, Box<Error>> {
        let captures = self.capture_sources(function)?;
        let descriptor = self.descriptor(bc::Descriptor::Closure {
            function: self.l.outputs[function.0 as usize],
            captures,
        })?;
        let out = self.temp()?;
        self.op(bc::Opcode::MakeClosure, out.0, descriptor.0, 0, n)?;
        Ok(out)
    }
    fn enter_scope(&mut self, n: h::NodeId) -> Result<(), Box<Error>> {
        if let Some(scope) = self.l.m.facts()[n.0 as usize].scope {
            let symbols = &self.l.m.scopes()[scope.0 as usize].symbols;
            self.l.b.work(symbols.len() as u64)?;
            for &sym in symbols {
                if self.l.m.symbols()[sym.0 as usize].kind == h::SymbolKind::Super {
                    continue;
                }
                if let Some(binding) = self.local(sym)? {
                    let binding_def = &self.def.bindings[binding.index()];
                    if binding_def.initialization == bc::BindingInitialization::Explicit {
                        match binding_def.storage {
                            bc::BindingStorage::Cell(_) => {
                                self.op(bc::Opcode::CellNew, binding.0, 0, 0, n)?
                            }
                            bc::BindingStorage::Value(reg) => {
                                self.op(bc::Opcode::Clear, reg.0, 0, 0, n)?
                            }
                        }
                    }
                }
            }
        }
        let children = self.l.m.hir().children(n);
        self.l.b.work(self.l.m.hir().elements(n).len() as u64)?;
        for child in children {
            let child = match self.l.m.hir().node(child).form {
                Form::Modified { declaration, .. } => declaration,
                _ => child,
            };
            if self.l.m.hir().node(child).kind == H::Function {
                let fact = &self.l.m.facts()[child.0 as usize];
                let function = fact.function.ok_or_else(|| {
                    self.l.b.error(
                        ErrorKind::InvalidIr,
                        "function declaration lacks analysis identity",
                    )
                })?;
                let symbol = fact.symbol.ok_or_else(|| {
                    self.l
                        .b
                        .error(ErrorKind::InvalidIr, "function declaration lacks binding")
                })?;
                if self.l.project.is_some()
                    && self.l.m.symbols()[symbol.0 as usize].origin
                        == h::BindingOrigin::ModuleGlobal
                {
                    continue;
                }
                let value = self.closure(function, child)?;
                self.write(symbol, value, true, child)?;
                self.release(value)?;
            }
        }
        Ok(())
    }
    fn finish(mut self) -> Result<(), Box<Error>> {
        self.def.register_count = self.registers.high;
        self.l.b.stats.registers += self.registers.high as u64;
        self.l.b.stats.max_function_registers = self
            .l
            .b
            .stats
            .max_function_registers
            .max(self.registers.high);
        self.registers.finish(&mut self.l.b);
        self.l.b.work(self.locals.len() as u64)?;
        for &(source, binding) in &self.locals {
            self.l.b.push(
                &mut self.l.binding_identities,
                super::BindingIdentity {
                    function: self.ir.output,
                    binding,
                    source,
                },
            )?;
        }
        self.l.b.release(self.locals);
        self.l.b.release(self.captures);
        self.l.b.release(self.loops);
        self.l.package.functions[self.ir.output.index()] = self.def;
        self.l.b.push(&mut self.l.functions, self.ir)
    }
}

fn body_binding(symbol: &h::Symbol, origin: &Origin, span: TextRange) -> bool {
    (symbol.kind == h::SymbolKind::Super
        || !matches!(
            symbol.initialization,
            h::Initialization::Parameter | h::Initialization::External
        ))
        && (!matches!(origin, Origin::Default { .. }) || span.contains_range(symbol.declaration))
}

fn binary(operator: K) -> Option<bc::Opcode> {
    Some(match operator {
        K::Plus | K::PlusEq => bc::Opcode::Add,
        K::Minus | K::MinusEq => bc::Opcode::Sub,
        K::Star | K::StarEq => bc::Opcode::Mul,
        K::Slash | K::SlashEq => bc::Opcode::Div,
        K::Percent | K::PercentEq => bc::Opcode::Rem,
        K::Amp | K::AmpEq => bc::Opcode::BitAnd,
        K::Pipe | K::PipeEq => bc::Opcode::BitOr,
        K::Caret | K::CaretEq => bc::Opcode::BitXor,
        K::Shl | K::ShlEq => bc::Opcode::Shl,
        K::Shr | K::ShrEq => bc::Opcode::Shr,
        K::EqEq => bc::Opcode::Eq,
        K::NotEq => bc::Opcode::Ne,
        K::Lt => bc::Opcode::Lt,
        K::Le => bc::Opcode::Le,
        K::Gt => bc::Opcode::Gt,
        K::Ge => bc::Opcode::Ge,
        _ => return None,
    })
}

mod admission;
use admission::{hint_matches, preflight, source_constant_matches, source_signature_matches};
