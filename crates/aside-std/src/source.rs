//! Source declarations and runtime registrations from the same standard schema.
use aside_host::{
    aside_compiler::semantic::{ModuleInterface, TypeHint},
    source::{Api, Declaration, Member, NamedType},
};
use std::sync::OnceLock;

pub(super) const RANDOM: &str = "std::Random";
pub(super) const CURVE: &str = "std::math::Curve";

pub(super) fn sequence() -> TypeHint {
    TypeHint::Union(vec![TypeHint::Array, aside_host::source::named("tuple")])
}

pub(super) fn api() -> &'static Api {
    static API: OnceLock<Api> = OnceLock::new();
    API.get_or_init(|| {
        let mut api = Api {
            functions: super::bindings()
                .into_iter()
                .map(|binding| {
                    let mut declaration =
                        Declaration::native(&binding.contract, binding.documentation);
                    declaration.symbol.signature =
                        Some((binding.source_signature)(&binding.contract));
                    declaration
                })
                .collect(),
            ..Default::default()
        };
        api.types.push(NamedType::new(
            RANDOM,
            "An independent mutable random stream. Saving restores its exact stream position.",
            ["int", "float", "pick"]
                .into_iter()
                .map(|name| {
                    Member::method(
                        api.native(&format!("std::random::{name}"))
                            .clone()
                            .bound(name),
                    )
                })
                .collect(),
        ));
        api.types.push(NamedType::new(
            CURVE,
            "An immutable scalar curve with heap-owned keys and logarithmic sampling.",
            vec![Member::method(
                api.native("std::math::curve::sample")
                    .clone()
                    .bound("sample"),
            )],
        ));
        let exports = |symbols: &[&str]| {
            symbols
                .iter()
                .map(|symbol| {
                    api.functions
                        .iter()
                        .find(|declaration| {
                            aside_host::source::canonical(&declaration.symbol) == *symbol
                        })
                        .expect("registered standard native")
                        .symbol
                        .clone()
                })
                .collect::<Vec<_>>()
        };
        api.globals = exports(&["std::random"]);
        for path in ["std", "std::prelude"] {
            let mut exports = api.globals.clone();
            if path == "std" {
                exports.push(api.types[0].symbol());
            }
            api.modules.push(ModuleInterface {
                canonical_path: path.into(),
                version: "1".into(),
                exports,
            });
        }
        api.modules.push(ModuleInterface {
            canonical_path: "std::core".into(),
            version: "1".into(),
            exports: exports(&["std::core::error_info"]),
        });
        api.modules.push(ModuleInterface {
            canonical_path: "std::time".into(),
            version: "1".into(),
            exports: exports(&["std::time::now", "std::time::parts", "std::time::format"]),
        });
        api.modules.push(ModuleInterface {
            canonical_path: "std::number".into(),
            version: "1".into(),
            exports: exports(&["std::number::format"]),
        });
        let mut math = super::math::bindings()
            .into_iter()
            .map(|binding| api.native(&binding.contract.symbol).symbol.clone())
            .collect::<Vec<_>>();
        math.push(api.native("std::math::curve").symbol.clone());
        math.push(api.types[1].symbol());
        api.modules.push(ModuleInterface {
            canonical_path: "std::math".into(),
            version: "1".into(),
            exports: math,
        });
        super::resume::describe(&mut api);
        api
    })
}
