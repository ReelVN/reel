use aside_compiler::{semantic::*, *};
#[test]
fn bounded_semantic_depth_and_diagnostics_in_a_child_process() {
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "semantic_budget_child", "--nocapture"])
        .env("L03_BUDGET_CHILD", "1")
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "{:?}: {}",
        child.status,
        String::from_utf8_lossy(&child.stderr)
    );
}
#[test]
fn semantic_budget_child() {
    if std::env::var_os("L03_BUDGET_CHILD").is_none() {
        return;
    }
    let source = "if true{}else ".repeat(50000) + "if true{}";
    let parsed = parse_with_options(
        &source,
        ParseOptions {
            entry: EntryPoint::Expression,
            limits: Limits {
                max_diagnostics: 4,
                ..Default::default()
            },
        },
    )
    .unwrap();
    let result = analyze(&parsed, &AnalysisOptions::default(), &mut NoModules);
    assert!(result.exhausted);
    assert!(result.module().is_none());
    assert!(result.diagnostics.len() <= 4);
    let source = "{".repeat(64) + "let 中文=(1,2);" + &"}".repeat(64);
    let parsed = parse(&source).unwrap();
    assert!(parsed.is_ok());
    let result = analyze(&parsed, &AnalysisOptions::default(), &mut NoModules);
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    drop(result);
    drop(parsed);
    let source = "missing;".repeat(50000);
    let options = AnalysisOptions {
        limits: AnalysisLimits {
            max_diagnostics: 4,
            ..Default::default()
        },
        ..Default::default()
    };
    let result = analyze_module(&source, &options, &mut NoModules).unwrap();
    assert_eq!(result.diagnostics.len(), 4);
    assert!(result.module().is_none());
    assert!(!result.exhausted);
    for diagnostic in result.diagnostics {
        assert!(usize::from(diagnostic.range.end()) <= source.len());
    }
}
