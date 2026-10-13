use aside_vm::bytecode::{
    self as bc, BuiltinType, BytecodePackage, Effects, NativeSupport, Parameter, RuntimeSupport,
    Signature, SymbolId, TypeDef, TypeId,
};

use super::AdapterError;

pub(super) const NODE_FIELDS: [&str; 17] = [
    "x",
    "y",
    "width",
    "height",
    "opacity",
    "visible",
    "clip",
    "gap",
    "padding",
    "font_size",
    "color",
    "background",
    "rotation",
    "scale_x",
    "scale_y",
    "anchor_x",
    "anchor_y",
];

pub(super) fn node_prefix(symbol: &str) -> &'static [&'static str] {
    match symbol {
        "reel::text" => &["value"],
        "reel::image" => &["source", "fit"],
        _ => &[],
    }
}

/// Host-owned metadata, supplied independently of the package being admitted.
/// No package Signature/TypeId/FunctionId can be imported as a trust assertion.
/// Parameter defaults are ordinary B01 default functions generated from these
/// host-owned constants. Runtime admission resolves the same declarations.
#[derive(Clone, Debug)]
pub(super) struct NativeParameter {
    pub name: String,
    pub ty: BuiltinType,
    pub default: Option<bc::Const>,
}
#[derive(Clone, Debug)]
pub(super) struct NativeContract {
    pub symbol: String,
    pub abi_version: u32,
    pub parameters: Vec<NativeParameter>,
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
            .enumerate()
            .map(|(index, parameter)| {
                Ok(Parameter {
                    name: symbol(&parameter.name)?,
                    ty: ty(&parameter.ty)?,
                    mutable: false,
                    default: parameter
                        .default
                        .as_ref()
                        .map(|value| default_function(package, contract, index, value))
                        .transpose()?,
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
    let mut contracts: Vec<_> = [
        ("narrator", "text", BuiltinType::String, BuiltinType::Null),
        ("choose", "options", BuiltinType::Array, BuiltinType::String),
    ]
    .into_iter()
    .map(|(name, parameter, input, result)| NativeContract {
        symbol: format!("reel::{name}"),
        abi_version: 1,
        parameters: vec![NativeParameter {
            name: parameter.into(),
            ty: input,
            default: None,
        }],
        result,
        effects: Effects::ALL,
        required_capabilities: capabilities(),
    })
    .collect();
    for name in ["text", "image", "row", "column", "stack"] {
        let mut parameters = Vec::new();
        for &field in node_prefix(&format!("reel::{name}")) {
            parameters.push(NativeParameter {
                name: field.into(),
                ty: BuiltinType::String,
                default: (field == "fit").then(|| bc::Const::Utf8("contain".into())),
            });
        }
        for (name, (ty, default)) in NODE_FIELDS.into_iter().zip([
            (BuiltinType::Any, bc::Const::Int(0)),
            (BuiltinType::Any, bc::Const::Int(0)),
            (BuiltinType::Any, bc::Const::Null),
            (BuiltinType::Any, bc::Const::Null),
            (BuiltinType::Any, bc::Const::Int(1)),
            (BuiltinType::Bool, bc::Const::Bool(true)),
            (BuiltinType::Bool, bc::Const::Bool(false)),
            (BuiltinType::Any, bc::Const::Int(0)),
            (BuiltinType::Any, bc::Const::Int(0)),
            (BuiltinType::Any, bc::Const::Int(24)),
            (BuiltinType::String, bc::Const::Utf8("#ffffff".into())),
            (BuiltinType::Any, bc::Const::Null),
            (BuiltinType::Any, bc::Const::Int(0)),
            (BuiltinType::Any, bc::Const::Int(1)),
            (BuiltinType::Any, bc::Const::Int(1)),
            (BuiltinType::Any, bc::Const::Int(0)),
            (BuiltinType::Any, bc::Const::Int(0)),
        ]) {
            parameters.push(NativeParameter {
                name: name.into(),
                ty,
                default: Some(default),
            });
        }
        if matches!(name, "row" | "column" | "stack") {
            parameters.push(NativeParameter {
                name: "children".into(),
                ty: BuiltinType::Any,
                default: Some(bc::Const::Null),
            });
        }
        contracts.push(NativeContract {
            symbol: format!("reel::{name}"),
            abi_version: 1,
            parameters,
            result: BuiltinType::Any,
            effects: Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST),
            required_capabilities: vec!["reel.stage".into()],
        });
    }
    contracts.push(NativeContract {
        symbol: "reel::stage::replace".into(),
        abi_version: 1,
        parameters: vec![NativeParameter {
            name: "root".into(),
            ty: BuiltinType::Any,
            default: None,
        }],
        result: BuiltinType::Null,
        effects: Effects(Effects::WRITE | Effects::HOST),
        required_capabilities: vec!["reel.stage".into()],
    });
    contracts
}

pub(super) fn capabilities() -> Vec<String> {
    vec!["reel.session".into(), "reel.stage".into()]
}

pub(super) fn default_name(contract: &NativeContract, index: usize) -> String {
    format!("{}::default::{index}", contract.symbol)
}

fn default_function(
    package: &BytecodePackage,
    contract: &NativeContract,
    index: usize,
    value: &bc::Const,
) -> Result<bc::FunctionId, AdapterError> {
    let name = default_name(contract, index);
    let (id, function) = package
        .functions
        .iter()
        .enumerate()
        .find(|(_, function)| package.symbols[function.name.index()] == name)
        .ok_or(AdapterError::Input("native default missing"))?;
    // A package cannot replace the registered constant default with script effects.
    let valid = function.context == bc::Context::NoWait
        && function.captures.is_empty()
        && function.code.len() == 2
        && function.code[0].opcode == bc::Opcode::LoadConst as u16
        && function.code[1] == bc::Instruction::new(bc::Opcode::Return, index as u32, 0, 0)
        && function.code[0].operands[0] == index as u32
        && package.constants.get(function.code[0].operands[1] as usize) == Some(value);
    if !valid {
        return Err(AdapterError::Input("native default differs from registry"));
    }
    Ok(bc::FunctionId(id as u32))
}
