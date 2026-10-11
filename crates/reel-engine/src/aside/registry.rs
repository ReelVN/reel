use aside_vm::bytecode::{
    BuiltinType, BytecodePackage, Effects, NativeSupport, Parameter, RuntimeSupport, Signature,
    SymbolId, TypeDef, TypeId,
};

use super::AdapterError;

/// Host-owned metadata, supplied independently of the package being admitted.
/// No package Signature/TypeId/FunctionId can be imported as a trust assertion.
/// This first adapter supports required builtin-typed parameters, no defaults,
/// rest, property tail or script callbacks. Other contracts fail admission.
#[derive(Clone, Debug)]
pub(super) struct NativeContract {
    pub symbol: String,
    pub abi_version: u32,
    pub parameters: Vec<(String, BuiltinType)>,
    pub result: BuiltinType,
    pub effects: Effects,
    pub required_capabilities: Vec<String>,
}

/// Resolve independently declared names/types into this package's indices.
/// The VM subsequently checks exact metadata, including mutability/defaults,
/// effects and required capabilities, against every NativeDef.
pub(super) fn runtime_support(
    package: &BytecodePackage,
    contracts: &[NativeContract],
    capabilities: &[String],
) -> Result<RuntimeSupport, AdapterError> {
    let mut natives = Vec::new();
    for (index, contract) in contracts.iter().enumerate() {
        if contracts[..index]
            .iter()
            .any(|other| other.symbol == contract.symbol)
        {
            return Err(AdapterError::Input("duplicate trusted native"));
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
                .ok_or(AdapterError::Input("native parameter name missing"))
        };
        let ty = |builtin: &BuiltinType| {
            package
                .types
                .iter()
                .position(|t| t == &TypeDef::Builtin(*builtin))
                .map(|i| TypeId(i as u32))
                .ok_or(AdapterError::Input("native builtin type missing"))
        };
        let parameters = contract
            .parameters
            .iter()
            .map(|(name, builtin)| {
                Ok(Parameter {
                    name: symbol(name)?,
                    ty: ty(builtin)?,
                    mutable: false,
                    default: None,
                })
            })
            .collect::<Result<Vec<_>, AdapterError>>()?;
        natives.push(NativeSupport {
            symbol: contract.symbol.clone(),
            abi_version: contract.abi_version,
            signature: Signature {
                parameters,
                rest: None,
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
    })
}

pub(super) fn contracts() -> Vec<NativeContract> {
    [
        ("narrator", "text", BuiltinType::String, BuiltinType::Null),
        ("choose", "options", BuiltinType::Array, BuiltinType::String),
    ]
    .into_iter()
    .map(|(name, parameter, input, result)| NativeContract {
        symbol: format!("reel::{name}"),
        abi_version: 1,
        parameters: vec![(parameter.into(), input)],
        result,
        effects: Effects::ALL,
        required_capabilities: capabilities(),
    })
    .collect()
}

pub(super) fn capabilities() -> Vec<String> {
    vec!["reel.session".into()]
}
