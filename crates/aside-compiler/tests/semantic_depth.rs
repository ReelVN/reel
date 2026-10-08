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

#[test]
fn deep_reexport_result_drop_uses_a_bounded_stack_in_a_child_process() {
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "reexport_drop_child", "--nocapture"])
        .env("L03_REEXPORT_DROP_CHILD", "1")
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
fn reexport_drop_child() {
    if std::env::var_os("L03_REEXPORT_DROP_CHILD").is_none() {
        return;
    }
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            let mut inputs = Vec::with_capacity(10_000);
            inputs.push(SourceModule {
                canonical_path: "m0".into(),
                version: "drop-v1".into(),
                source: "pub let x=1;".into(),
            });
            for i in 1..10_000 {
                inputs.push(SourceModule {
                    canonical_path: format!("m{i}"),
                    version: "drop-v1".into(),
                    source: format!("pub use m{}::{{x}};", i - 1).into(),
                });
            }
            let start = std::time::Instant::now();
            let result =
                analyze_project(&inputs, &AnalysisOptions::default(), &mut NoModules).unwrap();
            assert!(result.is_ok(), "{:?}", result.diagnostics);
            let last = result
                .modules()
                .unwrap()
                .iter()
                .find(|m| m.canonical_path() == "m9999")
                .unwrap();
            let descriptor = &last.external_bindings()[0].descriptor;
            let mut source = descriptor.resolved_from.as_deref();
            let mut hops = 0;
            while let Some(hop) = source {
                hops += 1;
                source = hop.previous.as_deref();
            }
            assert_eq!(hops, 9999);
            assert_eq!(
                descriptor.reference.as_ref().unwrap().export.as_deref(),
                Some("x")
            );
            eprintln!("10000-module analysis_ms={}", start.elapsed().as_millis());
            let start = std::time::Instant::now();
            drop(result);
            eprintln!(
                "10000-module result drop_ms={}",
                start.elapsed().as_millis()
            );
            // A retained descriptor keeps the same immutable tail alive after the
            // module results go away; releasing its final owner must be bounded too.
            let result =
                analyze_project(&inputs, &AnalysisOptions::default(), &mut NoModules).unwrap();
            assert!(result.is_ok());
            let last = result
                .modules()
                .unwrap()
                .iter()
                .find(|m| m.canonical_path() == "m9999")
                .unwrap();
            let shared_tail = last.external_bindings()[0]
                .descriptor
                .resolved_from
                .as_ref()
                .unwrap()
                .clone();
            let start = std::time::Instant::now();
            drop(result);
            let mut origin = &shared_tail;
            let mut retained_hops = 1;
            while let Some(previous) = &origin.previous {
                retained_hops += 1;
                origin = previous;
            }
            assert_eq!(retained_hops, 9999, "shared history stays intact");
            let origin_weak = std::sync::Arc::downgrade(origin);
            drop(shared_tail);
            assert!(
                origin_weak.upgrade().is_none(),
                "the final tail is released"
            );
            eprintln!(
                "10000-module shared result drop_ms={}",
                start.elapsed().as_millis()
            );
        })
        .unwrap()
        .join()
        .unwrap();
}
