use aside_compiler::{hir::*, semantic::*, *};
fn input(path: &str, source: &str) -> SourceModule {
    SourceModule {
        canonical_path: path.into(),
        version: "test-v1".into(),
        source: source.into(),
    }
}
#[test]
fn source_modules_link_public_signatures_reexports_and_namespaces() {
    let inputs = [
        input(
            "game::a",
            "let secret=7; pub fn add(mut x:int,y:int=1,...rest)->int{x+y}",
        ),
        input("game::b", "pub use game::a::{add as sum};"),
        input("game::c", "use game::b as api; let n=api.sum(1,y:2);"),
    ];
    let result = analyze_project(&inputs, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    assert_eq!(
        result.plan.initialization_order,
        ["game::a", "game::b", "game::c"]
    );
    let m = &result.modules().unwrap()[2];
    assert!(
        m.external_bindings()
            .iter()
            .filter_map(|e| e.descriptor.signature.as_ref())
            .any(|s| s.parameters[0].mutable && s.parameters[1].default && s.parameters[2].rest)
    );
    let invalid = [
        inputs[0].clone(),
        input("game::c", "use game::a::{secret};"),
    ];
    let result = analyze_project(&invalid, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(!result.is_ok());
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.diagnostic.code == DiagnosticCode::ModuleResolution)
    );
    let invalid = [
        inputs[0].clone(),
        input("game::c", "use game::a::{add}; add(z:2);"),
    ];
    let result = analyze_project(&invalid, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(!result.is_ok());
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.diagnostic.code == DiagnosticCode::InvalidCall)
    );
}
#[test]
fn function_scc_and_global_initialization_cycles_keep_runtime_checks() {
    let inputs = [
        input("game::b", "use game::a::{a}; pub fn b(){a()} pub let bv=1;"),
        input(
            "game::a",
            "use game::b::{b,bv}; pub fn a(){b()} pub let av=bv;",
        ),
    ];
    let result = analyze_project(&inputs, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    assert_eq!(result.plan.components, vec![vec!["game::a", "game::b"]]);
    assert!(
        result
            .modules()
            .unwrap()
            .iter()
            .flat_map(|m| m.facts())
            .any(|f| f.checks.0 & RuntimeChecks::INITIALIZED != 0)
    );
}
#[test]
fn wildcard_collisions_and_explicit_symbol_ambiguity() {
    let mut r = MemoryResolver::default();
    r.insert(ModuleInterface {
        canonical_path: "lib".into(),
        version: "v".into(),
        exports: vec![ExternalSymbol::dynamic("x"), ExternalSymbol::dynamic("m")],
    });
    r.insert(ModuleInterface {
        canonical_path: "lib::m".into(),
        version: "v".into(),
        exports: vec![],
    });
    let result =
        analyze_module("use lib::*; let x=1;", &AnalysisOptions::default(), &mut r).unwrap();
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::DuplicateBinding)
    );
    let result = analyze_module("use lib::m;", &AnalysisOptions::default(), &mut r).unwrap();
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::ModuleResolution)
    );
    let result = analyze_module("use lib::{m}; m();", &AnalysisOptions::default(), &mut r).unwrap();
    assert!(result.is_ok(), "{:?}", result.diagnostics);
}
#[test]
fn recursive_namespace_reexports_are_references_not_recursively_copied_trees() {
    let inputs = [
        input("game::a", "pub use game::b as b; pub fn a(){}"),
        input("game::b", "pub use game::a as a; pub fn b(){}"),
        input("game::c", "use game::a as api; api.b.a.b.a.a();"),
    ];
    let result = analyze_project(&inputs, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(result.is_ok(), "{:?}", result.diagnostics);
}
#[test]
fn public_destructuring_exports_only_its_own_names() {
    let inputs = [
        input(
            "game::a",
            "pub let {x}={x:1}; pub let callback=||{let hidden=1;hidden};",
        ),
        input("game::b", "use game::a::{x,callback}; callback();"),
    ];
    let r = analyze_project(&inputs, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(r.is_ok(), "{:?}", r.diagnostics);
    let invalid = [
        inputs[0].clone(),
        input("game::b", "use game::a::{hidden};"),
    ];
    let r = analyze_project(&invalid, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(
        r.diagnostics
            .iter()
            .any(|d| d.diagnostic.code == DiagnosticCode::ModuleResolution)
    );
}
#[test]
fn project_resource_budgets_are_total_across_modules() {
    let inputs = [input("a", "let a=1;"), input("b", "let b=1;")];
    let options = AnalysisOptions {
        limits: AnalysisLimits {
            max_symbols: 1,
            ..Default::default()
        },
        ..Default::default()
    };
    let r = analyze_project(&inputs, &options, &mut NoModules).unwrap();
    assert!(r.exhausted);
    assert!(!r.is_ok());
    let options = AnalysisOptions {
        limits: AnalysisLimits {
            max_total_source_bytes: 1,
            ..Default::default()
        },
        ..Default::default()
    };
    let r = analyze_project(&inputs, &options, &mut NoModules).unwrap();
    assert!(r.exhausted);
}
#[test]
fn qualified_native_or_class_types_follow_resolved_namespace_identity() {
    let inputs = [
        input("game::types", "pub class C{}"),
        input(
            "game::main",
            "use game::types as t; fn f(x:t::C){x} let c=t.C(); f(c);",
        ),
    ];
    let r = analyze_project(&inputs, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(r.is_ok(), "{:?}", r.diagnostics);
}
