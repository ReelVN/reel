//! One symbol pool, package and work ledger for the analyzed module graph.
use super::{budget::Budget, build, emit, *};
use crate::{
    hir as h,
    semantic::{ModulePlan, SymbolReference},
};
use std::borrow::Cow;
use std::mem::size_of;

pub(super) struct PackageState {
    pub package: bc::BytecodePackage,
    pub budget: Budget,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct GlobalTarget {
    pub module: bc::ModuleId,
    pub global: bc::GlobalId,
}
pub(super) struct ProjectLinks {
    pub module: bc::ModuleId,
    pub imports: Vec<bc::Import>,
    symbols: Vec<Option<GlobalTarget>>,
    nodes: Vec<Option<GlobalTarget>>,
    exports: Vec<Option<bc::GlobalId>>,
    pub namespaces: Vec<bool>,
}
impl ProjectLinks {
    pub fn symbol(&self, symbol: h::SymbolId) -> Option<GlobalTarget> {
        self.symbols[symbol.0 as usize]
    }
    pub fn node(&self, node: h::NodeId) -> Option<GlobalTarget> {
        self.nodes[node.0 as usize]
    }
    pub fn export_global(&self, symbol: h::SymbolId) -> Option<bc::GlobalId> {
        self.exports[symbol.0 as usize]
    }
    fn release(self, b: &mut Budget) {
        b.release(self.imports);
        b.release(self.symbols);
        b.release(self.nodes);
        b.release(self.exports);
        b.release(self.namespaces);
    }
}

#[derive(Debug)]
pub struct ProjectModule {
    pub module: bc::ModuleId,
    pub source: bc::SourceId,
    pub initializer: bc::FunctionId,
    pub metadata: Metadata,
}
#[derive(Debug)]
pub struct ProjectCompilation {
    pub program: bc::VerifiedProgram,
    pub stats: Stats,
    pub modules: Vec<ProjectModule>,
    pub initializer: bc::FunctionId,
    pub initialization_order: Vec<bc::ModuleId>,
}
#[derive(Debug)]
pub struct ProjectError {
    pub module: String,
    pub error: Box<Error>,
}
impl std::fmt::Display for ProjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.module, self.error)
    }
}
impl std::error::Error for ProjectError {}

struct ModuleIndex<'a> {
    path: &'a str,
    version: &'a str,
    input: usize,
    module: bc::ModuleId,
}
type BindingIndex = ((u32, u32), GlobalTarget);
struct LinkTables<'a> {
    index: &'a [ModuleIndex<'a>],
    bindings: &'a [BindingIndex],
    names: &'a [(&'a str, bc::SymbolId)],
    native_exports: &'a [Option<GlobalTarget>],
    package: &'a bc::BytecodePackage,
}

