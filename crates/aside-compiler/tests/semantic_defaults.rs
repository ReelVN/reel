//! Real source regressions for the parameter-default lexical control boundary.
use aside_compiler::{hir::*, semantic::*, *};

fn analysis(source: &str) -> Analysis {
    let parsed = parse(source).unwrap();
    assert!(
        parsed.is_ok(),
        "parser: {source}\n{:?}",
        parsed.diagnostics()
    );
    analyze(&parsed, &AnalysisOptions::default(), &mut NoModules)
}
fn valid(source: &str) -> AnalyzedModule {
    let result = analysis(source);
    assert!(result.is_ok(), "{source}\n{:?}", result.diagnostics);
    result.into_module().unwrap()
}

#[test]
fn explicit_default_returns_are_lexically_rejected_in_every_control_path() {
    for source in [
        "fn f(x=if true{return 1;}else{2}){}",
        "fn f(x=if false{return;}else{1}){}",
        "fn f(x=false && if true{return true;}else{false}){}",
        "fn f(x=true || if true{return true;}else{false}){}",
        "fn f(x=1 ?? if true{return 2;}else{3}){}",
        "fn f(xs,x=xs?[if true{return 1;}else{0}]){}",
        "fn f(x=if true{loop{return 1;}1}else{2}){}",
        "fn f(x=match 1{_=>{return 1;},}){}",
        "fn f(x=if true{try{throw 1;}catch e{return e;}1}else{2}){}",
        "fn f(x=if true{try{}finally{return 1;}1}else{2}){}",
        "fn f(x=class{let answer=if true{return 1;}else{2};}){}",
    ] {
        let result = analysis(source);
        assert!(result.module().is_none(), "{source}");
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == DiagnosticCode::InvalidDefault),
            "{source}\n{:?}",
            result.diagnostics
        );
    }
}

#[test]
fn default_return_diagnostic_has_the_exact_source_statement_and_default_boundary() {
    let source = "fn 外层(x=if false{return \"错误\";}else{1})->int{1}";
    let result = analysis(source);
    assert!(result.module().is_none());
    let diagnostic = result
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::InvalidDefault)
        .unwrap();
    let start = source.find("return").unwrap();
    let end = source[start..].find(';').unwrap() + start + 1;
    assert_eq!(
        diagnostic.range,
        TextRange::new(
            TextSize::try_from(start).unwrap(),
            TextSize::try_from(end).unwrap()
        )
    );
    assert_eq!(diagnostic.related.len(), 1);
    assert_eq!(
        &source[usize::from(diagnostic.related[0].range.start())
            ..usize::from(diagnostic.related[0].range.end())],
        "if false{return \"错误\";}else{1}"
    );
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::TypeMismatch),
        "invalid default return must not be checked against the outer return contract"
    );
}

#[test]
fn nested_function_returns_reset_only_their_own_source_boundary() {
    let module = valid("fn f(x=||{return 1;}){}");
    let closure = module
        .functions()
        .iter()
        .find(|f| module.hir().node(f.node).kind == HirKind::Closure)
        .unwrap();
    let returning_node = module
        .hir()
        .nodes
        .iter()
        .position(|n| n.kind == HirKind::Return)
        .unwrap();
    assert_eq!(
        module.facts()[returning_node]
            .control
            .as_ref()
            .unwrap()
            .target,
        Some(closure.node),
        "nested source return targets its own function"
    );
    for source in [
        "fn f(x=if true{fn g()->int{return 1;}g()}else{2}){}",
        "fn f(x=if true{try{}finally{let callback=||{return 1;};}1}else{2}){}",
        "fn f(x=||{fn g(y=||{return 1;}){return y;}return g;}){}",
        "fn f(x=class{fn answer(self){return 1;}}){}",
    ] {
        valid(source);
    }
    let source = "fn f(x=||{fn g(y=if true{return 1;}else{2}){return y;}return g;}){}";
    let result = analysis(source);
    let defaults: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::InvalidDefault)
        .collect();
    assert_eq!(defaults.len(), 1, "{:?}", result.diagnostics);
    let start = source.find("return 1;").unwrap();
    assert_eq!(usize::from(defaults[0].range.start()), start);
    assert_eq!(
        usize::from(defaults[0].range.end()),
        start + "return 1;".len()
    );
    let boundary = defaults[0].related[0].range;
    assert_eq!(
        &source[usize::from(boundary.start())..usize::from(boundary.end())],
        "if true{return 1;}else{2}"
    );
    let normal = analysis("fn f()->int{return \"bad\";}");
    assert!(
        normal
            .diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::TypeMismatch)
    );
    assert!(
        !normal
            .diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::InvalidDefault)
    );
}

#[test]
fn bounded_default_diagnostics_do_not_relabel_an_earlier_error() {
    let source = "fn f(x=if true{missing;return 1;}else{2}){}";
    let options = AnalysisOptions {
        limits: AnalysisLimits {
            max_diagnostics: 1,
            ..Default::default()
        },
        ..Default::default()
    };
    let result = analyze_module(source, &options, &mut NoModules).unwrap();
    assert!(result.module().is_none());
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].code, DiagnosticCode::UnknownName);
    assert!(result.diagnostics[0].related.is_empty());
}

#[test]
fn default_tail_results_loops_and_exceptions_remain_valid_for_thunk_lowering() {
    let module = valid("fn f(mut a:int=1,b:int=if true{a+=1;a}else{0})->int{a+b}");
    let f = &module.functions()[0];
    let default = f.parameters[1].default.unwrap();
    assert!(matches!(
        module.types()[module.facts()[default.0 as usize].ty.0 as usize],
        Type::Int
    ));
    assert!(
        !module.hir().nodes.iter().any(|n| n.kind == HirKind::Return),
        "L03 preserves an expression result; L04 generates the thunk Return"
    );
    for source in [
        "fn f(x=if true{let mut n=0;local:while n<2{n+=1;if n==1{continue local;}break local;}n}else{2}){}",
        "fn f(x=if true{throw 1;}else{2}){}",
        "fn f(x=if true{try{throw 1;}catch e{throw;}1}else{2}){}",
    ] {
        valid(source);
    }
    let result = analysis("outer:while true{fn f(x=if true{break outer;}else{1}){}}");
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::InvalidControl)
    );
}
