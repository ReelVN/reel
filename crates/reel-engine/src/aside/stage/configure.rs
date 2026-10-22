//! Writable node schemas generate one typed configuration method per node kind.
pub(in crate::aside) mod runtime;
use super::{registry, source};
use aside_host::{
    NativeContract, NativeParameter, PropertyDocumentation,
    source::{Api, Member, named},
};
use aside_vm::bytecode::{BuiltinType, Const, Effects};
use std::{collections::BTreeSet, sync::OnceLock};

/// A schema field's actual location in the host property's ordered storage.
/// Common fields follow the concrete node's own prefix, including through Node.
#[derive(Clone, Copy, Debug)]
pub(in crate::aside) enum FieldIndex {
    Specific(u32),
    Common(u32),
}

/// One explicitly supplied native parameter and the property it changes.
pub(in crate::aside) struct Field {
    pub parameter: u32,
    pub index: FieldIndex,
    pub documentation: PropertyDocumentation,
}

/// One native configuration entry for a complete registered node kind.
pub(in crate::aside) struct Schema {
    pub symbol: String,
    pub type_name: &'static str,
    pub fields: Vec<Field>,
}

pub(in crate::aside) fn schemas() -> &'static [Schema] {
    static SCHEMAS: OnceLock<Vec<Schema>> = OnceLock::new();
    SCHEMAS.get_or_init(|| {
        let mut seen = BTreeSet::new();
        std::iter::once(("node", "reel::types::Node"))
            .chain(registry::NODE_CONSTRUCTORS.iter().copied())
            .filter_map(|(constructor, type_name)| {
                if !seen.insert(type_name) {
                    return None;
                }
                let constructor_symbol = format!("reel::{constructor}");
                let prefix = registry::node_prefix(&constructor_symbol).len();
                let fields = source::properties(&constructor_symbol)
                    .into_iter()
                    .enumerate()
                    .filter(|(_, doc)| {
                        doc.property.writable
                            && !(constructor == "video" && doc.property.name == "source")
                    })
                    .enumerate()
                    .map(|(parameter, (index, documentation))| Field {
                        parameter: parameter as u32 + 1,
                        index: if index < prefix {
                            FieldIndex::Specific(index as u32)
                        } else {
                            FieldIndex::Common((index - prefix) as u32)
                        },
                        documentation,
                    })
                    .collect();
                Some(Schema {
                    symbol: format!("reel::{constructor}::with"),
                    type_name,
                    fields,
                })
            })
            .collect()
    })
}

pub(in crate::aside) fn schema(symbol: &str) -> Option<&'static Schema> {
    schemas().iter().find(|schema| schema.symbol == symbol)
}

pub(in crate::aside) fn contracts() -> Vec<NativeContract> {
    schemas()
        .iter()
        .map(|schema| {
            let parameters = std::iter::once(NativeParameter {
                name: "receiver".into(),
                ty: BuiltinType::Any,
                default: None,
            })
            .chain(schema.fields.iter().map(|field| NativeParameter {
                name: field.documentation.property.name.into(),
                ty: BuiltinType::Any,
                default: Some(Const::Null),
            }))
            .collect();
            NativeContract {
                symbol: schema.symbol.clone(),
                abi_version: 1,
                parameters,
                rest: None,
                result: BuiltinType::Any,
                effects: Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST),
                required_capabilities: vec!["reel.stage".into()],
            }
        })
        .collect()
}

pub(in crate::aside) fn describe(api: &mut Api) {
    for schema in schemas() {
        let owner = api
            .types
            .iter()
            .find(|ty| ty.name == schema.type_name)
            .expect("configuration node kind is registered");
        let mut hints = vec![("receiver", named(schema.type_name))];
        for field in &schema.fields {
            let name = field.documentation.property.name;
            let member = owner
                .members
                .iter()
                .find(|member| member.declaration.symbol.name == name)
                .expect("configuration schema matches the registered property");
            hints.push((
                name,
                member
                    .write_type
                    .clone()
                    .expect("configuration accepts only writable properties"),
            ));
        }
        api.refine(&schema.symbol, named(schema.type_name), &hints);
        let declaration = api.native_mut(&schema.symbol);
        declaration.returns_parameter("receiver");
        declaration.property_group(
            &format!(
                "{} properties",
                schema.type_name.rsplit("::").next().unwrap()
            ),
            schema
                .fields
                .iter()
                .filter(|field| matches!(field.index, FieldIndex::Specific(_)))
                .map(|field| &field.documentation),
        );
        declaration.property_group(
            "Node properties",
            schema
                .fields
                .iter()
                .filter(|field| matches!(field.index, FieldIndex::Common(_)))
                .map(|field| &field.documentation),
        );
        declaration.supplied_only(
            &schema
                .fields
                .iter()
                .map(|field| field.documentation.property.name)
                .collect::<Vec<_>>(),
        );
        let member = Member::method(declaration.clone().bound("with"));
        api.types
            .iter_mut()
            .find(|ty| ty.name == schema.type_name)
            .unwrap()
            .members
            .push(member);
    }
}

const DOCUMENTATION: &str = "Set the supplied writable properties together and return this same node. Omitted properties remain unchanged; an explicit null is assigned only when that property accepts null. Binding values retain the property's registered access permissions. A rejected proposal preserves the previous properties, bindings and animation ownership. Configuration uses the ordinary node lifecycle and save protocol.";

pub(in crate::aside) fn documentation(symbol: &str) -> Option<&'static str> {
    schema(symbol).map(|_| DOCUMENTATION)
}
