use super::stage::NodeProperty;
use aside_host::PropertySchema;
pub(super) use aside_host::{NativeContract, NativeParameter};
use aside_vm::bytecode::{self as bc, BuiltinType, Effects};

pub(super) fn node_prefix(symbol: &str) -> &'static [&'static str] {
    match symbol {
        "reel::scroll" => &["axis", "follow_end"],
        "reel::text" => &["value"],
        "reel::image" => &["source", "fit"],
        "reel::button" => &["label", "on_click"],
        "reel::slider" => &["value", "min", "max", "step", "on_change"],
        "reel::toggle" => &["label", "value", "on_change"],
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
    for name in [
        "text", "image", "row", "column", "stack", "button", "input", "slider", "toggle", "scroll",
    ] {
        let mut parameters = Vec::new();
        if name == "scroll" {
            parameters.extend([
                NativeParameter {
                    name: "axis".into(),
                    ty: BuiltinType::String,
                    default: Some(bc::Const::Utf8("vertical".into())),
                },
                NativeParameter {
                    name: "follow_end".into(),
                    ty: BuiltinType::Bool,
                    default: Some(bc::Const::Bool(false)),
                },
            ]);
        } else if name == "slider" {
            parameters.extend(
                super::stage::controls::SliderField::PROPERTIES
                    .iter()
                    .map(|p| p.parameter()),
            );
        } else if name == "toggle" {
            parameters.extend(
                super::stage::controls::ToggleField::PROPERTIES
                    .iter()
                    .map(|p| p.parameter()),
            );
        } else {
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
        }
        parameters.extend(NodeProperty::PROPERTIES.iter().map(|p| p.parameter()));
        // Property parameters accept either a scalar or a stable reference;
        // the shared host schema validates the resolved value at the boundary.
        for parameter in &mut parameters {
            parameter.ty = BuiltinType::Any;
        }

        if matches!(
            name,
            "row" | "column" | "stack" | "button" | "input" | "toggle" | "scroll"
        ) {
            parameters.push(NativeParameter {
                name: "children".into(),
                ty: BuiltinType::Any,
                default: Some(bc::Const::Null),
            });
        }
        contracts.push(NativeContract {
            rest: None,
            symbol: format!("reel::{name}"),
            abi_version: if name == "scroll" { 1 } else { 4 },
            parameters,
            result: BuiltinType::Any,
            effects: Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST),
            required_capabilities: vec!["reel.stage".into()],
        });
    }
    for (symbol, parameters, effects) in [
        ("reel::game::dialogue", vec![], Effects::ALLOCATE),
        ("reel::game::choice", vec![], Effects::ALLOCATE),
        (
            "reel::dialogue::clear",
            vec!["dialogue"],
            Effects::ALLOCATE | Effects::WRITE | Effects::HOST,
        ),
        (
            "reel::choice::select",
            vec!["choice", "id"],
            Effects::WRITE | Effects::HOST,
        ),
    ] {
        contracts.push(NativeContract {
            rest: None,
            symbol: symbol.into(),
            abi_version: 1,
            parameters: parameters
                .into_iter()
                .map(|name| NativeParameter {
                    name: name.into(),
                    ty: BuiltinType::Any,
                    default: None,
                })
                .collect(),
            result: BuiltinType::Any,
            effects: Effects(effects),
            required_capabilities: vec![],
        });
    }
    let mut view_parameters = vec![NativeParameter {
        name: "build".into(),
        ty: BuiltinType::Any,
        default: None,
    }];
    view_parameters.extend(NodeProperty::PROPERTIES.iter().map(|p| p.parameter()));
    for parameter in &mut view_parameters {
        parameter.ty = BuiltinType::Any;
    }
    contracts.push(NativeContract {
        rest: None,
        symbol: "reel::view".into(),
        abi_version: 1,
        parameters: view_parameters,
        result: BuiltinType::Any,
        effects: Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST),
        required_capabilities: vec!["reel.stage".into()],
    });
    contracts.push(NativeContract {
        rest: None,
        symbol: "reel::derive".into(),
        abi_version: 1,
        parameters: vec![NativeParameter {
            name: "compute".into(),
            ty: BuiltinType::Any,
            default: None,
        }],
        result: BuiltinType::Any,
        effects: Effects(Effects::ALLOCATE),
        required_capabilities: vec![],
    });
    contracts.push(NativeContract {
        rest: None,
        symbol: "reel::model".into(),
        abi_version: 1,
        parameters: vec![NativeParameter {
            name: "defaults".into(),
            ty: BuiltinType::Any,
            default: None,
        }],
        result: BuiltinType::Any,
        effects: Effects(Effects::ALLOCATE),
        required_capabilities: vec![],
    });
    contracts.push(NativeContract {
        rest: None,
        symbol: "reel::unbind".into(),
        abi_version: 1,
        parameters: vec![NativeParameter {
            name: "property".into(),
            ty: BuiltinType::Any,
            default: None,
        }],
        result: BuiltinType::Any,
        effects: Effects(Effects::WRITE),
        required_capabilities: vec![],
    });
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
        abi_version: 2,
        parameters: [
            "title",
            "width",
            "height",
            "fonts",
            "text_speed",
            "default_dialogue",
            "default_choices",
        ]
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
