use super::stage::NodeProperty;
use aside_host::PropertySchema;
pub(super) use aside_host::{NativeContract, NativeParameter};
use aside_vm::bytecode::{self as bc, BuiltinType, Effects};

pub(super) const IMAGE_PREFIX: &[&str] = super::stage::appearance::ImageField::NAMES;

pub(super) const NODE_CONSTRUCTORS: &[(&str, &str)] = &[
    ("text", "reel::types::TextNode"),
    ("text_input", "reel::types::TextInputNode"),
    ("text_area", "reel::types::TextInputNode"),
    ("image", "reel::types::ImageNode"),
    ("video", "reel::types::VideoNode"),
    ("particles", "reel::types::ParticlesNode"),
    ("row", "reel::types::FlowNode"),
    ("column", "reel::types::FlowNode"),
    ("grid", "reel::types::GridNode"),
    ("stack", "reel::types::Node"),
    ("cursor", "reel::types::Node"),
    ("button", "reel::types::ButtonNode"),
    ("input", "reel::types::InputNode"),
    ("slider", "reel::types::SliderNode"),
    ("toggle", "reel::types::ToggleNode"),
    ("checkbox", "reel::types::ToggleNode"),
    ("radio", "reel::types::RadioNode"),
    ("scroll", "reel::types::ScrollNode"),
    ("popup", "reel::types::PopupNode"),
    ("camera", "reel::types::CameraNode"),
];

pub(super) fn node_type(symbol: &str) -> Option<&'static str> {
    let name = symbol.strip_prefix("reel::")?;
    NODE_CONSTRUCTORS
        .iter()
        .find(|(constructor, _)| *constructor == name)
        .map(|(_, ty)| *ty)
        .or(match name {
            "list" | "view" | "layered_image" => Some("reel::types::Node"),
            _ => None,
        })
}

pub(super) fn is_node_constructor(symbol: &str) -> bool {
    node_type(symbol).is_some()
}

pub(super) fn node_prefix(symbol: &str) -> &'static [&'static str] {
    match symbol {
        "reel::row" | "reel::column" => super::stage::flow::FlowField::NAMES,
        "reel::grid" => super::stage::flow::GridField::NAMES,
        "reel::camera" => super::stage::camera::CameraField::NAMES,
        "reel::scroll" => super::stage::scroll::ScrollField::NAMES,
        "reel::popup" => super::stage::popup::PopupField::NAMES,
        "reel::text" => super::stage::appearance::TextField::NAMES,
        "reel::text_input" | "reel::text_area" => super::stage::text_input::TextInputField::NAMES,
        "reel::particles" => super::particles::PREFIX,
        "reel::image" | "reel::video" => IMAGE_PREFIX,
        "reel::button" => super::stage::appearance::ButtonField::NAMES,
        "reel::slider" => super::stage::controls::SliderField::NAMES,
        "reel::toggle" | "reel::checkbox" => super::stage::controls::ToggleField::NAMES,
        "reel::radio" => super::stage::controls::RadioField::NAMES,
        "reel::input" => super::stage::input::InputField::NAMES,
        _ => &[],
    }
}

