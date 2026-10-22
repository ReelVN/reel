//! The same node property declarations feed constructor types and named host types.
use super::*;
use aside_compiler::semantic::TypeHint;
use aside_host::{
    PropertyDocumentation,
    source::{Api, Member, NamedType, named, nullable},
};

fn bindable(ty: TypeHint) -> TypeHint {
    if ty == TypeHint::Any {
        ty
    } else {
        TypeHint::Union(vec![ty, TypeHint::Named("Binding".into())])
    }
}
fn property(owner: &str, doc: PropertyDocumentation) -> Member {
    let mut member = Member::property(owner, doc);
    member.write_type = member.write_type.map(bindable);
    member
}

pub(in crate::aside) fn properties(symbol: &str) -> Vec<PropertyDocumentation> {
    fn schema<T: PropertySchema>() -> Vec<PropertyDocumentation> {
        T::PROPERTIES
            .iter()
            .enumerate()
            .map(|(index, property)| PropertyDocumentation {
                property,
                markdown: T::documentation(index),
            })
            .collect()
    }
    let mut properties = match symbol {
        "reel::image" | "reel::video" => schema::<appearance::ImageField>(),
        "reel::text" => schema::<appearance::TextField>(),
        "reel::button" => schema::<appearance::ButtonField>(),
        "reel::row" | "reel::column" => schema::<flow::FlowField>(),
        "reel::grid" => schema::<flow::GridField>(),
        "reel::camera" => schema::<camera::CameraField>(),
        "reel::scroll" => schema::<scroll::ScrollField>(),
        "reel::popup" => schema::<popup::PopupField>(),
        "reel::input" => schema::<input::InputField>(),
        "reel::slider" => schema::<controls::SliderField>(),
        "reel::toggle" | "reel::checkbox" => schema::<controls::ToggleField>(),
        "reel::radio" => schema::<controls::RadioField>(),
        "reel::text_input" | "reel::text_area" => schema::<text_input::TextInputField>(),
        "reel::particles" => schema::<super::super::particles::ParticleField>(),
        _ => Vec::new(),
    };
    properties.extend(schema::<NodeProperty>());
    properties
}

pub(in crate::aside) fn describe(api: &mut Api) {
    let node_name = "reel::types::Node";
    let node = named(node_name);
    for symbol in ["reel::node::add", "reel::node::move"] {
        let result = node.clone();
        api.refine(
            symbol,
            result,
            &[("receiver", node.clone()), ("child", node.clone())],
        );
        api.native_mut(symbol).returns_parameter("child");
    }
    for symbol in ["reel::node::remove", "reel::node::clear"] {
        api.refine(symbol, TypeHint::Null, &[("receiver", node.clone())]);
    }
    api.refine(
        "reel::node::input_value",
        TypeHint::Float,
        &[("receiver", named("reel::types::InputNode"))],
    );
    let mut members = NodeProperty::PROPERTIES
        .iter()
        .enumerate()
        .map(|(index, field)| {
            property(
                node_name,
                PropertyDocumentation {
                    property: field,
                    markdown: NodeProperty::documentation(index),
                },
            )
        })
        .collect::<Vec<_>>();
    for name in interaction::FIELDS {
        members.push(Member::field(
            node_name,
            name,
            TypeHint::Bool,
            None,
            interaction_documentation(name),
            Some(aside_vm::bytecode::Const::Bool(false)),
        ));
    }
    for name in ["add", "remove", "clear", "move"] {
        members.push(Member::method(
            api.native(&format!("reel::node::{name}"))
                .clone()
                .bound(name),
        ));
    }
    let base = NamedType::new(
        node_name,
        "A stage node with shared layout, appearance, input feedback and tree methods.",
        members,
    );
    api.types.push(base.clone());
    for &(constructor, type_name) in registry::NODE_CONSTRUCTORS {
        let symbol = format!("reel::{constructor}");
        let property_docs = properties(&symbol);
        api.native_mut(&symbol).property_parameters = property_docs
            .iter()
            .map(|doc| doc.property.name.to_owned())
            .collect();
        let mut hints = property_docs
            .iter()
            .map(|doc| {
                (
                    doc.property.name,
                    if doc.property.writable {
                        bindable(doc.property.source_type())
                    } else {
                        doc.property.source_type()
                    },
                )
            })
            .collect::<Vec<_>>();
        if api
            .native(&symbol)
            .symbol
            .signature
            .as_ref()
            .unwrap()
            .parameters
            .iter()
            .any(|p| p.name == "children")
        {
            hints.push(("children", nullable(TypeHint::Array)));
        }
        api.refine(&symbol, named(type_name), &hints);
        if constructor == "video" {
            api.refine(&symbol, named(type_name), &[("source", TypeHint::String)]);
        }
        if type_name == node_name || api.types.iter().any(|ty| ty.name == type_name) {
            continue;
        }
        let prefix = registry::node_prefix(&symbol);
        let mut members = property_docs
            .into_iter()
            .take(prefix.len())
            .map(|doc| property(type_name, doc))
            .collect::<Vec<_>>();
        let declaration = api.native(&symbol);
        for member in &mut members {
            member.declaration.property_default = declaration
                .parameter_defaults
                .get(&member.declaration.symbol.name)
                .and_then(aside_host::source::ParameterDefault::value)
                .cloned();
        }
        if constructor == "input" {
            members.push(Member::method(
                api.native("reel::node::input_value").clone().bound("value"),
            ));
        }
        if constructor == "scroll" {
            for name in scroll::METRICS {
                members.push(Member::field(
                    type_name,
                    name,
                    TypeHint::Float,
                    None,
                    "The accepted scroll viewport or content size in logical pixels.",
                    Some(aside_vm::bytecode::Const::FloatBits(0f64.to_bits())),
                ));
            }
        }
        if constructor == "text_input" {
            text_input::describe(api, &mut members);
        }
        if constructor == "video" {
            let source = members
                .iter_mut()
                .find(|member| member.declaration.symbol.name == "source")
                .unwrap();
            source.declaration.symbol.ty = TypeHint::String;
            source.write_type = None;
            api.refine(
                "reel::video::play",
                named("reel::types::Playback"),
                &[("node", named(type_name))],
            );
            members.push(Member::method(
                api.native("reel::video::play").clone().bound("play"),
            ));
        }
        api.types.push(
            NamedType::new(type_name, &api.native(&symbol).documentation, members).extends(&base),
        );
    }
    for (symbol, result) in [
        ("reel::list", node.clone()),
        ("reel::view", node.clone()),
        ("reel::layered_image", node.clone()),
    ] {
        let hints = NodeProperty::PROPERTIES
            .iter()
            .map(|property| (property.name, bindable(property.source_type())))
            .collect::<Vec<_>>();
        api.refine(symbol, result, &hints);
        api.native_mut(symbol).property_parameters = NodeProperty::PROPERTIES
            .iter()
            .map(|property| property.name.to_owned())
            .collect();
    }
    api.refine("reel::stage::replace", TypeHint::Null, &[("root", node)]);
}

fn interaction_documentation(name: &str) -> &'static str {
    match name {
        "hover" => "Whether this node or a descendant is under the pointer.",
        "pressed" => "Whether pointer capture or the node's confirmation control is pressed.",
        "focused" => "Whether this node or a descendant receives keyboard focus.",
        "focus_visible" => "Whether keyboard or gamepad navigation focus should be displayed.",
        "disabled" => "Whether this node is disabled by itself or an ancestor.",
        _ => unreachable!("registered interaction field"),
    }
}