/// Native links retain the exact analyzed registration identities. Source
/// bindings are resolved internally, including reexports and legal SCCs.
pub fn compile_project(
    modules: &[h::AnalyzedModule],
    plan: &ModulePlan,
    seed: &bc::VerifiedProgram,
    natives: &[Vec<NativeLink<'_>>],
    expansions: &[Vec<CallExpansion>],
    options: Options,
) -> Result<ProjectCompilation, ProjectError> {
    let mut current = "<project>";
    let result = (|| -> Result<ProjectCompilation, Box<Error>> {
        let mut b = Budget::new(options.limits);
        if modules.len() != natives.len() || modules.is_empty() {
            return Err(b.error(
                ErrorKind::LinkContract,
                "project requires one native-link set per source module",
            ));
        }
        if !expansions.is_empty() && expansions.len() != modules.len() {
            return Err(b.error(
                ErrorKind::LinkContract,
                "project requires one expansion set per source module",
            ));
        }
        if seed.package().content_version != options.content_version {
            return Err(b.error(
                ErrorKind::LinkContract,
                "project and trusted seed content versions differ",
            ));
        }
        b.capacity(seed.stats().owned_capacity_bytes - seed.stats().derived_capacity_bytes)?;
        b.work(seed.stats().records + seed.stats().encoded_bytes as u64)?;
        b.stats.records = seed.stats().records as usize;
        let mut package = seed.package().clone();
        let names = names(modules, seed, expansions, &mut package, &mut b)?;
        let name = |text: &str, b: &mut Budget| name(&names, text, b);
        let mut index = Vec::new();
        for (input, module) in modules.iter().enumerate() {
            b.push(
                &mut index,
                ModuleIndex {
                    path: module.canonical_path(),
                    version: module.version(),
                    input,
                    module: bc::ModuleId(0),
                },
            )?;
        }
        let sort_work = modules
            .iter()
            .map(|m| m.canonical_path().len() as u64 + 1)
            .sum::<u64>()
            * (usize::BITS - modules.len().leading_zeros() + 1) as u64
            * 8;
        b.work(sort_work)?;
        index.sort_unstable_by_key(|m| m.path);
        for pair in index.windows(2) {
            if pair[0].path == pair[1].path {
                return Err(b.error(ErrorKind::LinkContract, "duplicate source module identity"));
            }
        }
        let any = builtin(&mut package, bc::BuiltinType::Any, &mut b)?;
        let closure = builtin(&mut package, bc::BuiltinType::Closure, &mut b)?;
        let mut bindings = Vec::new();
        for entry in &mut index {
            current = entry.path;
            entry.module = bc::ModuleId(package.modules.len() as u32);
            let module = &modules[entry.input];
            let mut globals = Vec::new();
            for symbol in module
                .symbols()
                .iter()
                .filter(|s| s.origin == h::BindingOrigin::ModuleGlobal)
            {
                let symbol_name = name(module.hir().name(symbol.name), &mut b)?;
                let global = bc::GlobalId(globals.len() as u32);
                b.push(
                    &mut globals,
                    bc::GlobalDef {
                        name: symbol_name,
                        ty: any,
                        mutable: symbol.mutable,
                        linked_function: None,
                    },
                )?;
                b.push(
                    &mut bindings,
                    (
                        (entry.module.0, symbol_name.0),
                        GlobalTarget {
                            module: entry.module,
                            global,
                        },
                    ),
                )?;
            }
            let path = name(entry.path, &mut b)?;
            b.push(
                &mut package.modules,
                bc::ModuleDef {
                    path,
                    globals,
                    imports: Vec::new(),
                    exports: Vec::new(),
                    initializer: None,
                },
            )?;
        }
        b.work(
            bindings.len() as u64
                * (usize::BITS - bindings.len().max(1).leading_zeros() + 1) as u64
                * 8,
        )?;
        bindings.sort_unstable_by_key(|(key, _)| *key);
        // Reexported native callables use the same prepared function values.
        let mut native_exports = b.filled(package.natives.len(), None)?;
        let mut requested_natives = b.filled(package.natives.len(), false)?;
        for (module, links) in modules.iter().zip(natives) {
            for link in links {
                if let ExternalTarget::Symbol(symbol) = link.target
                    && module.exports().contains(&symbol)
                {
                    requested_natives[link.native.index()] = true;
                }
            }
        }
        for function in 0..package.functions.len() {
            let bc::FunctionKind::Native(native) = package.functions[function].kind else {
                continue;
            };
            if !requested_natives[native.index()] {
                continue;
            }
            let module = package.functions[function].module;
            let symbol = package.natives[native.index()].symbol;
            let global = bc::GlobalId(package.modules[module.index()].globals.len() as u32);
            b.push(
                &mut package.modules[module.index()].globals,
                bc::GlobalDef {
                    name: symbol,
                    ty: closure,
                    mutable: false,
                    linked_function: Some(bc::FunctionId(function as u32)),
                },
            )?;
            b.push(
                &mut package.modules[module.index()].exports,
                bc::Export {
                    name: symbol,
                    global,
                },
            )?;
            native_exports[native.index()] = Some(GlobalTarget { module, global });
        }
        b.release(requested_natives);
        let mut results = Vec::new();
        let mut state = PackageState { package, budget: b };
        for entry in &index {
            current = entry.path;
            let module = &modules[entry.input];
            let layout = links(
                module,
                entry.module,
                LinkTables {
                    index: &index,
                    bindings: &bindings,
                    names: &names,
                    native_exports: &native_exports,
                    package: &state.package,
                },
                &natives[entry.input],
                &mut state.budget,
            )?;
            let mut ir = build::lower_prepared(
                module,
                &Linkage {
                    package: Some(seed),
                    natives: &natives[entry.input],
                    expansions: expansions.get(entry.input).map_or(&[], Vec::as_slice),
                },
                options,
                state,
                Cow::Borrowed(&names),
                Some(&layout),
            )?;
            emit::write(&mut ir)?;
            let output = ProjectModule {
                module: ir.module,
                source: ir.source,
                initializer: ir.entry,
                metadata: ir.metadata,
            };
            state = PackageState {
                package: ir.package,
                budget: ir.budget,
            };
            layout.release(&mut state.budget);
            state.budget.push(&mut results, output)?;
        }
        current = "<project>";
        let mut order = Vec::new();
        for path in &plan.initialization_order {
            if let Ok(position) = index.binary_search_by_key(&path.as_str(), |m| m.path) {
                state.budget.push(&mut order, index[position].module)?;
            }
        }
        if order.len() != results.len() {
            return Err(state.budget.error(
                ErrorKind::LinkContract,
                "initialization plan does not cover source modules",
            ));
        }
        let initializer = initializer(&mut state.package, &order, &names, &mut state.budget)?;
        super::constants::intern(&mut state.package, &mut state.budget)?;
        state.budget.release(index);
        state.budget.release(bindings);
        state.budget.release(native_exports);
        state.budget.release(names);
        let metadata_bytes = results
            .iter()
            .map(|m| m.metadata.capacity_bytes)
            .sum::<usize>()
            + results.capacity() * size_of::<ProjectModule>()
            + order.capacity() * size_of::<bc::ModuleId>();
        let mut limits = options.bytecode_limits;
        limits.max_allocation_bytes = limits.max_allocation_bytes.min(
            options
                .limits
                .max_capacity_bytes
                .saturating_sub(metadata_bytes),
        );
        let program = bc::verify(state.package, limits)
            .map_err(|e| state.budget.error(ErrorKind::Bytecode, e.to_string()))?;
        let package_stats = program.stats();
        let mut stats = state.budget.stats;
        stats.capacity_bytes =
            package_stats.owned_capacity_bytes - size_of::<bc::BytecodePackage>() + metadata_bytes;
        stats.capacity_peak_bytes = stats.capacity_peak_bytes.max(
            stats.capacity_bytes - package_stats.derived_capacity_bytes
                + package_stats.validation_scratch_peak,
        );
        Ok(ProjectCompilation {
            program,
            stats,
            modules: results,
            initializer,
            initialization_order: order,
        })
    })();
    result.map_err(|error| ProjectError {
        module: current.into(),
        error,
    })
}

fn names<'a>(
    modules: &'a [h::AnalyzedModule],
    seed: &'a bc::VerifiedProgram,
    expansions: &'a [Vec<CallExpansion>],
    package: &mut bc::BytecodePackage,
    b: &mut Budget,
) -> Result<Vec<(&'a str, bc::SymbolId)>, Box<Error>> {
    let mut names = Vec::new();
    for (i, name) in seed.package().symbols.iter().enumerate() {
        b.push(&mut names, (name.as_str(), Some(bc::SymbolId(i as u32))))?;
    }
    for module in modules {
        for name in &module.hir().names {
            b.push(&mut names, (name.as_str(), None))?;
        }
        for name in [module.canonical_path(), module.source_path()] {
            b.push(&mut names, (name, None))?;
        }
    }
    for expansion in expansions.iter().flatten() {
        for argument in &expansion.arguments {
            b.push(&mut names, (argument.name.as_str(), None))?;
        }
    }
    for name in [
        "$module",
        "$closure",
        "$default",
        "children",
        "aside::project",
        "$initialize",
    ] {
        b.push(&mut names, (name, None))?;
    }
    build::sort_names(&mut names, b)?;
    let mut result = Vec::new();
    for &(text, seed) in &names {
        if result.last().is_some_and(|&(last, _)| last == text) {
            continue;
        }
        let symbol = if let Some(seed) = seed {
            seed
        } else {
            let symbol = bc::SymbolId(package.symbols.len() as u32);
            let owned = b.string(text)?;
            b.push(&mut package.symbols, owned)?;
            symbol
        };
        b.push(&mut result, (text, symbol))?;
    }
    b.release(names);
    Ok(result)
}
fn name(
    names: &[(&str, bc::SymbolId)],
    text: &str,
    b: &mut Budget,
) -> Result<bc::SymbolId, Box<Error>> {
    let (mut low, mut high) = (0, names.len());
    while low < high {
        let middle = (low + high) / 2;
        match b.compare(names[middle].0, text)? {
            std::cmp::Ordering::Less => low = middle + 1,
            std::cmp::Ordering::Greater => high = middle,
            std::cmp::Ordering::Equal => return Ok(names[middle].1),
        }
    }
    Err(b.error(ErrorKind::InvalidIr, "project name was not interned"))
}
fn builtin(
    p: &mut bc::BytecodePackage,
    ty: bc::BuiltinType,
    b: &mut Budget,
) -> Result<bc::TypeId, Box<Error>> {
    if let Some(index) = p.types.iter().position(|t| *t == bc::TypeDef::Builtin(ty)) {
        return Ok(bc::TypeId(index as u32));
    }
    let index = bc::TypeId(p.types.len() as u32);
    b.push(&mut p.types, bc::TypeDef::Builtin(ty))?;
    Ok(index)
}

