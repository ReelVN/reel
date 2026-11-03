use aside_compiler::semantic::{
    CallEffect, ExternalParameter, ExternalSignature, ExternalSymbol, SymbolReference, TypeHint,
};
use aside_vm::bytecode::{
    self as bc, BuiltinType, Effects, NativeSupport, Parameter, RuntimeSupport, Signature,
    SymbolId, TypeDef, TypeId,
};
pub use aside_vm::{HostMethodSupport, HostTypeSupport};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeclarationError(pub &'static str);
impl std::fmt::Display for DeclarationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for DeclarationError {}
/// Host-owned metadata, supplied independently of the package being admitted.
/// No package Signature/TypeId/FunctionId can be imported as a trust assertion.
/// Parameter defaults reference shared immutable bytecode constants. Runtime
/// admission verifies these values against the same independent declarations.
/// Dependent script defaults retain their ordinary lexical function semantics.
#[derive(Clone, Debug)]
pub struct NativeParameter {
    pub name: String,
    pub ty: BuiltinType,
    pub default: Option<bc::Const>,
}
#[derive(Clone, Debug)]
pub struct NativeContract {
    pub symbol: String,
    pub abi_version: u32,
    pub parameters: Vec<NativeParameter>,
    /// Name of the fresh array containing remaining positional arguments.
    pub rest: Option<String>,
    pub result: BuiltinType,
    pub effects: Effects,
    pub required_capabilities: Vec<String>,
    /// Trusted scalar computation executed within the VM's work budget.
    pub read_only_leaf: Option<bc::ReadOnlyLeaf>,
    /// Opt into the VM's standard scheduled derived-commit continuation.
    /// Uses the atomic host-operation cost documented by `NativeSupport`.
    pub standard_derived_commit: bool,
}

impl NativeContract {
    pub fn reference(&self) -> SymbolReference {
        reference(self)
    }
    pub fn external(&self) -> ExternalSymbol {
        let reference = self.reference();
        ExternalSymbol::function(
            reference.export.as_ref().unwrap(),
            ExternalSignature {
                parameters: self
                    .parameters
                    .iter()
                    .map(|p| ExternalParameter {
                        name: p.name.clone(),
                        ty: hint(&p.ty),
                        mutable: false,
                        default: p.default.is_some(),
                        rest: false,
                    })
                    .chain(self.rest.iter().map(|name| ExternalParameter {
                        name: name.clone(),
                        ty: hint(&BuiltinType::Array),
                        mutable: false,
                        default: false,
                        rest: true,
                    }))
                    .collect(),
                result: hint(&self.result),
                result_cases: Vec::new(),
                result_parameter: None,
                effect: if self.effects.0 & Effects::WAIT == 0 {
                    CallEffect::NoWait
                } else {
                    CallEffect::MayWait
                },
                init_safe: Some(
                    self.effects.0 & (Effects::HOST | Effects::WAIT | Effects::WRITE) == 0,
                ),
                derived_safe: Some(
                    self.effects.0 & (Effects::HOST | Effects::WAIT | Effects::WRITE) == 0,
                ),
            },
        )
        .with_reference(reference)
    }
}

#[derive(Debug)]
pub enum BuildError {
    Format(bc::Error),
    Lowering(Box<aside_compiler::lowering::Error>),
}
impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for BuildError {}

/// Borrowed host compile plans, one set per analyzed module. Empty plans leave
/// ordinary source lowering unchanged. Single-module compilation accepts at
/// most one set; project compilation requires one set per module when present.
#[derive(Default)]
pub struct CompileOptions<'a> {
    pub lowering: aside_compiler::lowering::Options,
    pub expansions: &'a [Vec<aside_compiler::lowering::CallExpansion>],
}
impl From<aside_compiler::lowering::Options> for CompileOptions<'_> {
    fn from(lowering: aside_compiler::lowering::Options) -> Self {
        Self {
            lowering,
            expansions: &[],
        }
    }
}

