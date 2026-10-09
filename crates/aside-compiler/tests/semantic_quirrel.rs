use aside_compiler::{semantic::*, *};
use std::path::Path;
fn inputs(dir: &Path, result: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            inputs(&p, result)
        } else if p.extension().is_some_and(|e| e == "aside") {
            result.push(p)
        }
    }
}
#[test]
fn accepted_t01_samples_parse_and_analyze_without_claiming_execution() {
    let mut paths = Vec::new();
    inputs(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/quirrel")
            .as_path(),
        &mut paths,
    );
    paths.sort();
    assert_eq!(paths.len(), 10);
    let options = AnalysisOptions {
        hosts: vec![
            ExternalSymbol::function(
                "assert",
                ExternalSignature {
                    parameters: vec![
                        ExternalParameter {
                            name: "condition".into(),
                            ty: TypeHint::Bool,
                            mutable: false,
                            default: false,
                            rest: false,
                        },
                        ExternalParameter {
                            name: "message".into(),
                            ty: TypeHint::String,
                            mutable: false,
                            default: true,
                            rest: false,
                        },
                    ],
                    result: TypeHint::Null,
                    effect: CallEffect::NoWait,
                    init_safe: Some(true),
                    derived_safe: Some(true),
                },
            ),
            ExternalSymbol::dynamic("print"),
            ExternalSymbol::dynamic("println"),
        ],
        ..Default::default()
    };
    let mut assertions = 0;
    for path in paths {
        let source = std::fs::read_to_string(&path).unwrap();
        let parsed = parse(&source).unwrap();
        assert!(
            parsed.is_ok(),
            "{} parse: {:?}",
            path.display(),
            parsed.diagnostics()
        );
        assertions += parsed
            .syntax()
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::CallExpr)
            .filter(|n| {
                n.children().next().is_some_and(|n| {
                    n.kind() == SyntaxKind::NameExpr && n.text().to_string().trim() == "assert"
                })
            })
            .count();
        let result = analyze(&parsed, &options, &mut NoModules);
        assert!(
            result.is_ok(),
            "{} analysis: {:?}",
            path.display(),
            result.diagnostics
        );
        println!(
            "{}: parse=passed analysis=passed execution=waiting_runtime",
            path.display()
        );
    }
    assert_eq!(
        assertions, 32,
        "original assertion calls must remain intact"
    );
}
