use super::stage::NodeProperty;
use aside_host::PropertySchema;
pub(super) use aside_host::{NativeContract, NativeParameter};
use aside_vm::bytecode::{self as bc, BuiltinType, Effects};

pub(super) const IMAGE_PREFIX: &[&str] = &[
    "source",
    "fit",
    "slice",
    "slice_scale",
    "repeat",
    "tile_scale",
    "corner_colors",
    "slice_center",
];

pub(super) fn node_prefix(symbol: &str) -> &'static [&'static str] {
    match symbol {
        "reel::camera" => &["camera_x", "camera_y", "zoom", "camera_rotation"],
        "reel::scroll" => &[
            "axis",
            "follow_end",
            "content_align",
            "scroll_x",
            "scroll_y",
        ],
        "reel::text" => &["value", "on_link"],
        "reel::text_input" => &["value", "on_change", "on_submit"],
        "reel::particles" => super::particles::PREFIX,
        "reel::image" | "reel::video" => IMAGE_PREFIX,
        "reel::button" => &["label", "on_click"],
        "reel::slider" => &["value", "min", "max", "step", "on_change"],
        "reel::toggle" | "reel::checkbox" => &["label", "value", "on_change"],
        "reel::radio" => &["option", "label", "value", "on_change"],
        "reel::input" => &["bindings", "on_action", "on_key", "on_pointer"],
        _ => &[],
    }
}