/// Lower one analyzed module using independently registered native source types,
/// runtime ABI contracts and host nominal declarations.
pub fn lower(
    module: &aside_compiler::hir::AnalyzedModule,
    contracts: &[NativeContract],
    source_declarations: &[crate::source::Declaration],
    types: &[HostTypeSupport],
    capabilities: &[String],
    options: CompileOptions<'_>,
) -> Result<aside_compiler::lowering::Compilation, BuildError> {
    use aside_compiler::lowering::{self, ExternalTarget, Linkage, NativeLink};
    if options.expansions.len() > 1 {
        return Err(BuildError::Lowering(Box::new(lowering::Error {
            kind: lowering::ErrorKind::LinkContract,
            span: module.hir().node(aside_compiler::hir::NodeId(0)).span,
            detail: "single-module compilation accepts one expansion set".into(),
            stats: Default::default(),
        })));
    }
    let expansions = options.expansions.first().map_or(&[][..], Vec::as_slice);
    let options = options.lowering;
    let seed = seed(contracts, types, capabilities, options.content_version)
        .map_err(BuildError::Format)?;
    let references = contracts
        .iter()
        .map(NativeContract::reference)
        .collect::<Vec<_>>();
    let mut links = Vec::new();
    for binding in module
        .external_bindings()
        .iter()
        .filter(|binding| binding.kind == aside_compiler::hir::ExternalBindingKind::Reference)
    {
        let Some(reference) = binding.descriptor.reference.as_deref() else {
            continue;
        };
        let Some(index) = references.iter().position(|r| r == reference) else {
            continue;
        };
        let target = if let Some(symbol) = binding.symbol {
            ExternalTarget::Symbol(symbol)
        } else if let Some(node) = binding.node {
            ExternalTarget::Node(node)
        } else {
            continue;
        };
        links.push(NativeLink {
            target,
            native: bc::NativeId(index as u32),
            reference: &references[index],
            source_signature: source_declarations
                .iter()
                .find(|declaration| {
                    declaration.symbol.reference.as_deref() == Some(&references[index])
                })
                .and_then(|declaration| declaration.symbol.signature.as_ref()),
        });
    }
    lowering::compile(
        module,
        &Linkage {
            package: Some(&seed),
            natives: &links,
            expansions,
        },
        options,
    )
    .map_err(BuildError::Lowering)
}

/// Lower a source graph using one trusted source/ABI registration, nominal seed
/// and final B01 verification. Each analyzed module is released after lowering.
pub fn lower_project(
    modules: Vec<aside_compiler::hir::AnalyzedModule>,
    plan: &aside_compiler::semantic::ModulePlan,
    contracts: &[NativeContract],
    source_declarations: &[crate::source::Declaration],
    types: &[HostTypeSupport],
    capabilities: &[String],
    options: CompileOptions<'_>,
) -> Result<aside_compiler::lowering::ProjectCompilation, aside_compiler::lowering::ProjectError> {
    lower_project_with_progress(
        modules,
        plan,
        contracts,
        source_declarations,
        types,
        capabilities,
        options,
        &mut |_| {},
    )
}

