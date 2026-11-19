//! Real Function bindings survive source lowering, release ASB and engine admission.
use aside_compiler::semantic::SourceModule;
use reel_package::{self as package, EffectProducts, ErrorKind};
use std::collections::BTreeMap;

const SHADER: &str = r#"
pub fn reveal(pixel, progress: float = 0.25) -> float {
    let (r, g, b, a) = pixel.sample(pixel.uv);
    if progress > 0.5 { return a; }
    r * progress
}
pub fn tint(pixel, amount: float = 0.2) -> (float, float, float, float) {
    let (r, g, b, a) = pixel.sample(pixel.uv);
    (r * amount, g, b, a)
}
"#;

fn checked(shader: &str, alias: &str) -> reel_cli::Checked {
    let source = format!(
        r#"
use game::shader::{{reveal as {alias}, tint}};
use reel::{{effect as prepare}};
fn main() {{
    let first = prepare({alias}, {{progress: 0.4}});
    let second = prepare(function: {alias}, params: {{progress: 0.7}});
    let color = effect(tint, {{amount: 0.3}});
}}
"#
    );
    reel_cli::check_sources(
        &[
            SourceModule {
                canonical_path: "game::main".into(),
                source_path: Some("src/main.aside".into()),
                version: "1".into(),
                source: source.into(),
            },
            SourceModule {
                canonical_path: "game::shader".into(),
                source_path: Some("src/shader.aside".into()),
                version: "1".into(),
                source: shader.into(),
            },
        ],
        &BTreeMap::new(),
    )
    .unwrap()
}

#[test]
fn source_function_bindings_survive_debug_release_and_reencoding() {
    let checked = checked(SHADER, "edge");
    assert_eq!(checked.effects.bindings().count(), 2);
    assert_eq!(checked.effects.definitions().len(), 2);
    for profile in [
        reel_cli::BuildProfile::Debug,
        reel_cli::BuildProfile::Release,
    ] {
        let artifact = reel_cli::build_with_profile(&checked, Some("main::main"), profile).unwrap();
        let package = package::decode(
            &artifact.bytes,
            reel_engine::aside::source::types(),
            Default::default(),
        )
        .unwrap();
        assert_eq!(package.effects().bindings.len(), 2);
        let mapping = checked.effects.bindings().collect::<BTreeMap<_, _>>();
        for binding in &package.effects().bindings {
            let function = &package.program().package().functions[binding.function as usize];
            assert_eq!(function.module.0, binding.module);
            assert_eq!(
                mapping[&aside_vm::bytecode::FunctionId(binding.function)],
                package.effects().definitions[binding.definition as usize].id
            );
        }
        let distribution = package::distribution::Distribution {
            text: None,
            resources: vec![],
            catalogs: BTreeMap::new(),
            loose: Some(package::distribution::LooseResources::Project),
        };
        let rebound = package::encode(
            &package.program().encode(Default::default()).unwrap(),
            package.metadata(),
            Some(&distribution),
            package::Products {
                effects: package.effects(),
                resume: package.resume().table(),
                visual_plan: package.visual_plan(),
            },
            reel_engine::aside::source::types(),
            Default::default(),
        )
        .unwrap();
        let decoded = package::decode(
            &rebound,
            reel_engine::aside::source::types(),
            Default::default(),
        )
        .unwrap();
        assert_eq!(decoded.effects(), package.effects());
        let story = reel_player::distribution::story_from_package(decoded).unwrap();
        assert_eq!(
            story.effects.bindings().collect::<BTreeMap<_, _>>(),
            mapping
        );
        for definition in checked.effects.definitions() {
            let restored = story
                .effects
                .definitions()
                .find(|other| other.id == definition.id)
                .unwrap();
            assert_eq!(restored.program, definition.program);
        }
    }
}

#[test]
fn definition_ids_ignore_aliases_and_include_defaults() {
    let first = checked(SHADER, "edge");
    let renamed = checked(SHADER, "other_alias");
    assert_eq!(
        first
            .effects
            .definitions()
            .map(|d| d.id)
            .collect::<Vec<_>>(),
        renamed
            .effects
            .definitions()
            .map(|d| d.id)
            .collect::<Vec<_>>()
    );
    let changed = checked(
        &SHADER.replace("progress: float = 0.25", "progress: float = 0.5"),
        "edge",
    );
    let unchanged = first
        .effects
        .definitions()
        .filter(|d| changed.effects.definitions().any(|other| other.id == d.id))
        .count();
    assert_eq!(
        unchanged, 1,
        "only the modified definition changes identity"
    );
    let shared = reel_cli::check(
        r#"
fn first(pixel, amount: float = 0.2) -> float { amount }
fn second(pixel, amount: float = 0.2) -> float { amount }
fn main() { let a = effect(first); let b = effect(second, {amount: 0.7}); }
"#,
        "shared.aside",
    )
    .unwrap();
    assert_eq!(shared.effects.bindings().count(), 2);
    assert_eq!(
        shared.effects.definitions().len(),
        1,
        "distinct real functions share identical definitions"
    );
}

#[test]
fn package_and_engine_validate_distinct_effect_boundaries() {
    let checked = checked(SHADER, "edge");
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    let loaded = package::decode(
        &artifact.bytes,
        reel_engine::aside::source::types(),
        Default::default(),
    )
    .unwrap();
    let bytecode = loaded.program().encode(Default::default()).unwrap();
    let encode = |products: &EffectProducts| {
        package::encode(
            &bytecode,
            loaded.metadata(),
            None,
            package::Products {
                effects: products,
                resume: loaded.resume().table(),
                visual_plan: loaded.visual_plan(),
            },
            reel_engine::aside::source::types(),
            Default::default(),
        )
    };
    for mutation in 0..4 {
        let mut products = loaded.effects().clone();
        match mutation {
            0 => products.bindings[0].module = u32::MAX,
            1 => products.bindings[0].definition = u32::MAX,
            2 => products.bindings.push(products.bindings[0].clone()),
            _ => products.definitions[0].id[0] ^= 1,
        }
        assert!(matches!(
            encode(&products).unwrap_err().kind,
            ErrorKind::Metadata | ErrorKind::Integrity
        ));
    }
    let mut products = loaded.effects().clone();
    products.definitions[0].bytes = b"REELFXD1\xff\xff\xff\xff\x0f".to_vec();
    let hex = package::digest(&products.definitions[0].bytes);
    products.definitions[0].id =
        std::array::from_fn(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap());
    let bytes = encode(&products).unwrap();
    let container = package::decode(
        &bytes,
        reel_engine::aside::source::types(),
        Default::default(),
    )
    .unwrap();
    assert!(
        reel_player::distribution::story_from_package(container).is_err(),
        "typed admission must reject malformed opaque IR before publication"
    );
}

#[test]
fn canonical_collection_ignores_shadowed_names_and_rejects_runtime_captures() {
    let shadow = r#"fn effect(value, params) { value } fn main() { let result = effect(1, {}); }"#;
    let checked = reel_cli::check(shadow, "shadow.aside").unwrap();
    assert!(checked.effects.is_empty());
    let dynamic = r#"fn use_effect(function) { effect(function, {}) } fn main() {}"#;
    let failure = reel_cli::check(dynamic, "dynamic.aside").err().unwrap();
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.diagnostic.message.contains("statically resolved"))
    );
    let captured = r#"fn main() { let amount = 0.5; let value = effect(|pixel| amount, {}); }"#;
    let failure = reel_cli::check(captured, "captured.aside").err().unwrap();
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.diagnostic.message.contains("captur"))
    );
}
