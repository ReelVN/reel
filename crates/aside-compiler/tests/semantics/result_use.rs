use super::*;

fn constructor() -> ExternalSymbol {
    ExternalSymbol {
        must_use_result: true,
        ..ExternalSymbol::dynamic("make")
    }
}

fn check(source: &str) -> Analysis {
    analyze_module(
        source,
        &AnalysisOptions {
            hosts: vec![constructor(), ExternalSymbol::dynamic("effect")],
            ..Default::default()
        },
        &mut NoModules,
    )
    .unwrap()
}

#[test]
fn unused_results_point_to_the_discarded_constructor_call() {
    for (source, expression) in [
        ("fn heading(){make();}", "make()"),
        ("make();", "make()"),
        ("make {}", "make {}"),
        ("fn f(){(make());}", "make()"),
        ("fn f(){make {} effect();}", "make {}"),
        ("fn f(){if true {make()} else {null};}", "make()"),
        ("fn f(){while false {make()}}", "make()"),
        ("fn f(){for x in [] {make()}}", "make()"),
        ("fn f(){try{make()}catch e{}}", "make()"),
        ("fn f(){try{}finally{make()}}", "make()"),
        ("fn f(){match 1 {1=>make(),_=>null};}", "make()"),
        ("fn f(){let fn_value=||{make();};}", "make()"),
    ] {
        let result = check(source);
        assert_eq!(
            result.diagnostics.len(),
            1,
            "{source}: {:?}",
            result.diagnostics
        );
        let diagnostic = &result.diagnostics[0];
        assert_eq!(
            diagnostic.code,
            DiagnosticCode::UnusedResult,
            "{source}: {diagnostic:?}"
        );
        assert_eq!(&source[byte_range(diagnostic.range)], expression);
        assert!(result.module().is_none());
    }
}

