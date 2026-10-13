//! Real source/compiler linkage using the same independent ABI v1 descriptions
//! as runtime admission. These functions compile; they do not execute the VM.
use super::{AdapterError, registry};
use aside_compiler::{
    AnalysisOptions, analyze_module,
    hir::AnalyzedModule,
    lowering::{self, ExternalTarget, Linkage, NativeLink},
    semantic::{
        CallEffect, ExternalParameter, ExternalSignature, ExternalSymbol, MemoryResolver,
        ModuleInterface, NamespaceRef, SymbolReference, TypeHint,
    },
};
use aside_vm::bytecode as bc;
pub use bc::FunctionId as StoryFunction;

pub struct CompiledStory {
    pub program: bc::VerifiedProgram,
    /// Run the real emitted module initializer before the selected story entry.
    pub initializer: bc::FunctionId,
    pub entry: bc::FunctionId,
}

impl CompiledStory {
    /// Validate host entry metadata in addition to B01 verification. Native
    /// admission remains exclusively the responsibility of SessionVm::load.
    pub fn validate(&self) -> Result<(), SourceError> {
        let p = self.program.package();
        let function = p
            .functions
            .get(self.entry.index())
            .ok_or(SourceError::Entry)?;
        let signature = &p.signatures[function.signature.index()];
        if !function.captures.is_empty()
            || function.kind != bc::FunctionKind::Plain
            || function.context != bc::Context::Ordinary
            || signature
                .parameters
                .iter()
                .any(|parameter| parameter.default.is_none())
            || p.modules[function.module.index()].initializer != Some(self.initializer)
        {
            return Err(SourceError::Entry);
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum SourceError {
    Source(aside_compiler::SourceTooLarge),
    Analysis(aside_compiler::semantic::AnalysisFailure),
    Lowering(Box<lowering::Error>),
    Format(bc::Error),
    Entry,
}
impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for SourceError {}

fn hint(ty: &bc::BuiltinType) -> TypeHint {
    match ty {
        bc::BuiltinType::Any => TypeHint::Any,
        bc::BuiltinType::Bool => TypeHint::Bool,
        bc::BuiltinType::Null => TypeHint::Null,
        bc::BuiltinType::String => TypeHint::String,
        bc::BuiltinType::Array => TypeHint::Array,
        _ => unreachable!("fixed Reel registry types"),
    }
}
fn reference(contract: &registry::NativeContract) -> SymbolReference {
    let (module, export) = contract.symbol.rsplit_once("::").unwrap();
    SymbolReference {
        module: module.into(),
        version: contract.abi_version.to_string(),
        export: Some(export.into()),
    }
}

fn seed(content_version: u64) -> Result<bc::VerifiedProgram, bc::Error> {
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
    let defaults_path = symbol(&mut package, "reel::defaults");
    package.modules.push(bc::ModuleDef {
        path: defaults_path,
        imports: vec![],
        exports: vec![],
        globals: vec![],
        initializer: None,
    });
    for contract in registry::contracts() {
        let mut parameters: Vec<bc::Parameter> = Vec::new();
        for (index, parameter) in contract.parameters.iter().enumerate() {
            let parameter_type = ty(&mut package, &parameter.ty);
            let default = if let Some(value) = &parameter.default {
                let constant = package.constants.len() as u32;
                package.constants.push(value.clone());
                let name = symbol(&mut package, &registry::default_name(&contract, index));
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
        let signature = bc::SignatureId(package.signatures.len() as u32);
        package.signatures.push(bc::Signature {
            parameters,
            rest: None,
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
        package.natives.push(bc::NativeDef {
            symbol: name,
            signature,
            abi_version: contract.abi_version,
            capabilities,
        });
    }
    package.capabilities = registry::capabilities()
        .iter()
        .map(|name| symbol(&mut package, name))
        .collect();
    bc::verify(package, bc::Limits::default())
}

/// One source analysis and its B01 lowering, shared with author tools.
pub struct SourceCompilation {
    pub module: AnalyzedModule,
    pub compilation: lowering::Compilation,
}

/// The author CLI and runtime share this resolver and trusted native registry.
/// `content_version` is supplied by the asset/module resolver, not a save ID.
pub fn compile_module(
    source: &str,
    module_path: &str,
    source_path: &str,
    content_version: u64,
) -> Result<SourceCompilation, SourceError> {
    let contracts = registry::contracts();
    let references = contracts.iter().map(reference).collect::<Vec<_>>();
    let exports = contracts
        .iter()
        .zip(&references)
        .map(|(contract, reference)| {
            ExternalSymbol::function(
                reference.export.as_ref().unwrap(),
                ExternalSignature {
                    parameters: contract
                        .parameters
                        .iter()
                        .map(|parameter| ExternalParameter {
                            name: parameter.name.clone(),
                            ty: hint(&parameter.ty),
                            mutable: false,
                            default: parameter.default.is_some(),
                            rest: false,
                        })
                        .collect(),
                    result: hint(&contract.result),
                    effect: if contract.effects.0 & bc::Effects::WAIT != 0 {
                        CallEffect::MayWait
                    } else {
                        CallEffect::NoWait
                    },
                    init_safe: Some(false),
                    derived_safe: Some(false),
                },
            )
            .with_reference(reference.clone())
        })
        .collect::<Vec<_>>();
    let mut resolver = MemoryResolver::default();
    let mut prelude = exports
        .iter()
        .zip(&references)
        .filter(|(_, reference)| reference.module == "reel")
        .map(|(export, _)| export.clone())
        .collect::<Vec<_>>();
    let mut stage = ExternalSymbol::dynamic("stage");
    stage.namespace = Some(NamespaceRef {
        module: "reel::stage".into(),
        version: "1".into(),
    });
    prelude.push(stage);
    for path in ["reel", "reel::prelude"] {
        resolver.insert(ModuleInterface {
            canonical_path: path.into(),
            version: "1".into(),
            exports: prelude.clone(),
        });
    }
    resolver.insert(ModuleInterface {
        canonical_path: "reel::stage".into(),
        version: "1".into(),
        exports: exports
            .iter()
            .zip(&references)
            .filter(|(_, reference)| reference.module == "reel::stage")
            .map(|(export, _)| export.clone())
            .collect(),
    });
    let options = AnalysisOptions {
        module: module_path.into(),
        source_path: Some(source_path.into()),
        version: content_version.to_string(),
        hosts: prelude,
        ..Default::default()
    };
    let module = analyze_module(source, &options, &mut resolver)
        .map_err(SourceError::Source)?
        .into_module()
        .map_err(SourceError::Analysis)?;
    let seed = seed(content_version).map_err(SourceError::Format)?;
    let mut links = Vec::new();
    for binding in module.external_bindings() {
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
        });
    }
    let compiled = lowering::compile(
        &module,
        &Linkage {
            package: Some(&seed),
            natives: &links,
        },
        lowering::Options {
            content_version,
            ..Default::default()
        },
    )
    .map_err(SourceError::Lowering)?;
    Ok(SourceCompilation {
        module,
        compilation: compiled,
    })
}

pub fn compile_story(
    source: &str,
    module_path: &str,
    content_version: u64,
    entry_name: &str,
) -> Result<CompiledStory, SourceError> {
    let SourceCompilation {
        module,
        compilation: compiled,
    } = compile_module(source, module_path, module_path, content_version)?;
    // The compiler initializes module function globals at runtime; their
    // GlobalDef.linked_function is intentionally None. Select the real HIR
    // declaration/output mapping, not that optional prelinked-closure field.
    let root_scope = module.facts()[0].scope.ok_or(SourceError::Entry)?;
    let declared = module
        .symbols()
        .iter()
        .position(|symbol| {
            symbol.scope == root_scope
                && module.hir().name(symbol.name) == entry_name
                && symbol.kind == aside_compiler::hir::SymbolKind::Function
        })
        .map(|index| aside_compiler::hir::SymbolId(index as u32))
        .ok_or(SourceError::Entry)?;
    let function_index = module
        .functions()
        .iter()
        .position(|function| module.facts()[function.node.0 as usize].symbol == Some(declared))
        .ok_or(SourceError::Entry)?;
    let entry = compiled.metadata.functions[function_index].output;
    let story = CompiledStory {
        program: compiled.program,
        initializer: compiled.entry,
        entry,
    };
    story.validate()?;
    Ok(story)
}

impl From<SourceError> for AdapterError {
    fn from(error: SourceError) -> Self {
        Self::Source(error)
    }
}
