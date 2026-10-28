//! Target selection survives host products and the portable container boundary.
use aside_compiler::semantic::SourceModule;
use aside_vm::bytecode::Const;

fn check(source: &str, target: &str) -> Result<reel_cli::Checked, reel_cli::Failure> {
    reel_cli::check_sources_with_progress(
        &[SourceModule {
            canonical_path: "main".into(),
            source_path: Some("src/main.aside".into()),
            version: "1".into(),
            source: source.into(),
        }],
        &Default::default(),
        target,
        &mut |_| {},
    )
}

#[test]
fn target_selects_code_closures_translations_and_shader_products() {
    let source = r#"
        fn tint(pixel) -> (float, float, float, float) {
            if build.target == "vita" {
                (0.25, 0.0, 0.0, 1.0)
            } else {
                pixel.sample(pixel.uv)
            }
        }
        fn main() {
            if build.target == "vita" {
                let create = || effect(tint);
                let message = tr("portable", { "zh-CN": "掌机" });
                (create(), message, 11223)
            } else {
                let message = tr("desktop", { "zh-CN": "桌面" });
                (null, message, 33445)
            }
        }
    "#;
    for (target, kept, removed, text, effects) in [
        ("vita", 11223, 33445, "portable", 1),
        ("windows", 33445, 11223, "desktop", 0),
    ] {
        let checked = check(source, target).unwrap();
        assert_eq!(checked.catalogs.messages.len(), 1);
        assert_eq!(checked.catalogs.messages[0].source, text);
        assert_eq!(checked.effects.definitions().len(), effects);
        let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
        let package = reel_package::decode(&artifact.bytes, Default::default()).unwrap();
        let constants = &package.program().package().constants;
        assert!(constants.contains(&Const::Int(kept)));
        assert!(!constants.contains(&Const::Int(removed)));
        assert_eq!(package.effects().definitions.len(), effects);
    }
}

#[test]
fn inactive_translation_keeps_source_diagnostics() {
    let source = r#"
        fn main() {
            if build.target == "vita" { 0 } else {
                tr("desktop", { "invalid_language_tag!": "bad" })
            }
        }
    "#;
    let error = check(source, "vita").err().unwrap();
    assert_eq!(error.stage, "localization");
    assert_eq!(
        error.diagnostics[0].source.as_deref(),
        Some("src/main.aside")
    );
}
