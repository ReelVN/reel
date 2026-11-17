//! Expected signatures bind direct lambda parameters before checking the body.
use super::*;

#[test]
fn direct_lambdas_inherit_script_and_callable_argument_contracts() {
    for source in [
        "class Item{let label:string=\"item\";}fn use_it(build:fn(Item)->string){}use_it(|item|item.label);",
        "class Item{let label:string=\"item\";}fn use_it(build:fn(Item)->string){}use_it(build:(|item|item.label));",
        "fn use_it(build:(fn(string)->int)|null){}use_it(|label|label.byte_len());use_it(null);",
        "fn forward(use_it:fn(fn(string)->int)->null){use_it(|label|label.byte_len());}",
        "class Factory{fn apply(self,build:fn(string)->int){}}let maker=Factory();maker.apply(|label|label.byte_len());",
        "class Factory{fn init(self,build:fn(string)->int){}}Factory(|label|label.byte_len());",
        "fn use_it(build:fn(int)->int){}use_it(|n:any|n.unknown);",
        "fn use_it(build:fn(int)->fn(string)->int){}use_it(|n| |label|n+label.byte_len());",
    ] {
        valid(source);
    }
    for (source, code, expression) in [
        (
            "class Item{let label:string=\"item\";}fn use_it(build:fn(Item)->string){}use_it(|item|item.missing);",
            DiagnosticCode::UnknownMember,
            "missing",
        ),
        (
            "fn use_it(build:fn(string)->int){}use_it(|label|label.missing);",
            DiagnosticCode::UnknownMember,
            "missing",
        ),
        (
            "fn use_it(build:fn(int)->string){}use_it(|n|42);",
            DiagnosticCode::TypeMismatch,
            "42",
        ),
        (
            "fn use_it(build:fn(int)->string){}use_it(|n|{return 42;});",
            DiagnosticCode::TypeMismatch,
            "42",
        ),
    ] {
        let analysis = analyze_code(source);
        let diagnostic = analysis
            .diagnostics
            .iter()
            .find(|d| d.code == code)
            .unwrap_or_else(|| panic!("{source}: {:?}", analysis.diagnostics));
        assert_eq!(&source[byte_range(diagnostic.range)], expression);
        assert!(analysis.module().is_none());
    }
}

#[test]
fn annotated_bindings_assignments_defaults_and_returns_contextualize_lambdas() {
    valid(
        r#"
        let mut render:fn(string)->int=|label|label.byte_len();
        render=|label|label.byte_len()+1;
        fn explicit()->fn(string)->int{return |label|label.byte_len();}
        fn implicit()->fn(string)->int{|label|label.byte_len()}
        fn fallback(build:fn(string)->int=|label|label.byte_len()){}
        class Holder{let mut render:fn(string)->int=|label|label.byte_len();}
        fn edit(holder:Holder){holder.render=|label|label.byte_len()+1;}
    "#,
    );
    for source in [
        "let render:fn(string)->int=|label|label.missing;",
        "let mut render:fn(string)->int=|label|label.byte_len();render=|label|label.missing;",
        "fn explicit()->fn(string)->int{return |label|label.missing;}",
        "fn implicit()->fn(string)->int{|label|label.missing}",
        "fn fallback(build:fn(string)->int=|label|label.missing){}",
        "class Holder{let render:fn(string)->int=|label|label.missing;}",
        "class Holder{let mut render:fn(string)->int=|label|label.byte_len();}fn edit(holder:Holder){holder.render=|label|label.missing;}",
    ] {
        rejects(source, DiagnosticCode::UnknownMember);
    }
}

#[test]
fn explicit_annotations_arity_and_ambiguous_signatures_keep_existing_rules() {
    for source in [
        "fn use_it(build:fn(string)->int){}use_it(|label:int|label);",
        "fn use_it(build:fn(string)->int){}use_it(||1);",
        "fn use_it(build:fn(string)->int){}use_it(|label,other|1);",
        "let render:fn(string)->int=|label|\"wrong\";",
    ] {
        rejects(source, DiagnosticCode::TypeMismatch);
    }
    for source in [
        "fn use_it(build:(fn(string)->int)|(fn(int)->int)){}use_it(|value|value.unknown);",
        "fn use_it(build:(fn(string)->int)|any){}use_it(|value|value.unknown);",
        "fn use_it(first,build:fn(string)->int){}fn forward(args){use_it(...args,|value|value.unknown);}",
        "fn use_it(build:fn(string)->int){}let indirect=|value|value.unknown;use_it(indirect);",
    ] {
        valid(source);
    }
    valid("fn use_it(first,build:fn(string)->int){}use_it(...(1,),|value|value.byte_len());");
    rejects(
        "fn use_it(first,build:fn(string)->int){}use_it(...(1,),|value|value.unknown);",
        DiagnosticCode::UnknownMember,
    );
}

#[test]
fn native_named_callable_parameters_share_contextual_binding() {
    let host = ExternalSymbol::function(
        "host",
        ExternalSignature {
            parameters: vec![ExternalParameter {
                name: "build".into(),
                ty: TypeHint::Union(vec![
                    TypeHint::Callable(vec![TypeHint::String], Box::new(TypeHint::Int)),
                    TypeHint::Null,
                ]),
                mutable: false,
                default: true,
                rest: false,
            }],
            result: TypeHint::Null,
            result_cases: Vec::new(),
            result_parameter: None,
            effect: CallEffect::NoWait,
            init_safe: Some(true),
            derived_safe: Some(true),
        },
    );
    let options = AnalysisOptions {
        hosts: vec![host],
        ..Default::default()
    };
    for source in ["host(build:|label|label.byte_len());", "host();host(null);"] {
        let analysis = analyze_module(source, &options, &mut NoModules).unwrap();
        assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
        if let Some(function) = analysis
            .module()
            .unwrap()
            .functions()
            .iter()
            .find(|f| analysis.module().unwrap().hir().node(f.node).kind == HirKind::Closure)
        {
            assert!(!function.declared_context.no_wait);
        }
    }
    let analysis = analyze_module(
        "host(build:|label|label.missing);",
        &options,
        &mut NoModules,
    )
    .unwrap();
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::UnknownMember),
        "{:?}",
        analysis.diagnostics
    );
}