fn links(
    m: &h::AnalyzedModule,
    module: bc::ModuleId,
    tables: LinkTables<'_>,
    natives: &[NativeLink<'_>],
    b: &mut Budget,
) -> Result<ProjectLinks, Box<Error>> {
    let LinkTables {
        index,
        bindings,
        names,
        native_exports,
        package,
    } = tables;
    let mut out = ProjectLinks {
        module,
        imports: Vec::new(),
        symbols: b.filled(m.symbols().len(), None)?,
        nodes: b.filled(m.hir().nodes.len(), None)?,
        exports: b.filled(m.symbols().len(), None)?,
        namespaces: b.filled(m.symbols().len(), false)?,
    };
    for binding in m
        .external_bindings()
        .iter()
        .filter(|binding| binding.kind == h::ExternalBindingKind::Reference)
    {
        let Some(reference) = binding.descriptor.reference.as_deref() else {
            continue;
        };
        if binding.descriptor.namespace.is_some() {
            if let Some(symbol) = binding.symbol {
                out.namespaces[symbol.0 as usize] = true;
            }
            continue;
        }
        let target = match index.binary_search_by_key(&reference.module.as_str(), |m| m.path) {
            Ok(position) => source_binding(reference, &index[position], bindings, names, b)?,
            Err(_) => {
                let Some(symbol) = binding.symbol.filter(|s| m.exports().contains(s)) else {
                    continue;
                };
                let native = natives
                    .iter()
                    .find(|n| n.target == ExternalTarget::Symbol(symbol))
                    .ok_or_else(|| {
                        b.error(
                            ErrorKind::MissingLink,
                            "reexport has no linked source or native",
                        )
                    })?;
                native_exports[native.native.index()].ok_or_else(|| {
                    b.error(
                        ErrorKind::MissingLink,
                        "native reexport has no prepared callable",
                    )
                })?
            }
        };
        if let Some(symbol) = binding.symbol {
            out.symbols[symbol.0 as usize] = Some(target);
            let local_name = name(names, m.hir().name(m.symbols()[symbol.0 as usize].name), b)?;
            let target_name =
                package.modules[target.module.index()].globals[target.global.index()].name;
            let slot = bc::GlobalId(
                package.modules[module.index()].globals.len() as u32 + out.imports.len() as u32,
            );
            b.push(
                &mut out.imports,
                bc::Import {
                    name: local_name,
                    module: target.module,
                    export: target_name,
                    content_version: package.content_version,
                },
            )?;
            out.exports[symbol.0 as usize] = Some(slot);
        }
        if let Some(node) = binding.node {
            out.nodes[node.0 as usize] = Some(target);
        }
    }
    Ok(out)
}
fn source_binding(
    reference: &SymbolReference,
    entry: &ModuleIndex<'_>,
    bindings: &[BindingIndex],
    names: &[(&str, bc::SymbolId)],
    b: &mut Budget,
) -> Result<GlobalTarget, Box<Error>> {
    if reference.version != entry.version {
        return Err(b.error(
            ErrorKind::LinkContract,
            "source import version differs from analyzed module",
        ));
    }
    let export = reference.export.as_deref().ok_or_else(|| {
        b.error(
            ErrorKind::LinkContract,
            "source value has no declaration identity",
        )
    })?;
    let symbol = name(names, export, b)?;
    b.work((usize::BITS - bindings.len().max(1).leading_zeros() + 1) as u64)?;
    bindings
        .binary_search_by_key(&(entry.module.0, symbol.0), |(key, _)| *key)
        .map(|index| bindings[index].1)
        .map_err(|_| {
            b.error(
                ErrorKind::MissingLink,
                "source import has no declared global",
            )
        })
}

fn initializer(
    p: &mut bc::BytecodePackage,
    order: &[bc::ModuleId],
    names: &[(&str, bc::SymbolId)],
    b: &mut Budget,
) -> Result<bc::FunctionId, Box<Error>> {
    let module = bc::ModuleId(p.modules.len() as u32);
    let function = bc::FunctionId(p.functions.len() as u32);
    let ty = builtin(p, bc::BuiltinType::Null, b)?;
    let signature = bc::SignatureId(p.signatures.len() as u32);
    b.push(
        &mut p.signatures,
        bc::Signature {
            parameters: Vec::new(),
            rest: None,
            return_type: ty,
            effects: bc::Effects(bc::Effects::ALL.0 & !bc::Effects::WAIT),
            property_bag_tail: None,
        },
    )?;
    let mut descriptors = Vec::new();
    let mut code = Vec::new();
    for &source in order {
        let initializer = p.modules[source.index()]
            .initializer
            .ok_or_else(|| b.error(ErrorKind::InvalidIr, "source module has no initializer"))?;
        let descriptor = descriptors.len() as u32;
        b.push(
            &mut descriptors,
            bc::Descriptor::Call(bc::CallDesc {
                target: bc::CallTarget::Function(initializer),
                arguments: bc::Arguments::Window(bc::RegWindow {
                    start: bc::Reg(0),
                    len: 0,
                }),
                names: Vec::new(),
            }),
        )?;
        b.push(
            &mut code,
            bc::Instruction::new(bc::Opcode::Call, 0, descriptor, 0),
        )?;
    }
    let constant = bc::ConstId(p.constants.len() as u32);
    b.push(&mut p.constants, bc::Const::Null)?;
    b.push(
        &mut code,
        bc::Instruction::new(bc::Opcode::LoadConst, 0, constant.0, 0),
    )?;
    b.push(&mut code, bc::Instruction::new(bc::Opcode::Return, 0, 0, 0))?;
    let function_name = name(names, "$initialize", b)?;
    let module_name = name(names, "aside::project", b)?;
    b.push(
        &mut p.functions,
        bc::FunctionDef {
            module,
            name: function_name,
            kind: bc::FunctionKind::Plain,
            signature,
            context: bc::Context::Init,
            captures: Vec::new(),
            register_count: 1,
            bindings: Vec::new(),
            descriptors,
            code,
            regions: Vec::new(),
            source_map: Vec::new(),
        },
    )?;
    b.push(
        &mut p.modules,
        bc::ModuleDef {
            path: module_name,
            imports: Vec::new(),
            exports: Vec::new(),
            globals: Vec::new(),
            initializer: Some(function),
        },
    )?;
    Ok(function)
}