pub(super) fn contracts() -> Vec<NativeContract> {
    let mut contracts: Vec<_> = [
        ("narrator", "text", BuiltinType::Any, BuiltinType::Any),
        ("choose", "options", BuiltinType::Array, BuiltinType::String),
    ]
    .into_iter()
    .map(|(name, parameter, input, result)| NativeContract {
        rest: None,
        symbol: format!("reel::{name}"),
        abi_version: 1,
        parameters: {
            let mut parameters = vec![NativeParameter {
                name: parameter.into(),
                ty: input,
                default: None,
            }];
            if name == "narrator" {
                parameters.push(NativeParameter {
                    name: "history".into(),
                    ty: BuiltinType::Any,
                    default: Some(bc::Const::Bool(true)),
                });
                parameters.push(NativeParameter {
                    name: "reveal_skippable".into(),
                    ty: BuiltinType::Bool,
                    default: Some(bc::Const::Bool(true)),
                });
                parameters.push(NativeParameter {
                    name: "wait".into(),
                    ty: BuiltinType::Bool,
                    default: Some(bc::Const::Bool(true)),
                });
            }
            parameters
        },
        result,
        effects: Effects::ALL,
        required_capabilities: capabilities(),
    })
    .collect();
    for (name, fields) in [
        ("info", &[][..]),
        (
            "configure",
            &["width", "height", "fullscreen", "vsync", "owner"][..],
        ),
    ] {
        contracts.push(NativeContract {
            rest: None,
            symbol: format!("reel::window::{name}"),
            abi_version: 1,
            parameters: fields
                .iter()
                .map(|field| NativeParameter {
                    name: (*field).into(),
                    ty: BuiltinType::Any,
                    default: Some(bc::Const::Null),
                })
                .collect(),
            result: BuiltinType::Any,
            effects: Effects(
                Effects::ALLOCATE
                    | Effects::HOST
                    | if name == "configure" {
                        Effects::WRITE
                    } else {
                        0
                    },
            ),
            required_capabilities: vec![],
        });
    }
    for (name, fields) in [
        ("pointer", &[][..]),
        ("move_pointer", &["x", "y", "owner"][..]),
    ] {
        contracts.push(NativeContract {
            rest: None,
            symbol: format!("reel::stage::{name}"),
            abi_version: 1,
            parameters: fields
                .iter()
                .map(|field| NativeParameter {
                    name: (*field).into(),
                    ty: BuiltinType::Any,
                    default: None,
                })
                .collect(),
            result: BuiltinType::Any,
            effects: Effects(
                Effects::ALLOCATE
                    | Effects::HOST
                    | if name == "move_pointer" {
                        Effects::WRITE
                    } else {
                        0
                    },
            ),
            required_capabilities: vec!["reel.stage".into()],
        });
    }
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
                ("text", BuiltinType::Any, None),
                ("history", BuiltinType::Any, Some(bc::Const::Bool(true))),
            ],
            Effects::ALLOCATE,
        ),
        (
            "character::speak",
            vec![
                ("character", BuiltinType::Any, None),
                ("text", BuiltinType::Any, None),
                ("history", BuiltinType::Any, Some(bc::Const::Bool(true))),
                (
                    "reveal_skippable",
                    BuiltinType::Bool,
                    Some(bc::Const::Bool(true)),
                ),
                ("wait", BuiltinType::Bool, Some(bc::Const::Bool(true))),
            ],
            Effects::ALL.0,
        ),
        (
            "say",
            vec![
                ("lines", BuiltinType::Array, None),
                (
                    "reveal_skippable",
                    BuiltinType::Bool,
                    Some(bc::Const::Bool(true)),
                ),
                ("wait", BuiltinType::Bool, Some(bc::Const::Bool(true))),
            ],
            Effects::ALL.0,
        ),
        (
            "speech::append",
            vec![
                ("speech", BuiltinType::Any, None),
                ("text", BuiltinType::Any, None),
                ("history", BuiltinType::Any, Some(bc::Const::Bool(true))),
                ("wait", BuiltinType::Bool, Some(bc::Const::Bool(true))),
            ],
            Effects::ALL.0,
        ),
        (
            "speech::wait",
            vec![("speech", BuiltinType::Any, None)],
            Effects::ALL.0,
        ),
        (
            "speech::revealed",
            vec![("speech", BuiltinType::Any, None)],
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
    for &(name, _) in NODE_CONSTRUCTORS {
        let mut parameters = Vec::new();
        if matches!(name, "row" | "column" | "grid") {
            let properties = if name == "grid" {
                super::stage::flow::GridField::PROPERTIES
            } else {
                super::stage::flow::FlowField::PROPERTIES
            };
            parameters.extend(properties.iter().map(|p| p.parameter()));
        } else if name == "camera" {
            parameters.extend(
                super::stage::camera::CameraField::PROPERTIES
                    .iter()
                    .map(|p| p.parameter()),
            );
        } else if name == "particles" {
            parameters.extend(super::particles::parameters());
        } else if name == "scroll" {
            parameters.extend(
                super::stage::scroll::ScrollField::PROPERTIES
                    .iter()
                    .map(|p| p.parameter()),
            );
        } else if name == "popup" {
            parameters.extend(
                super::stage::popup::PopupField::PROPERTIES
                    .iter()
                    .map(|p| p.parameter()),
            );
        } else if name == "input" {
            parameters.extend(
                super::stage::input::InputField::PROPERTIES
                    .iter()
                    .map(|p| p.parameter()),
            );
        } else if matches!(name, "text_input" | "text_area") {
            parameters.extend(
                super::stage::text_input::TextInputField::PROPERTIES
                    .iter()
                    .map(|p| p.parameter()),
            );
        } else if name == "slider" {
            parameters.extend(
                super::stage::controls::SliderField::PROPERTIES
                    .iter()
                    .map(|p| p.parameter()),
            );
        } else if matches!(name, "toggle" | "checkbox") {
            parameters.extend(
                super::stage::controls::ToggleField::PROPERTIES
                    .iter()
                    .map(|p| p.parameter()),
            );
        } else if name == "radio" {
            parameters.extend(
                super::stage::controls::RadioField::PROPERTIES
                    .iter()
                    .map(|p| p.parameter()),
            );
        } else if let Some(properties) = match name {
            "text" => Some(super::stage::appearance::TextField::PROPERTIES),
            "button" => Some(super::stage::appearance::ButtonField::PROPERTIES),
            "image" | "video" => Some(super::stage::appearance::ImageField::PROPERTIES),
            _ => None,
        } {
            parameters.extend(properties.iter().map(|p| p.parameter()));
            if matches!(name, "text" | "image" | "video") {
                parameters[0].default = None;
            }
        }
        parameters.extend(NodeProperty::PROPERTIES.iter().map(|p| p.parameter()));
        if name == "popup" {
            parameters
                .iter_mut()
                .find(|p| p.name == "modal")
                .unwrap()
                .default = Some(bc::Const::Bool(true));
        }
        if name == "camera" {
            parameters
                .iter_mut()
                .find(|p| p.name == "clip")
                .unwrap()
                .default = Some(bc::Const::Bool(true));
        }
        // Property parameters accept either a scalar or a stable reference;
        // the shared host schema validates the resolved value at the boundary.
        for parameter in &mut parameters {
            parameter.ty = BuiltinType::Any;
        }

        if matches!(
            name,
            "grid"
                | "row"
                | "column"
                | "stack"
                | "cursor"
                | "button"
                | "slider"
                | "input"
                | "toggle"
                | "checkbox"
                | "radio"
                | "scroll"
                | "popup"
                | "camera"
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
            abi_version: 1,
            parameters,
            result: BuiltinType::Any,
            effects: Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST),
            required_capabilities: vec!["reel.stage".into()],
        });
    }
    for (symbol, parameters, effects) in [
        ("reel::game::exit", vec![], Effects::WRITE | Effects::HOST),
        (
            "reel::game::start",
            vec!["entry"],
            Effects::WRITE | Effects::HOST,
        ),
        ("reel::game::dialogue", vec![], Effects::ALLOCATE),
        ("reel::game::choice", vec![], Effects::ALLOCATE),
        ("reel::game::history", vec![], Effects::ALLOCATE),
        ("reel::game::reading", vec![], Effects::ALLOCATE),
        (
            "reel::reading::pause",
            vec!["reading", "paused"],
            Effects::ALLOCATE | Effects::WRITE | Effects::HOST,
        ),
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
    contracts.push(NativeContract {
        rest: None,
        symbol: "reel::game::record".into(),
        abi_version: 1,
        parameters: vec![
            NativeParameter {
                name: "text".into(),
                ty: BuiltinType::Any,
                default: None,
            },
            NativeParameter {
                name: "speaker".into(),
                ty: BuiltinType::Any,
                default: Some(bc::Const::Null),
            },
        ],
        result: BuiltinType::Null,
        effects: Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST),
        required_capabilities: capabilities(),
    });
    let mut view_parameters = vec![NativeParameter {
        name: "build".into(),
        ty: BuiltinType::Any,
        default: None,
    }];
    let mut list_parameters: Vec<_> = ["items", "key", "build"]
        .into_iter()
        .map(|name| NativeParameter {
            name: name.into(),
            ty: BuiltinType::Any,
            default: None,
        })
        .collect();
    list_parameters.push(NativeParameter {
        name: "virtual".into(),
        ty: BuiltinType::Bool,
        default: Some(bc::Const::Bool(false)),
    });
    list_parameters.extend(NodeProperty::PROPERTIES.iter().map(|p| p.parameter()));
    for parameter in &mut list_parameters {
        parameter.ty = BuiltinType::Any;
    }
    contracts.push(NativeContract {
        rest: None,
        symbol: "reel::list".into(),
        abi_version: 1,
        parameters: list_parameters,
        result: BuiltinType::Any,
        effects: Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST),
        required_capabilities: vec!["reel.stage".into()],
    });
    view_parameters.extend(NodeProperty::PROPERTIES.iter().map(|p| p.parameter()));
    for parameter in &mut view_parameters {
        parameter.ty = BuiltinType::Any;
    }
    contracts.push(NativeContract {
        rest: None,
        symbol: "reel::view".into(),
        abi_version: 1,
        parameters: view_parameters.clone(),
        result: BuiltinType::Any,
        effects: Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST),
        required_capabilities: vec!["reel.stage".into()],
    });
    view_parameters[0].name = "layers".into();
    contracts.push(NativeContract {
        rest: None,
        symbol: "reel::when".into(),
        abi_version: 1,
        parameters: vec![
            NativeParameter {
                name: "predicate".into(),
                ty: BuiltinType::Any,
                default: None,
            },
            NativeParameter {
                name: "owner".into(),
                ty: BuiltinType::Any,
                default: Some(bc::Const::Null),
            },
        ],
        result: BuiltinType::Any,
        effects: Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST),
        required_capabilities: vec!["reel.stage".into()],
    });
    contracts.push(NativeContract {
        rest: None,
        symbol: "reel::when::evaluate".into(),
        abi_version: 1,
        parameters: vec![NativeParameter {
            name: "predicate".into(),
            ty: BuiltinType::Any,
            default: None,
        }],
        result: BuiltinType::Bool,
        effects: Effects(Effects::ALLOCATE),
        required_capabilities: vec!["reel.stage".into()],
    });
    contracts.push(NativeContract {
        rest: None,
        symbol: "reel::layered_image".into(),
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
    contracts.push(NativeContract {
        rest: None,
        symbol: "reel::stage::capture".into(),
        abi_version: 1,
        parameters: vec![
            parameter("width", Some(bc::Const::Null)),
            parameter("height", Some(bc::Const::Null)),
            parameter("owner", Some(bc::Const::Null)),
        ],
        result: BuiltinType::Any,
        effects: Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST),
        required_capabilities: vec!["reel.stage".into()],
    });
    for (name, parameters) in [
        (
            "save",
            vec![
                parameter("slot", None),
                parameter("title", Some(bc::Const::Null)),
                parameter("thumbnail", Some(bc::Const::Null)),
            ],
        ),
        ("load", vec![parameter("slot", None)]),
        ("delete", vec![parameter("slot", None)]),
        ("thumbnail", vec![parameter("slot", None)]),
        ("list", vec![]),
    ] {
        contracts.push(NativeContract {
            rest: None,
            symbol: format!("reel::saves::{name}"),
            abi_version: 1,
            parameters,
            result: BuiltinType::Any,
            effects: Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST),
            required_capabilities: vec![],
        });
    }
    for (name, parameters) in [
        (
            "read",
            vec![
                parameter("key", None),
                parameter("default", Some(bc::Const::Null)),
            ],
        ),
        (
            "write",
            vec![parameter("key", None), parameter("value", None)],
        ),
        ("delete", vec![parameter("key", None)]),
    ] {
        contracts.push(NativeContract {
            rest: None,
            symbol: format!("reel::persistent::{name}"),
            abi_version: 1,
            parameters,
            result: BuiltinType::Any,
            effects: Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST),
            required_capabilities: vec![],
        });
    }
    for (name, parameters, waits) in [
        (
            "queue",
            vec![
                parameter("activity", None),
                parameter("source", None),
                parameter("start", Some(bc::Const::Int(0))),
                parameter("end", Some(bc::Const::Null)),
                parameter("looping", Some(bc::Const::Bool(false))),
                parameter("loop_start", Some(bc::Const::Int(0))),
                parameter("loop_end", Some(bc::Const::Null)),
            ],
            false,
        ),
        (
            "play",
            vec![
                parameter("source", None),
                parameter("looping", Some(bc::Const::Bool(false))),
                parameter("volume", Some(bc::Const::FloatBits(1_f64.to_bits()))),
                parameter("owner", Some(bc::Const::Null)),
                parameter("group", Some(bc::Const::Utf8("effects".into()))),
                parameter("pan", Some(bc::Const::Int(0))),
                parameter("fade_in", Some(bc::Const::Int(0))),
                parameter("start", Some(bc::Const::Int(0))),
                parameter("end", Some(bc::Const::Null)),
                parameter("loop_start", Some(bc::Const::Int(0))),
                parameter("loop_end", Some(bc::Const::Null)),
                parameter("paused", Some(bc::Const::Bool(false))),
            ],
            false,
        ),
        (
            "volume",
            vec![
                parameter("activity", None),
                parameter("value", None),
                parameter("duration", Some(bc::Const::Int(0))),
            ],
            false,
        ),
        (
            "group_volume",
            vec![
                parameter("group", None),
                parameter("value", None),
                parameter("duration", Some(bc::Const::Int(0))),
            ],
            false,
        ),
        (
            "pan",
            vec![
                parameter("activity", None),
                parameter("value", None),
                parameter("duration", Some(bc::Const::Int(0))),
            ],
            false,
        ),
        (
            "stop",
            vec![
                parameter("activity", None),
                parameter("fade", Some(bc::Const::Int(0))),
            ],
            false,
        ),
        (
            "seek",
            vec![parameter("activity", None), parameter("seconds", None)],
            false,
        ),
        (
            "at",
            vec![parameter("activity", None), parameter("seconds", None)],
            false,
        ),
        ("position", vec![parameter("activity", None)], false),
        ("await", vec![parameter("activity", None)], true),
    ] {
        contracts.push(NativeContract {
            rest: None,
            symbol: format!("reel::audio::{name}"),
            abi_version: 1,
            parameters,
            result: if name == "position" {
                BuiltinType::Float
            } else if matches!(name, "seek" | "queue") {
                BuiltinType::Null
            } else {
                BuiltinType::Any
            },
            effects: Effects(
                Effects::ALLOCATE
                    | if name == "position" {
                        0
                    } else {
                        Effects::WRITE | Effects::HOST
                    }
                    | if waits { Effects::WAIT } else { 0 },
            ),
            required_capabilities: vec!["reel.audio".into()],
        });
    }
    for (name, parameters) in [
        (
            "play",
            vec![
                parameter("node", None),
                parameter("looping", Some(bc::Const::Bool(false))),
                parameter("volume", Some(bc::Const::Int(1))),
                parameter("start", Some(bc::Const::Int(0))),
                parameter("end", Some(bc::Const::Null)),
                parameter("owner", Some(bc::Const::Null)),
                parameter("paused", Some(bc::Const::Bool(false))),
            ],
        ),
        (
            "seek",
            vec![parameter("activity", None), parameter("seconds", None)],
        ),
        (
            "at",
            vec![parameter("activity", None), parameter("seconds", None)],
        ),
        ("position", vec![parameter("activity", None)]),
    ] {
        contracts.push(NativeContract {
            rest: None,
            symbol: format!("reel::video::{name}"),
            abi_version: 1,
            parameters,
            result: BuiltinType::Any,
            effects: Effects(
                Effects::ALLOCATE
                    | if name == "position" {
                        0
                    } else {
                        Effects::WRITE | Effects::HOST
                    },
            ),
            required_capabilities: vec!["reel.stage".into()],
        });
    }
    for (symbol, parameters) in [
        ("reel::game::clear_read_progress", vec![]),
        (
            "reel::reading::clear_history",
            vec![
                parameter("reading", None),
                parameter("keep_last", Some(bc::Const::Int(0))),
            ],
        ),
    ] {
        contracts.push(NativeContract {
            rest: None,
            symbol: symbol.into(),
            abi_version: 1,
            parameters,
            result: if symbol == "reel::reading::clear_history" {
                BuiltinType::Any
            } else {
                BuiltinType::Null
            },
            effects: Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST),
            required_capabilities: vec![],
        });
    }
    contracts.push(NativeContract {
        rest: None,
        symbol: "reel::reading::set_auto".into(),
        abi_version: 1,
        parameters: vec![
            parameter("reading", None),
            parameter("enabled", Some(bc::Const::Null)),
            parameter("delay", Some(bc::Const::Null)),
            parameter("condition", Some(bc::Const::Null)),
        ],
        result: BuiltinType::Any,
        effects: Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST),
        required_capabilities: vec![],
    });
    contracts.push(NativeContract {
        rest: None,
        symbol: "reel::reading::set_skip".into(),
        abi_version: 1,
        parameters: vec![
            parameter("reading", None),
            parameter("enabled", None),
            parameter("unread", Some(bc::Const::Bool(false))),
            parameter("after_choices", Some(bc::Const::Bool(false))),
        ],
        result: BuiltinType::Any,
        effects: Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST),
        required_capabilities: vec![],
    });
    let mut tween = vec![
        parameter("target", None),
        parameter("duration", Some(bc::Const::FloatBits(0.3_f64.to_bits()))),
        NativeParameter {
            name: "easing".into(),
            ty: BuiltinType::Any,
            default: Some(bc::Const::Utf8("linear".into())),
        },
    ];
    tween.extend(
        NodeProperty::PROPERTIES
            .iter()
            .filter(|p| p.animatable)
            .map(|p| parameter(p.name, Some(bc::Const::Null))),
    );
    for (_, properties) in super::stage::animation_prefixes() {
        tween.extend(
            properties
                .iter()
                .filter(|p| p.animatable)
                .map(|p| parameter(p.name, Some(bc::Const::Null))),
        );
    }
    tween.push(parameter("from", Some(bc::Const::Null)));
    for (name, parameters, waits) in [
        ("tween", tween, false),
        (
            "stage::transition",
            vec![
                parameter("change", None),
                parameter("target", Some(bc::Const::Null)),
                parameter("duration", Some(bc::Const::FloatBits(0.3_f64.to_bits()))),
                parameter("easing", Some(bc::Const::Utf8("linear".into()))),
                parameter("mask", Some(bc::Const::Null)),
                parameter(
                    "mask_softness",
                    Some(bc::Const::FloatBits(0.06_f64.to_bits())),
                ),
                parameter("mask_invert", Some(bc::Const::Bool(false))),
                parameter("mask_x", Some(bc::Const::Int(0))),
                parameter("mask_y", Some(bc::Const::Int(0))),
                parameter("owner", Some(bc::Const::Null)),
            ],
            true,
        ),
        (
            "scope",
            vec![
                parameter("body", None),
                parameter("owner", Some(bc::Const::Null)),
            ],
            true,
        ),
        (
            "reading",
            vec![
                parameter("body", None),
                parameter("owner", Some(bc::Const::Null)),
            ],
            true,
        ),
        (
            "scope::run",
            vec![
                parameter("scope", None),
                parameter("action", None),
                parameter("paused", Some(bc::Const::Bool(false))),
            ],
            false,
        ),
        (
            "scope::spawn",
            vec![parameter("scope", None), parameter("work", None)],
            false,
        ),
        (
            "scope::add",
            vec![
                parameter("scope", None),
                parameter("parent", None),
                parameter("node", None),
                parameter("index", Some(bc::Const::Null)),
            ],
            false,
        ),
        (
            "scope::override",
            vec![
                parameter("scope", None),
                parameter("node", None),
                parameter("properties", None),
            ],
            false,
        ),
        ("scope::cancel", vec![parameter("scope", None)], false),
        (
            "scope::finish",
            vec![
                parameter("scope", None),
                parameter("value", Some(bc::Const::Null)),
            ],
            false,
        ),
        ("override::remove", vec![parameter("token", None)], false),
        (
            "scope::offset",
            vec![
                parameter("scope", None),
                parameter("node", None),
                parameter("x", Some(bc::Const::Int(0))),
                parameter("y", Some(bc::Const::Int(0))),
            ],
            false,
        ),
        ("delay", vec![parameter("seconds", None)], false),
        (
            "clock",
            vec![
                parameter("interval", Some(bc::Const::Int(1))),
                parameter("owner", Some(bc::Const::Null)),
                parameter("paused", Some(bc::Const::Bool(false))),
            ],
            false,
        ),
        (
            "run",
            vec![
                parameter("action", None),
                parameter("owner", Some(bc::Const::Null)),
                parameter("paused", Some(bc::Const::Bool(false))),
            ],
            false,
        ),
        ("activity::await", vec![parameter("activity", None)], true),
        (
            "stage::presented",
            vec![parameter("owner", Some(bc::Const::Null))],
            false,
        ),
        (
            "stage::geometry",
            vec![
                parameter("node", None),
                parameter("relative_to", Some(bc::Const::Null)),
                parameter("owner", Some(bc::Const::Null)),
            ],
            false,
        ),
        (
            "completion",
            vec![parameter("owner", Some(bc::Const::Null))],
            false,
        ),
        (
            "list::scroll_to",
            vec![
                parameter("list", None),
                parameter("key", None),
                parameter("align", Some(bc::Const::Utf8("nearest".into()))),
            ],
            false,
        ),
        ("completion::await", vec![parameter("result", None)], true),
        (
            "completion::resolve",
            vec![
                parameter("result", None),
                parameter("value", Some(bc::Const::Null)),
            ],
            false,
        ),
        (
            "completion::reject",
            vec![parameter("result", None), parameter("error", None)],
            false,
        ),
        ("completion::cancel", vec![parameter("result", None)], false),
        (
            "spawn",
            vec![
                parameter("work", None),
                parameter("owner", Some(bc::Const::Null)),
            ],
            false,
        ),
        ("task::cancel", vec![parameter("task", None)], false),
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
    for name in ["then", "together"] {
        contracts.push(NativeContract {
            rest: None,
            symbol: format!("reel::action::{name}"),
            abi_version: 1,
            parameters: vec![parameter("action", None), parameter("other", None)],
            result: BuiltinType::Any,
            effects: Effects(Effects::ALLOCATE | Effects::HOST),
            required_capabilities: vec!["reel.stage".into()],
        });
    }
    contracts.extend(super::stage::bindings().map(|binding| binding.contract));
    contracts.extend(super::stage::configure::contracts());
    contracts.push(NativeContract {
        rest: None,
        symbol: "reel::rich".into(),
        abi_version: 1,
        parameters: vec![parameter("markup", None)],
        result: BuiltinType::Any,
        effects: Effects(Effects::ALLOCATE),
        required_capabilities: vec![],
    });
    contracts.push(NativeContract {
        rest: None,
        symbol: "reel::rich_escape".into(),
        abi_version: 1,
        parameters: vec![parameter("text", None)],
        result: BuiltinType::String,
        effects: Effects(Effects::ALLOCATE),
        required_capabilities: vec![],
    });
    for (symbol, parameters, effects, result) in [
        (
            "reel::tr",
            vec![
                parameter("source", None),
                parameter("translations", Some(bc::Const::Null)),
                parameter("params", Some(bc::Const::Null)),
                parameter("locale", Some(bc::Const::Null)),
            ],
            Effects::ALLOCATE,
            BuiltinType::String,
        ),
        (
            "reel::localization::load",
            vec![
                parameter("locale", None),
                parameter("source", Some(bc::Const::Null)),
            ],
            Effects::ALL.0,
            BuiltinType::Null,
        ),
        (
            "reel::localization::set_locale",
            vec![parameter("locale", None)],
            Effects::ALL.0,
            BuiltinType::Null,
        ),
        (
            "reel::localization::locale",
            vec![],
            Effects::ALLOCATE,
            BuiltinType::String,
        ),
    ] {
        contracts.push(NativeContract {
            rest: None,
            symbol: symbol.into(),
            abi_version: 1,
            parameters,
            result,
            effects: Effects(effects),
            required_capabilities: vec![],
        });
    }
    contracts.push(NativeContract {
        rest: None,
        symbol: "reel::emit".into(),
        abi_version: 1,
        parameters: super::particles::emission_parameters(),
        result: BuiltinType::Any,
        effects: Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST),
        required_capabilities: vec!["reel.stage".into()],
    });
    contracts.extend(aside_std::contracts());
    contracts.extend(super::masks::contracts());
    contracts
}

pub(super) fn capabilities() -> Vec<String> {
    vec![
        "reel.session".into(),
        "reel.stage".into(),
        "reel.audio".into(),
    ]
}
