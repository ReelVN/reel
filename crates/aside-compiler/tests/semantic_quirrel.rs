use aside_compiler::{semantic::*, *};
use std::path::Path;
fn significant(source: &str) -> Vec<String> {
    lex(source)
        .unwrap()
        .tokens
        .into_iter()
        .filter(|token| !token.kind.is_trivia())
        .map(|token| token.text(source).to_owned())
        .collect()
}
fn original_assertion(source: &str, mapping: &serde_json::Value) -> Vec<String> {
    let tokens = lex(source).unwrap().tokens;
    let mut translated = source.to_owned();
    if let Some(edits) = mapping["syntax_edits"].as_array() {
        for edit in edits {
            assert_eq!(edit["kind"], "interpolation_prefix");
            let offset = edit["offset"].as_u64().unwrap() as usize;
            assert!(
                tokens
                    .windows(2)
                    .any(|pair| pair[0].kind == SyntaxKind::ErrorToken
                        && pair[0].text(source) == "$"
                        && u32::from(pair[0].range.start()) as usize == offset
                        && pair[1].kind == SyntaxKind::StringStart
                        && pair[0].range.end() == pair[1].range.start())
            );
            translated.replace_range(offset..offset + 1, "f");
        }
    }
    significant(&translated)
}
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
fn migrated_sources_preserve_original_assertions_and_analyze() {
    let mut paths = Vec::new();
    inputs(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/quirrel")
            .as_path(),
        &mut paths,
    );
    paths.sort();
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/quirrel");
    let provenance: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture_root.join("provenance.json")).unwrap())
            .unwrap();
    let mut registered: Vec<_> = provenance["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| fixture_root.join(entry["path"].as_str().unwrap()))
        .collect();
    registered.sort();
    assert_eq!(paths, registered);
    let options = AnalysisOptions {
        hosts: vec![
            ExternalSymbol::function(
                "assert",
                ExternalSignature {
                    result_cases: Vec::new(),
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
        let calls: Vec<_> = parsed
            .syntax()
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::CallExpr)
            .filter(|n| {
                n.children().next().is_some_and(|n| {
                    n.kind() == SyntaxKind::NameExpr && n.text().to_string().trim() == "assert"
                })
            })
            .map(|n| significant(&n.text().to_string()))
            .collect();
        let mapping: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path.with_extension("map.json")).unwrap())
                .unwrap();
        let original = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/quirrel/original")
                .join(mapping["source"].as_str().unwrap()),
        )
        .unwrap();
        let range = |value: &serde_json::Value| {
            value[0].as_u64().unwrap() as usize..value[1].as_u64().unwrap() as usize
        };
        let expected: Vec<_> = mapping["assertions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|assertion| {
                let original_tokens =
                    original_assertion(&original[range(&assertion["original"])], assertion);
                assert_eq!(
                    original_tokens,
                    significant(&source[range(&assertion["aside"])])
                );
                original_tokens
            })
            .collect();
        assert_eq!(
            calls,
            expected,
            "original assertions changed in {}",
            path.display()
        );
        for segment in mapping["segments"].as_array().unwrap() {
            if segment["kind"] == "copied" {
                assert_eq!(
                    &original[range(&segment["original"])],
                    &source[range(&segment["aside"])]
                );
            }
        }
        assertions += calls.len();
        let result = analyze(&parsed, &options, &mut NoModules);
        assert!(
            result.is_ok(),
            "{} analysis: {:?}",
            path.display(),
            result.diagnostics
        );
        println!(
            "{}: parse=passed analysis=passed execution=checked_by_quirrel_vm",
            path.display()
        );
    }
    assert_eq!(
        assertions, 140,
        "original assertion calls must remain intact"
    );
}
