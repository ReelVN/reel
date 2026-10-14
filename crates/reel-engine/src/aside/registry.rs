use super::stage::NodeProperty;
use aside_host::PropertySchema;
pub(super) use aside_host::{NativeContract, NativeParameter};
use aside_vm::bytecode::{self as bc, BuiltinType, Effects};

pub(super) fn node_prefix(symbol: &str) -> &'static [&'static str] {
    match symbol {
        "reel::text" => &["value"],
        "reel::image" => &["source", "fit"],
        "reel::button" => &["label", "on_click"],
        "reel::input" => &["bindings", "on_action"],
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
    for (symbol, parameters, effects) in [
        (
            "character",
            vec![
                ("name", BuiltinType::String, None),
                ("id", BuiltinType::Any, Some(bc::Const::Null)),
            ],
            Effects::ALLOCATE,
        ),
        (
            "character::line",
            vec![
                ("character", BuiltinType::Any, None),
                ("text", BuiltinType::String, None),
            ],
            Effects::ALLOCATE,
        ),
        (
            "character::speak",
            vec![
                ("character", BuiltinType::Any, None),
                ("text", BuiltinType::String, None),
            ],
            Effects::ALL.0,
        ),
        (
            "say",
            vec![("lines", BuiltinType::Array, None)],
            Effects::ALL.0,
        ),
    ] {
        contracts.push(NativeContract {
            rest: None,
            symbol: format!("reel::{symbol}"),
            abi_version: 1,
            parameters: parameters
                .into_iter()
                .map(|(name, ty, default)| NativeParameter {
                    name: name.into(),
                    ty,
                    default,
                })
                .collect(),
            result: BuiltinType::Any,
            effects: Effects(effects),
            required_capabilities: capabilities(),
        });
    }
    for name in ["text", "image", "row", "column", "stack", "button", "input"] {
        let mut parameters = Vec::new();
        for &field in node_prefix(&format!("reel::{name}")) {
            parameters.push(NativeParameter {
                name: field.into(),
                ty: if matches!(name, "button" | "input") {
                    BuiltinType::Any
                } else {
                    BuiltinType::String
                },
                default: if name == "button" || field == "on_action" {
                    Some(bc::Const::Null)
                } else {
                    (field == "fit").then(|| bc::Const::Utf8("contain".into()))
                },
            });
        }
        parameters.extend(NodeProperty::PROPERTIES.iter().map(|p| p.parameter()));
        if matches!(name, "row" | "column" | "stack" | "button" | "input") {
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
    contracts.push(NativeContract {
        rest: None,
        symbol: "reel::game::configure".into(),
        abi_version: 1,
        parameters: ["title", "width", "height", "fonts", "text_speed"]
            .into_iter()
            .map(|name| NativeParameter {
                name: name.into(),
                ty: BuiltinType::Any,
                default: Some(bc::Const::Null),
            })
            .collect(),
        result: BuiltinType::Null,
        effects: Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST),
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
        (
            "repeat",
            vec![
                parameter("action", None),
                parameter("times", Some(bc::Const::Null)),
            ],
            false,
        ),
        ("pause", vec![parameter("activity", None)], false),
        ("resume", vec![parameter("activity", None)], false),
        ("stop", vec![parameter("activity", None)], false),
        ("finish", vec![parameter("activity", None)], false),
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
    for name in ["sequence", "parallel"] {
        contracts.push(NativeContract {
            rest: Some("actions".into()),
            symbol: format!("reel::{name}"),
            abi_version: 1,
            parameters: vec![],
            result: BuiltinType::Any,
            effects: Effects(Effects::ALLOCATE | Effects::HOST),
            required_capabilities: vec!["reel.stage".into()],
        });
    }
    contracts.extend(super::stage::bindings().map(|binding| binding.contract));
    contracts
}

pub(super) fn capabilities() -> Vec<String> {
    vec!["reel.session".into(), "reel.stage".into()]
}