pub(super) fn contracts() -> Vec<NativeContract> {
    let mut contracts: Vec<_> = [
        ("narrator", "text", BuiltinType::Any, BuiltinType::Null),
        ("choose", "options", BuiltinType::Array, BuiltinType::String),
    ]
    .into_iter()
    .map(|(name, parameter, input, result)| NativeContract {
        rest: None,
        symbol: format!("reel::{name}"),
        abi_version: if name == "narrator" { 3 } else { 1 },
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
            abi_version: if symbol.starts_with("character::") {
                3
            } else {
                1
            },
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
        "text",
        "text_input",
        "image",
        "video",
        "particles",
        "row",
        "column",
        "stack",
        "button",
        "input",
        "slider",
        "toggle",
        "checkbox",
        "radio",
        "scroll",
        "camera",
    ] {
        let mut parameters = Vec::new();
        if name == "camera" {
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
        } else if name == "input" {
            parameters.extend(
                super::stage::input::InputField::PROPERTIES
                    .iter()
                    .map(|p| p.parameter()),
            );
        } else if name == "text_input" {
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
        } else {
            for &field in node_prefix(&format!("reel::{name}")) {
                parameters.push(NativeParameter {
                    name: field.into(),
                    ty: if matches!(name, "button" | "input") {
                        BuiltinType::Any
                    } else {
                        BuiltinType::String
                    },
                    default: if matches!(name, "button" | "input")
                        || matches!(field, "on_link" | "slice" | "corner_colors")
                    {
                        Some(bc::Const::Null)
                    } else if matches!(field, "slice_scale" | "tile_scale") {
                        Some(bc::Const::Int(1))
                    } else if field == "repeat" {
                        Some(bc::Const::Utf8("none".into()))
                    } else if field == "slice_center" {
                        Some(bc::Const::Bool(true))
                    } else {
                        (field == "fit").then(|| bc::Const::Utf8("contain".into()))
                    },
                });
            }
        }
        parameters.extend(NodeProperty::PROPERTIES.iter().map(|p| p.parameter()));
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
            "row"
                | "column"
                | "stack"
                | "button"
                | "slider"
                | "input"
                | "toggle"
                | "checkbox"
                | "radio"
                | "scroll"
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
            abi_version: if name == "video" {
                2
            } else if name == "image" {
                13
            } else if name == "slider" {
                12
            } else if name == "particles" {
                6
            } else if name == "camera" {
                5
            } else if matches!(name, "checkbox" | "radio") {
                7
            } else if name == "input" {
                13
            } else if name == "scroll" {
                9
            } else if name == "text_input" {
                8
            } else {
                11
            },
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
            "reel::game::pause_reading",
            vec!["paused"],
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
    list_parameters.extend(NodeProperty::PROPERTIES.iter().map(|p| p.parameter()));
    for parameter in &mut list_parameters {
        parameter.ty = BuiltinType::Any;
    }
    contracts.push(NativeContract {
        rest: None,
        symbol: "reel::list".into(),
        abi_version: 5,
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
        abi_version: 5,
        parameters: view_parameters.clone(),
        result: BuiltinType::Any,
        effects: Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST),
        required_capabilities: vec!["reel.stage".into()],
    });
    view_parameters[0].name = "layers".into();
    contracts.push(NativeContract {
        rest: None,
        symbol: "reel::layered_image".into(),
        abi_version: 4,
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
        abi_version: 3,
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
            abi_version: if name == "save" { 2 } else { 1 },
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
                parameter("loop_start", Some(bc::Const::Int(0))),
                parameter("loop_end", Some(bc::Const::Null)),
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
        ("position", vec![parameter("activity", None)], false),
        ("await", vec![parameter("activity", None)], true),
    ] {
        contracts.push(NativeContract {
            rest: None,
            symbol: format!("reel::audio::{name}"),
            abi_version: match name {
                "play" => 3,
                "volume" => 2,
                _ => 1,
            },
            parameters,
            result: if name == "position" {
                BuiltinType::Float
            } else if name == "seek" {
                BuiltinType::Null
            } else {
                BuiltinType::Any
            },
            effects: Effects(
                Effects::ALLOCATE
                    | if name == "position" {
                        0
                    } else {
                        Effects::WRITE
                    }
                    | Effects::HOST
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
            ],
        ),
        (
            "seek",
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
                    | Effects::HOST
                    | if name == "position" {
                        0
                    } else {
                        Effects::WRITE
                    },
            ),
            required_capabilities: vec!["reel.stage".into()],
        });
    }
    for symbol in [
        "reel::game::clear_read_progress",
        "reel::game::clear_history",
    ] {
        contracts.push(NativeContract {
            rest: None,
            symbol: symbol.into(),
            abi_version: 1,
            parameters: vec![],
            result: BuiltinType::Null,
            effects: Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST),
            required_capabilities: vec![],
        });
    }
    contracts.push(NativeContract {
        rest: None,
        symbol: "reel::game::set_auto".into(),
        abi_version: 1,
        parameters: vec![
            parameter("enabled", None),
            parameter("delay", Some(bc::Const::Null)),
        ],
        result: BuiltinType::Null,
        effects: Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST),
        required_capabilities: vec![],
    });
    contracts.push(NativeContract {
        rest: None,
        symbol: "reel::game::set_skip".into(),
        abi_version: 2,
        parameters: vec![
            parameter("enabled", None),
            parameter("unread", Some(bc::Const::Bool(false))),
            parameter("after_choices", Some(bc::Const::Bool(false))),
        ],
        result: BuiltinType::Null,
        effects: Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST),
        required_capabilities: vec![],
    });
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
    tween.extend(
        super::stage::camera::CameraField::PROPERTIES
            .iter()
            .filter(|p| p.animatable)
            .map(|p| parameter(p.name, Some(bc::Const::Null))),
    );
    tween.extend(
        super::stage::scroll::ScrollField::PROPERTIES
            .iter()
            .filter(|p| p.animatable)
            .map(|p| parameter(p.name, Some(bc::Const::Null))),
    );
    for (name, parameters, waits) in [
        ("tween", tween, false),
        ("delay", vec![parameter("seconds", None)], false),
        (
            "clock",
            vec![
                parameter("interval", Some(bc::Const::Int(1))),
                parameter("owner", Some(bc::Const::Null)),
            ],
            false,
        ),
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
            "stage::presented",
            vec![parameter("owner", Some(bc::Const::Null))],
            false,
        ),
        (
            "completion",
            vec![parameter("owner", Some(bc::Const::Null))],
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
            abi_version: if name == "tween" { 6 } else { 1 },
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
                parameter("id", Some(bc::Const::Null)),
                parameter("params", Some(bc::Const::Null)),
                parameter("locale", Some(bc::Const::Null)),
            ],
            Effects::ALLOCATE,
            BuiltinType::String,
        ),
        (
            "reel::localization::load",
            vec![parameter("locale", None), parameter("source", None)],
            Effects::ALL.0,
            BuiltinType::Null,
        ),
        (
            "reel::localization::set_locale",
            vec![parameter("locale", None)],
            Effects::ALLOCATE | Effects::WRITE | Effects::HOST,
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
            abi_version: if symbol == "reel::tr" { 3 } else { 1 },
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
        parameters: ["node", "duration", "owner"]
            .into_iter()
            .enumerate()
            .map(|(i, name)| NativeParameter {
                name: name.into(),
                ty: BuiltinType::Any,
                default: (i != 0).then_some(bc::Const::Null),
            })
            .collect(),
        result: BuiltinType::Any,
        effects: Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST),
        required_capabilities: vec!["reel.stage".into()],
    });
    contracts.extend(aside_std::contracts());
    contracts
}

pub(super) fn capabilities() -> Vec<String> {
    vec![
        "reel.session".into(),
        "reel.stage".into(),
        "reel.audio".into(),
    ]
}
