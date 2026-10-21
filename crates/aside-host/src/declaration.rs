use aside_compiler::semantic::{
    CallEffect, ExternalParameter, ExternalSignature, ExternalSymbol, SymbolReference, TypeHint,
};
pub use aside_vm::HostTypeSupport;
use aside_vm::bytecode::{
    self as bc, BuiltinType, Effects, NativeSupport, Parameter, RuntimeSupport, Signature,
    SymbolId, TypeDef, TypeId,
};

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
/// Parameter defaults are ordinary bytecode default functions generated from these
/// host-owned constants. Runtime admission resolves the same declarations.
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

/// Lower one analyzed module using independently registered native source types,
/// runtime ABI contracts and host nominal declarations.
pub fn lower(
    module: &aside_compiler::hir::AnalyzedModule,
    contracts: &[NativeContract],
    source_declarations: &[crate::source::Declaration],
    types: &[HostTypeSupport],
    capabilities: &[String],
    options: aside_compiler::lowering::Options,
) -> Result<aside_compiler::lowering::Compilation, BuildError> {
    use aside_compiler::lowering::{self, ExternalTarget, Linkage, NativeLink};
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
        },
        options,
    )
    .map_err(BuildError::Lowering)
}

/// Lower a source graph using one trusted source/ABI registration, nominal seed
/// and final B01 verification.
pub fn lower_project(
    modules: &[aside_compiler::hir::AnalyzedModule],
    plan: &aside_compiler::semantic::ModulePlan,
    contracts: &[NativeContract],
    source_declarations: &[crate::source::Declaration],
    types: &[HostTypeSupport],
    capabilities: &[String],
    options: aside_compiler::lowering::Options,
) -> Result<aside_compiler::lowering::ProjectCompilation, aside_compiler::lowering::ProjectError> {
    use aside_compiler::lowering::{self, ExternalTarget, NativeLink};
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
    lowering::compile_project(modules, plan, &seed, &links, options)
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
        if !package
            .natives
            .iter()
            .any(|native| package.symbols.get(native.symbol.index()) == Some(&contract.symbol))
        {
            continue;
        }
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
                        .map(|value| default_function(program, contract, index, value))
                        .transpose()?,
                })
            })
            .collect::<Result<Vec<_>, DeclarationError>>()?;
        natives.push(NativeSupport {
            symbol: contract.symbol.clone(),
            abi_version: contract.abi_version,
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

pub fn default_name(contract: &NativeContract, index: usize) -> String {
    format!("{}::default::{index}", contract.symbol)
}

fn default_function(
    program: &bc::VerifiedProgram,
    contract: &NativeContract,
    index: usize,
    value: &bc::Const,
) -> Result<bc::FunctionId, DeclarationError> {
    let package = program.package();
    let name = default_name(contract, index);
    let (id, function) = package
        .functions
        .iter()
        .enumerate()
        .find(|(_, function)| package.symbols[function.name.index()] == name)
        .ok_or(DeclarationError("native default missing"))?;
    // A package cannot replace the registered constant default with script effects.
    let valid = function.context == bc::Context::NoWait
        && function.captures.is_empty()
        && function.code.len() == 2
        && function.code[0].opcode == bc::Opcode::LoadConst as u16
        && function.code[1] == bc::Instruction::new(bc::Opcode::Return, index as u32, 0, 0)
        && function.code[0].operands[0] == index as u32
        && {
            let id = bc::ConstId(function.code[0].operands[1]);
            match (package.constants.get(id.index()), value) {
                (Some(bc::Const::Utf8(_) | bc::Const::Text { .. }), bc::Const::Utf8(expected)) => {
                    program.constant_bytes(id) == Some(expected.as_bytes())
                }
                (actual, _) => actual == Some(value),
            }
        };
    if !valid {
        return Err(DeclarationError("native default differs from registry"));
    }
    Ok(bc::FunctionId(id as u32))
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

/// Build verified native defaults and canonical nominal types from trusted Rust
/// declarations. Runtime loading validates nominal registration relationships.
pub fn seed(
    contracts: &[NativeContract],
    types: &[HostTypeSupport],
    capabilities: &[String],
    content_version: u64,
) -> Result<bc::VerifiedProgram, bc::Error> {
    let mut package = bc::BytecodePackage {
        content_version,
        ..Default::default()
    };
    fn symbol(p: &mut bc::BytecodePackage, name: &str) -> bc::SymbolId {
        let index = p.symbols.iter().position(|s| s == name).unwrap_or_else(|| {
            p.symbols.push(name.into());
            p.symbols.len() - 1
        });
        bc::SymbolId(index as u32)
    }
    fn ty(p: &mut bc::BytecodePackage, builtin: &bc::BuiltinType) -> bc::TypeId {
        let ty = bc::TypeDef::Builtin(*builtin);
        let index = p.types.iter().position(|t| t == &ty).unwrap_or_else(|| {
            p.types.push(ty);
            p.types.len() - 1
        });
        bc::TypeId(index as u32)
    }
    let defaults_path = symbol(&mut package, "aside::host_defaults");
    package.modules.push(bc::ModuleDef {
        path: defaults_path,
        imports: vec![],
        exports: vec![],
        globals: vec![],
        initializer: None,
    });
    for declaration in types {
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
        let path_symbol = symbol(&mut package, path);
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
        let name = symbol(&mut package, name);
        package.types.push(TypeDef::Named {
            module: bc::ModuleId(module as u32),
            name,
            version: content_version,
        });
    }
    for contract in contracts {
        let mut parameters: Vec<bc::Parameter> = Vec::new();
        for (index, parameter) in contract.parameters.iter().enumerate() {
            let parameter_type = ty(&mut package, &parameter.ty);
            let default = if let Some(value) = &parameter.default {
                let constant = package.constants.len() as u32;
                package.constants.push(value.clone());
                let name = symbol(&mut package, &default_name(contract, index));
                let signature = bc::SignatureId(package.signatures.len() as u32);
                package.signatures.push(bc::Signature {
                    parameters: parameters
                        .iter()
                        .cloned()
                        .map(|mut p| {
                            p.default = None;
                            p
                        })
                        .collect(),
                    rest: None,
                    return_type: parameter_type,
                    effects: bc::Effects(0),
                    property_bag_tail: None,
                });
                let function = bc::FunctionId(package.functions.len() as u32);
                package.functions.push(bc::FunctionDef {
                    module: bc::ModuleId(0),
                    name,
                    kind: bc::FunctionKind::Plain,
                    signature,
                    context: bc::Context::NoWait,
                    captures: vec![],
                    register_count: index as u32 + 1,
                    bindings: parameters
                        .iter()
                        .enumerate()
                        .map(|(i, p)| bc::BindingDef {
                            name: p.name,
                            ty: p.ty,
                            mutable: false,
                            storage: bc::BindingStorage::Value(bc::Reg(i as u32)),
                            initialization: bc::BindingInitialization::Parameter(i as u32),
                        })
                        .collect(),
                    descriptors: vec![],
                    regions: vec![],
                    source_map: vec![],
                    code: vec![
                        bc::Instruction::new(bc::Opcode::LoadConst, index as u32, constant, 0),
                        bc::Instruction::new(bc::Opcode::Return, index as u32, 0, 0),
                    ],
                });
                Some(function)
            } else {
                None
            };
            parameters.push(bc::Parameter {
                name: symbol(&mut package, &parameter.name),
                ty: parameter_type,
                mutable: false,
                default,
            });
        }
        let result = ty(&mut package, &contract.result);
        let rest = contract.rest.as_ref().map(|name| bc::Parameter {
            name: symbol(&mut package, name),
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
        let name = symbol(&mut package, &contract.symbol);
        let capabilities = contract
            .required_capabilities
            .iter()
            .map(|name| symbol(&mut package, name))
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
        .map(|name| symbol(&mut package, name))
        .collect();
    bc::verify(package, bc::Limits::default())
}