#[test]
fn used_results_and_explicit_discard_keep_existing_value_semantics() {
    for source in [
        "fn heading(){make()}",
        "fn heading(){return make();}",
        "fn heading(){let x=make();x}",
        "fn heading(){let _=make();}",
        "fn heading(){effect(make());}",
        "fn heading(){let xs=[make()];let t={node:make()};}",
        "fn heading(){if true {make()} else {null}}",
        "fn heading(){match 1 {1=>make(),_=>null}}",
        "fn heading(){||make()}",
        "fn heading(){let x=if true {make()}else{null};x}",
        "fn heading(){let _=if true {make()}else{null};}",
        "fn heading(){let x=effect {make();};x}",
        "fn heading(){effect();null}",
        "fn heading(){return null;}",
        "fn make(){42} fn heading(){make();}",
        "fn f(make){make();}",
    ] {
        let result = check(source);
        assert!(result.is_ok(), "{source}: {:?}", result.diagnostics);
    }
    let result = analyze_module(
        "{make()}",
        &AnalysisOptions {
            context: AnalysisContext::Function,
            hosts: vec![constructor()],
            ..Default::default()
        },
        &mut NoModules,
    )
    .unwrap();
    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn children_collection_stops_at_value_and_function_boundaries() {
    for source in [
        "fn f(){effect {make(); (make()); make()}}",
        "fn f(){effect {if true {make();} else {make();}}}",
        "fn f(){effect {for x in [1,2] {make();} while false {make();}}}",
        "fn f(){effect {match 1 {1=>make(),_=>{make();}}}}",
        "fn f(){effect {try{make();}catch e{make();}finally{make();}}}",
        "fn f(){effect {let x=make(); x; null;}}",
        "fn f(){effect {effect {make();};}}",
    ] {
        let result = check(source);
        assert!(result.is_ok(), "{source}: {:?}", result.diagnostics);
    }
    for source in [
        "fn f(){effect {let x=if true {make(); 1}else{0};}}",
        "fn f(){effect {effect(if true {make(); null}else{null});}}",
        "fn f(){effect {fn helper(){make();} helper();}}",
        "fn f(){effect {let callback=||{make();};}}",
        "fn f(){effect {if (if true {make();true}else{false}) {null;}}}",
        "fn f(){effect {return if true {make();null}else{null};}}",
        "fn f(){effect {match (if true {make();1}else{0}) {_=>null}}}",
    ] {
        let result = check(source);
        assert_eq!(
            result.diagnostics.len(),
            1,
            "{source}: {:?}",
            result.diagnostics
        );
        assert_eq!(result.diagnostics[0].code, DiagnosticCode::UnusedResult);
    }
}

#[test]
fn immutable_aliases_preserve_result_usage_without_guessing_dynamic_calls() {
    for source in [
        "fn f(){let constructor=make; constructor();}",
        "fn f(){let a=(make); let b=a; b();}",
        "fn f(){constructor();} let constructor=make;",
    ] {
        let result = check(source);
        assert_eq!(
            result.diagnostics.len(),
            1,
            "{source}: {:?}",
            result.diagnostics
        );
        assert_eq!(result.diagnostics[0].code, DiagnosticCode::UnusedResult);
    }
    for source in [
        "fn f(){let mut constructor=make; constructor=effect; constructor();}",
        "fn wrapper(){make()} fn f(){wrapper();}",
        "fn f(){let constructors=[make];constructors[0]();}",
        "fn f(){let object={constructor:make};object.constructor();}",
        "fn f(){let constructor=make; effect {constructor();}}",
    ] {
        let result = check(source);
        assert!(result.is_ok(), "{source}: {:?}", result.diagnostics);
    }
    let mutable_enclosing = analyze_module(
        "make();",
        &AnalysisOptions {
            context: AnalysisContext::Function,
            enclosing: vec![EnclosingBinding {
                symbol: constructor(),
                mutable: true,
            }],
            ..Default::default()
        },
        &mut NoModules,
    )
    .unwrap();
    assert!(
        mutable_enclosing.is_ok(),
        "{:?}",
        mutable_enclosing.diagnostics
    );
}

#[test]
fn imported_and_reexported_constructor_identity_keeps_the_diagnostic() {
    let mut resolver = MemoryResolver::default();
    resolver.insert(ModuleInterface {
        canonical_path: "host".into(),
        version: "1".into(),
        exports: vec![constructor()],
    });
    let source = |path: &str, source: &str| SourceModule {
        canonical_path: path.into(),
        source_path: None,
        version: "1".into(),
        source: source.into(),
    };
    let result = analyze_project(
        &[
            source("ui", "pub use host::{make as node};"),
            source("main", "use ui as api; fn heading(){api.node();}"),
        ],
        &AnalysisOptions::default(),
        &mut resolver,
    )
    .unwrap();
    assert_eq!(result.diagnostics.len(), 1, "{:?}", result.diagnostics);
    assert_eq!(result.diagnostics[0].module, "main");
    assert_eq!(
        result.diagnostics[0].diagnostic.code,
        DiagnosticCode::UnusedResult
    );
}

#[test]
fn direct_constructor_analysis_is_linear_and_charged_to_the_work_budget() {
    let mut extra_work = Vec::new();
    for count in [1_000, 10_000] {
        let source = format!("fn f(){{effect {{{}}}}}", "make();".repeat(count));
        let analyze_with = |required| {
            analyze_module(
                &source,
                &AnalysisOptions {
                    hosts: vec![
                        ExternalSymbol {
                            must_use_result: required,
                            ..constructor()
                        },
                        ExternalSymbol::dynamic("effect"),
                    ],
                    ..Default::default()
                },
                &mut NoModules,
            )
            .unwrap()
        };
        let baseline = analyze_with(false);
        let checked = analyze_with(true);
        assert!(baseline.is_ok() && checked.is_ok());
        let extra = checked.stats.work_units - baseline.stats.work_units;
        assert!(extra <= checked.stats.nodes * 3);
        assert_eq!(baseline.stats.retained_bytes, checked.stats.retained_bytes);
        eprintln!(
            "calls={count}, nodes={}, extra_work={extra}, retained_bytes={}",
            checked.stats.nodes, checked.stats.retained_bytes
        );
        extra_work.push(extra);
        let limited = analyze_module(
            &source,
            &AnalysisOptions {
                hosts: vec![constructor(), ExternalSymbol::dynamic("effect")],
                limits: AnalysisLimits {
                    max_work: baseline.stats.work_units,
                    ..Default::default()
                },
                ..Default::default()
            },
            &mut NoModules,
        )
        .unwrap();
        assert!(limited.exhausted);
        assert_eq!(limited.diagnostics[0].code, DiagnosticCode::BudgetExceeded);
    }
    assert!(extra_work[1] <= 10 * extra_work[0]);
}
