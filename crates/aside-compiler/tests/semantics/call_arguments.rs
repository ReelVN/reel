//! L09 source contracts; no bytecode execution.
use super::*;

#[test]
fn rest_annotation_describes_array_binding_and_names_cannot_supply_rest() {
    for source in [
        "fn f(...r:array){r}f();f(1,\"two\",null);",
        "fn f(...mut r:array){r=[];r}f(1,2);",
        "fn f(...r:array|null){r}f();",
        "fn f(...r:any){r}f(true);",
    ] {
        valid(source);
    }
    for annotation in ["int", "bool", "table", "(int,)", "int|string"] {
        let source = format!("fn f(...r:{annotation}){{r}}");
        let a = analyze_code(&source);
        let d = a
            .diagnostics
            .iter()
            .find(|d| d.code == DiagnosticCode::TypeMismatch)
            .unwrap();
        assert!(d.message.contains("rest array binding"));
        assert_eq!(&source[byte_range(d.range)], format!("...r:{annotation}"));
    }
    for source in [
        "fn f(...r){}f(r:[]);",
        "fn f(...r){}f(1,r:[]);",
        "fn f(a=1,...r){}fn g(xs){f(...xs,r:[]);}",
    ] {
        rejects(source, DiagnosticCode::InvalidCall);
    }
}

#[test]
fn sequences_tables_or_uncertain_types_can_spread_into_calls() {
    for value in ["1", "true", "null", "\"text\"", "||1"] {
        let source = format!("fn f(g){{g(...{value})}}");
        let a = analyze_code(&source);
        let d = a
            .diagnostics
            .iter()
            .find(|d| d.code == DiagnosticCode::TypeMismatch)
            .unwrap();
        assert!(d.message.contains("array, tuple or table"));
        assert_eq!(&source[byte_range(d.range)], format!("...{value}"));
    }
    rejects(
        "fn f(g,x:int|string){g(...x)}",
        DiagnosticCode::TypeMismatch,
    );
    for source in [
        "fn f(g){g(...[],...(),...(1,2));}",
        "fn f(g,x){g(...x);}",
        "fn f(g){g(...{x:1});g(x:1,...{y:2});}",
        "fn f(g,x:table|null){g(...x);}",
        "fn f(g,x:array|null){g(...x);}",
        "fn f(g,x:any){g?(...x);}",
    ] {
        valid(source);
    }
    let m = valid("fn f(g,x:array|null){g(...x);}");
    let spread = m
        .hir()
        .nodes
        .iter()
        .position(|n| n.kind == HirKind::Spread)
        .unwrap();
    assert_ne!(m.facts()[spread].checks.0 & RuntimeChecks::TYPE, 0);
}

#[test]
fn spread_checks_apply_only_when_its_source_can_continue_with_a_value() {
    for expression in [
        "if flag{return [];}else{return [];}",
        "(if flag{return [];}else{return [];})",
        "if flag{throw 1;}else{throw 2;}",
        "if flag{try{throw 1;}finally{0;}}else{try{throw 2;}finally{0;}}",
        "if flag{return [];}else{[]}",
        "if flag{[]}else{return [];}",
        "if flag{throw 1;}else{()}",
        "if flag{()}else{throw 2;}",
    ] {
        valid(&format!(
            "fn target(...rest){{rest}}fn f(flag){{target(1,...{expression},2)}}"
        ));
    }
    for expression in [
        "null",
        "1",
        "true",
        "\"bad\"",
        "if flag{null}else{null}",
        "if flag{1}else{2}",
        "if flag{return [];}else{null}",
        "if flag{null}else{return [];}",
        "if flag{throw 1;}else{null}",
        "if flag{null}else{throw 2;}",
    ] {
        let source = format!("fn target(...rest){{rest}}fn f(flag){{target(1,...{expression},2)}}");
        let a = analyze_code(&source);
        let d = a
            .diagnostics
            .iter()
            .find(|d| d.code == DiagnosticCode::TypeMismatch)
            .unwrap();
        assert!(d.message.contains("array, tuple or table"));
        assert_eq!(&source[byte_range(d.range)], format!("...{expression}"));
        assert!(a.module().is_none());
    }
}

#[test]
fn external_rest_names_follow_the_same_positional_only_rule() {
    let host = ExternalSymbol::function(
        "host",
        ExternalSignature {
            result_parameter: None,
            result_cases: Vec::new(),
            parameters: vec![ExternalParameter {
                name: "r".into(),
                ty: TypeHint::Array,
                mutable: false,
                default: false,
                rest: true,
            }],
            result: TypeHint::Any,
            effect: CallEffect::Unknown,
            init_safe: Some(true),
            derived_safe: None,
        },
    );
    let options = AnalysisOptions {
        hosts: vec![host],
        ..Default::default()
    };
    assert!(
        analyze_module("host(1,2);", &options, &mut NoModules)
            .unwrap()
            .is_ok()
    );
    let a = analyze_module("host(r:[]);", &options, &mut NoModules).unwrap();
    assert!(
        a.diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::InvalidCall)
    );
}