/// Run the same pipeline while reporting completed stages and modules.
#[allow(clippy::too_many_arguments)]
pub fn lower_project_with_progress(
    modules: Vec<aside_compiler::hir::AnalyzedModule>,
    plan: &aside_compiler::semantic::ModulePlan,
    contracts: &[NativeContract],
    source_declarations: &[crate::source::Declaration],
    types: &[HostTypeSupport],
    capabilities: &[String],
    options: CompileOptions<'_>,
    progress: &mut dyn FnMut(aside_compiler::Progress),
) -> Result<aside_compiler::lowering::ProjectCompilation, aside_compiler::lowering::ProjectError> {
    use aside_compiler::lowering::{self, ExternalTarget, NativeLink};
    let expansions = options.expansions;
    let options = options.lowering;
    let seed = seed(contracts, types, capabilities, options.content_version).map_err(|error| {
        lowering::ProjectError {
            module: "<host>".into(),
            error: Box::new(lowering::Error {
                kind: lowering::ErrorKind::Bytecode,
                span: aside_compiler::TextRange::empty(0.into()),
                detail: error.to_string(),
                stats: Default::default(),
            }),
        }
    })?;
    let references = contracts
        .iter()
        .map(NativeContract::reference)
        .collect::<Vec<_>>();
    let links = modules
        .iter()
        .map(|module| {
            module
                .external_bindings()
                .iter()
                .filter(|binding| {
                    binding.kind == aside_compiler::hir::ExternalBindingKind::Reference
                })
                .filter_map(|binding| {
                    let reference = binding.descriptor.reference.as_deref()?;
                    let index = references.iter().position(|r| r == reference)?;
                    let target = binding
                        .symbol
                        .map(ExternalTarget::Symbol)
                        .or_else(|| binding.node.map(ExternalTarget::Node))?;
                    Some(NativeLink {
                        target,
                        native: bc::NativeId(index as u32),
                        reference: &references[index],
                        source_signature: source_declarations
                            .iter()
                            .find(|declaration| {
                                declaration.symbol.reference.as_deref() == Some(&references[index])
                            })
                            .and_then(|declaration| declaration.symbol.signature.as_ref()),
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    lowering::compile_project_with_progress(
        modules, plan, &seed, &links, expansions, options, progress,
    )
}

/// Resolve independently declared names/types into this package's indices.
/// The VM subsequently checks exact metadata, including mutability/defaults,
/// effects and required capabilities, against every NativeDef.
pub fn runtime_support(
    program: &bc::VerifiedProgram,
    contracts: &[NativeContract],
    types: &[HostTypeSupport],
    capabilities: &[String],
) -> Result<RuntimeSupport, DeclarationError> {
    program
        .require_text_pool()
        .map_err(|_| DeclarationError("native defaults require a bound text pool"))?;
    let package = program.package();
    let mut natives = Vec::new();
    for (index, contract) in contracts.iter().enumerate() {
        if contracts[..index]
            .iter()
            .any(|other| other.symbol == contract.symbol)
        {
            return Err(DeclarationError("duplicate trusted native"));
        }
        // Unused host natives need not have names/types in a smaller package.
        let Some(native) = package
            .natives
            .iter()
            .find(|native| package.symbols.get(native.symbol.index()) == Some(&contract.symbol))
        else {
            continue;
        };
        let declared = &package.signatures[native.signature.index()];
        let symbol = |name: &str| {
            package
                .symbols
                .iter()
                .position(|s| s == name)
                .map(|i| SymbolId(i as u32))
                .ok_or(DeclarationError("native parameter name missing"))
        };
        let ty = |builtin: &BuiltinType| {
            package
                .types
                .iter()
                .position(|t| t == &TypeDef::Builtin(*builtin))
                .map(|i| TypeId(i as u32))
                .ok_or(DeclarationError("native builtin type missing"))
        };
        let parameters = contract
            .parameters
            .iter()
            .enumerate()
            .map(|(index, parameter)| {
                Ok(Parameter {
                    name: symbol(&parameter.name)?,
                    ty: ty(&parameter.ty)?,
                    mutable: false,
                    default: parameter
                        .default
                        .as_ref()
                        .map(|value| default_value(program, declared, index, value))
                        .transpose()?,
                })
            })
            .collect::<Result<Vec<_>, DeclarationError>>()?;
        natives.push(NativeSupport {
            symbol: contract.symbol.clone(),
            abi_version: contract.abi_version,
            standard_derived_commit: contract.standard_derived_commit,
            read_only_leaf: contract.read_only_leaf,
            signature: Signature {
                parameters,
                rest: contract
                    .rest
                    .as_ref()
                    .map(|name| {
                        Ok(Parameter {
                            name: symbol(name)?,
                            ty: ty(&BuiltinType::Array)?,
                            mutable: false,
                            default: None,
                        })
                    })
                    .transpose()?,
                return_type: ty(&contract.result)?,
                effects: contract.effects,
                property_bag_tail: None,
            },
            required_capabilities: contract.required_capabilities.clone(),
        });
    }
    Ok(RuntimeSupport {
        opcodes: Vec::new(),
        capabilities: capabilities.to_vec(),
        natives,
        host_types: types.to_vec(),
    })
}

fn default_value(
    program: &bc::VerifiedProgram,
    signature: &bc::Signature,
    index: usize,
    value: &bc::Const,
) -> Result<bc::ParameterDefault, DeclarationError> {
    let package = program.package();
    let default = signature
        .parameters
        .get(index)
        .and_then(|p| p.default)
        .ok_or(DeclarationError("native default missing"))?;
    let bc::ParameterDefault::Constant(id) = default else {
        return Err(DeclarationError(
            "native literal default must reference a constant",
        ));
    };
    let valid = match (package.constants.get(id.index()), value) {
        (Some(bc::Const::Utf8(_) | bc::Const::Text { .. }), bc::Const::Utf8(expected)) => {
            program.constant_bytes(id) == Some(expected.as_bytes())
        }
        (actual, _) => actual == Some(value),
    };
    if !valid {
        return Err(DeclarationError("native default differs from registry"));
    }
    Ok(default)
}
pub fn hint(ty: &bc::BuiltinType) -> TypeHint {
    match ty {
        bc::BuiltinType::Any => TypeHint::Any,
        bc::BuiltinType::Bool => TypeHint::Bool,
        bc::BuiltinType::Null => TypeHint::Null,
        bc::BuiltinType::String => TypeHint::String,
        bc::BuiltinType::Array => TypeHint::Array,
        bc::BuiltinType::Int => TypeHint::Int,
        bc::BuiltinType::Float => TypeHint::Float,
        bc::BuiltinType::Table => TypeHint::Table,
        bc::BuiltinType::Closure => TypeHint::Closure,
        bc::BuiltinType::Future => TypeHint::Future,
        bc::BuiltinType::Generator => TypeHint::Generator,
        bc::BuiltinType::PropertyRef => TypeHint::Named("Binding".into()),
        bc::BuiltinType::Tuple => TypeHint::Named("tuple".into()),
        _ => TypeHint::Any,
    }
}
pub fn reference(contract: &NativeContract) -> SymbolReference {
    let (module, export) = contract.symbol.rsplit_once("::").unwrap();
    SymbolReference {
        module: module.into(),
        version: contract.abi_version.to_string(),
        export: Some(export.into()),
    }
}

/// Build shared constant defaults and canonical nominal types from trusted Rust
/// declarations. Runtime loading validates nominal registration relationships.
pub fn seed(
    contracts: &[NativeContract],
    types: &[HostTypeSupport],
    capabilities: &[String],
    content_version: u64,
) -> Result<bc::VerifiedProgram, bc::Error> {
    use std::collections::HashMap;
    let mut package = bc::BytecodePackage {
        content_version,
        ..Default::default()
    };
    let mut symbols = HashMap::new();
    let mut defaults = HashMap::new();
    fn symbol<'a>(
        p: &mut bc::BytecodePackage,
        index: &mut HashMap<&'a str, bc::SymbolId>,
        name: &'a str,
    ) -> bc::SymbolId {
        *index.entry(name).or_insert_with(|| {
            let id = bc::SymbolId(p.symbols.len() as u32);
            p.symbols.push(name.into());
            id
        })
    }
    fn ty(p: &mut bc::BytecodePackage, builtin: &bc::BuiltinType) -> bc::TypeId {
        let ty = bc::TypeDef::Builtin(*builtin);
        let index = p.types.iter().position(|t| t == &ty).unwrap_or_else(|| {
            p.types.push(ty);
            p.types.len() - 1
        });
        bc::TypeId(index as u32)
    }
    let defaults_path = symbol(&mut package, &mut symbols, "aside::host_defaults");
    package.modules.push(bc::ModuleDef {
        path: defaults_path,
        imports: vec![],
        exports: vec![],
        globals: vec![],
        initializer: None,
    });
    for declaration in types {
        for method in &declaration.methods {
            symbol(&mut package, &mut symbols, &method.name);
        }
        let (path, name) = declaration
            .name
            .rsplit_once("::")
            .ok_or_else(|| bc::Error {
                kind: bc::ErrorKind::Signature,
                offset: None,
                function: None,
                pc: None,
                source: None,
                detail: "host type requires canonical module identity".into(),
            })?;
        let path_symbol = symbol(&mut package, &mut symbols, path);
        let module = package
            .modules
            .iter()
            .position(|module| module.path == path_symbol)
            .unwrap_or_else(|| {
                package.modules.push(bc::ModuleDef {
                    path: path_symbol,
                    imports: vec![],
                    exports: vec![],
                    globals: vec![],
                    initializer: None,
                });
                package.modules.len() - 1
            });
        let name = symbol(&mut package, &mut symbols, name);
        package.types.push(TypeDef::Named {
            module: bc::ModuleId(module as u32),
            name,
            version: content_version,
        });
    }
    for contract in contracts {
        let mut parameters: Vec<bc::Parameter> = Vec::new();
        for parameter in &contract.parameters {
            let parameter_type = ty(&mut package, &parameter.ty);
            let default = parameter.default.as_ref().map(|value| {
                bc::ParameterDefault::Constant(crate::literal_defaults::intern(
                    &mut package,
                    &mut defaults,
                    value,
                ))
            });
            parameters.push(bc::Parameter {
                name: symbol(&mut package, &mut symbols, &parameter.name),
                ty: parameter_type,
                mutable: false,
                default,
            });
        }
        let result = ty(&mut package, &contract.result);
        let rest = contract.rest.as_ref().map(|name| bc::Parameter {
            name: symbol(&mut package, &mut symbols, name),
            ty: ty(&mut package, &BuiltinType::Array),
            mutable: false,
            default: None,
        });
        let signature = bc::SignatureId(package.signatures.len() as u32);
        package.signatures.push(bc::Signature {
            parameters,
            rest,
            return_type: result,
            effects: contract.effects,
            property_bag_tail: None,
        });
        let name = symbol(&mut package, &mut symbols, &contract.symbol);
        let capabilities = contract
            .required_capabilities
            .iter()
            .map(|name| symbol(&mut package, &mut symbols, name))
            .collect();
        // Host objects can bind methods even when source never reads the native
        // name. The compiler reuses these same callable definitions.
        package.functions.push(bc::FunctionDef {
            module: bc::ModuleId(0),
            name,
            kind: bc::FunctionKind::Native(bc::NativeId(package.natives.len() as u32)),
            signature,
            context: bc::Context::Ordinary,
            captures: vec![],
            register_count: 0,
            bindings: vec![],
            descriptors: vec![],
            code: vec![],
            regions: vec![],
            source_map: vec![],
        });
        package.natives.push(bc::NativeDef {
            symbol: name,
            signature,
            abi_version: contract.abi_version,
            capabilities,
        });
    }
    package.capabilities = capabilities
        .iter()
        .map(|name| symbol(&mut package, &mut symbols, name))
        .collect();
    bc::verify(package, bc::Limits::default())
}
