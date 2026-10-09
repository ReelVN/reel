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
fn only_array_tuple_or_uncertain_types_can_spread_into_calls() {
    for value in ["1", "true", "null", "\"text\"", "{x:1}", "||1"] {
        let source = format!("fn f(g){{g(...{value})}}");
        let a = analyze_code(&source);
        let d = a
            .diagnostics
            .iter()
            .find(|d| d.code == DiagnosticCode::TypeMismatch)
            .unwrap();
        assert!(d.message.contains("array or tuple"));
        assert_eq!(&source[byte_range(d.range)], format!("...{value}"));
    }
    rejects(
        "fn f(g,x:int|string){g(...x)}",
        DiagnosticCode::TypeMismatch,
    );
    for source in [
        "fn f(g){g(...[],...(),...(1,2));}",
        "fn f(g,x){g(...x);}",
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
fn external_rest_names_follow_the_same_positional_only_rule() {
    let host = ExternalSymbol::function(
        "host",
        ExternalSignature {
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
