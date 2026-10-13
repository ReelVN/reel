use super::stage::NodeProperty;
use aside_host::PropertySchema;
pub(super) use aside_host::{NativeContract, NativeParameter};
use aside_vm::bytecode::{self as bc, BuiltinType, Effects};

pub(super) fn node_prefix(symbol: &str) -> &'static [&'static str] {
    match symbol {
        "reel::text" => &["value"],
        "reel::image" => &["source", "fit"],
        "reel::button" => &["label", "on_click"],
        _ => &[],
    }
}

pub(super) fn contracts() -> Vec<NativeContract> {
    let mut contracts: Vec<_> = [
        ("narrator", "text", BuiltinType::String, BuiltinType::Null),
        ("choose", "options", BuiltinType::Array, BuiltinType::String),
    ]
    .into_iter()
    .map(|(name, parameter, input, result)| NativeContract {
        rest: None,
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
    for name in ["text", "image", "row", "column", "stack", "button"] {
        let mut parameters = Vec::new();
        for &field in node_prefix(&format!("reel::{name}")) {
            parameters.push(NativeParameter {
                name: field.into(),
                ty: if name == "button" {
                    BuiltinType::Any
                } else {
                    BuiltinType::String
                },
                default: if name == "button" {
                    Some(bc::Const::Null)
                } else {
                    (field == "fit").then(|| bc::Const::Utf8("contain".into()))
                },
            });
        }
        parameters.extend(NodeProperty::PROPERTIES.iter().map(|p| p.parameter()));
        if matches!(name, "row" | "column" | "stack" | "button") {
            parameters.push(NativeParameter {
                name: "children".into(),
                ty: BuiltinType::Any,
                default: Some(bc::Const::Null),
            });
        }
        contracts.push(NativeContract {
            rest: None,
            symbol: format!("reel::{name}"),
            abi_version: 1,
            parameters,
            result: BuiltinType::Any,
            effects: Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST),
            required_capabilities: vec!["reel.stage".into()],
        });
    }
    contracts.push(NativeContract {
        rest: None,
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
    let parameter = |name: &str, default| NativeParameter {
        name: name.into(),
        ty: BuiltinType::Any,
        default,
    };
    let mut tween = vec![
        parameter("target", None),
        parameter("duration", Some(bc::Const::FloatBits(0.3_f64.to_bits()))),
        NativeParameter {
            name: "easing".into(),
            ty: BuiltinType::String,
            default: Some(bc::Const::Utf8("linear".into())),
        },
    ];
    tween.extend(
        NodeProperty::PROPERTIES
            .iter()
            .filter(|p| p.animatable)
            .map(|p| parameter(p.name, Some(bc::Const::Null))),
    );
    for (name, parameters, waits) in [
        ("tween", tween, false),
        (
            "run",
            vec![
                parameter("action", None),
                parameter("owner", Some(bc::Const::Null)),
            ],
            false,
        ),
        ("activity::await", vec![parameter("activity", None)], true),
    ] {
        contracts.push(NativeContract {
            rest: None,
            symbol: format!("reel::{name}"),
            abi_version: 1,
            parameters,
            result: BuiltinType::Any,
            effects: Effects(
                Effects::ALLOCATE
                    | Effects::WRITE
                    | Effects::HOST
                    | if waits { Effects::WAIT } else { 0 },
            ),
            required_capabilities: vec!["reel.stage".into()],
        });
    }
    contracts.extend([
        super::activity::pause::binding().contract,
        super::activity::resume::binding().contract,
        super::activity::stop::binding().contract,
        super::activity::finish::binding().contract,
    ]);
    contracts.extend(super::stage::bindings().map(|binding| binding.contract));
    contracts
}

pub(super) fn capabilities() -> Vec<String> {
    vec!["reel.session".into(), "reel.stage".into()]
}
