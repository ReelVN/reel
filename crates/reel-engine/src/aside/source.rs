//! Real source/compiler linkage using the same independent ABI v1 descriptions
//! as runtime admission. These functions compile; they do not execute the VM.
use super::{AdapterError, registry};
use aside_compiler::{
    AnalysisOptions, analyze_module,
    lowering::{self, ExternalTarget, Linkage, NativeLink},
    semantic::{
        CallEffect, ExternalParameter, ExternalSignature, ExternalSymbol, MemoryResolver,
        ModuleInterface, SymbolReference, TypeHint,
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

#[derive(Debug)]
pub enum SourceError {
    Source(aside_compiler::SourceTooLarge),
    Analysis(aside_compiler::semantic::AnalysisFailure),
    Lowering(lowering::Error),
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
        bc::BuiltinType::Null => TypeHint::Null,
        bc::BuiltinType::String => TypeHint::String,
        bc::BuiltinType::Array => TypeHint::Array,
        _ => unreachable!("fixed Reel registry types"),
    }
}
fn reference(contract: &registry::NativeContract) -> SymbolReference {
    SymbolReference {
        module: "reel".into(),
        version: contract.abi_version.to_string(),
        export: Some(contract.symbol.strip_prefix("reel::").unwrap().into()),
    }
}

fn seed() -> Result<bc::VerifiedProgram, bc::Error> {
    let mut package = bc::BytecodePackage::default();
    fn symbol(p: &mut bc::BytecodePackage, name: &str) -> bc::SymbolId {
        let index = p.symbols.iter().position(|s| s == name).unwrap_or_else(|| {
            p.symbols.push(name.into());
            p.symbols.len() - 1
        });
        bc::SymbolId(index as u32)
    }
    fn ty(p: &mut bc::BytecodePackage, builtin: &bc::BuiltinType) -> bc::TypeId {
        let ty = bc::TypeDef::Builtin(builtin.clone());
        let index = p.types.iter().position(|t| t == &ty).unwrap_or_else(|| {
            p.types.push(ty);
            p.types.len() - 1
        });
        bc::TypeId(index as u32)
    }
    for contract in registry::contracts() {
        let parameters = contract
            .parameters
            .iter()
            .map(|(name, builtin)| bc::Parameter {
                name: symbol(&mut package, name),
                ty: ty(&mut package, builtin),
                mutable: false,
                default: None,
            })
            .collect();
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

/// Compile a module containing a named ordinary story function. Supports the
/// formal `use reel::prelude::*` import; no permissive unknown-name resolver.
/// `content_version` is supplied by the asset/module resolver, not a save ID.
pub fn compile_story(
    source: &str,
    module_path: &str,
    content_version: u64,
    entry_name: &str,
) -> Result<CompiledStory, SourceError> {
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
                        .map(|(name, ty)| ExternalParameter {
                            name: name.clone(),
                            ty: hint(ty),
                            mutable: false,
                            default: false,
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
    for path in ["reel", "reel::prelude"] {
        resolver.insert(ModuleInterface {
            canonical_path: path.into(),
            version: "1".into(),
            exports: exports.clone(),
        });
    }
    let options = AnalysisOptions {
        module: module_path.into(),
        version: content_version.to_string(),
        ..Default::default()
    };
    let module = analyze_module(source, &options, &mut resolver)
        .map_err(SourceError::Source)?
        .into_module()
        .map_err(SourceError::Analysis)?;
    let seed = seed().map_err(SourceError::Format)?;
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
    let p = compiled.program.package();
    // The compiler initializes module function globals at runtime; their
    // GlobalDef.linked_function is intentionally None. Select the real HIR
    // declaration/output mapping, not that optional prelinked-closure field.
    let root_scope = module.facts()[0].scope.ok_or(SourceError::Entry)?;
    let declared = module.symbols().iter().any(|symbol| {
        symbol.scope == root_scope
            && module.hir().name(symbol.name) == entry_name
            && symbol.kind == aside_compiler::hir::SymbolKind::Function
    });
    if !declared {
        return Err(SourceError::Entry);
    }
    let function_index = module
        .functions()
        .iter()
        .position(|function| {
            function.parent.is_none()
                && match module.hir().node(function.node).form {
                    aside_compiler::hir::Form::Function {
                        name: Some(token), ..
                    } => token
                        .name
                        .is_some_and(|name| module.hir().name(name) == entry_name),
                    _ => false,
                }
        })
        .ok_or(SourceError::Entry)?;
    let entry = compiled.metadata.functions[function_index].output;
    let function = &p.functions[entry.index()];
    let signature = &p.signatures[function.signature.index()];
    if !function.captures.is_empty()
        || function.kind != bc::FunctionKind::Plain
        || function.context != bc::Context::Ordinary
        || signature
            .parameters
            .iter()
            .any(|parameter| parameter.default.is_none())
    {
        return Err(SourceError::Entry);
    }
    Ok(CompiledStory {
        program: compiled.program,
        initializer: compiled.entry,
        entry,
    })
}

impl From<SourceError> for AdapterError {
    fn from(error: SourceError) -> Self {
        Self::Source(error)
    }
}
