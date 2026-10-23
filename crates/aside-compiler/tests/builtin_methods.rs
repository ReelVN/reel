use aside_compiler::{AnalysisOptions, analyze_module, lowering, semantic::NoModules};
use aside_vm::bytecode as bc;

#[test]
fn builtin_member_contracts_are_source_metadata_and_never_external_native_calls() {
    let code = r#"fn search(values:array, tuple_value:tuple) {
        let find=values.find;let contains=tuple_value.contains;
        let found:int|null=find(value:42);
        let present:bool=contains(value:42);
        let fixed:bool=(1,"two").contains(1);
        values.resize(length:4);values.append(1,2);
        let sliced:tuple=tuple_value.slice(0..0);
        let parts:array="a:b".split(separator:":");
        (found,present,fixed,sliced,parts)
    } fn main(){search([42],(42,));}"#;
    let analysis = analyze_module(code, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
    let compiled = lowering::compile(
        &analysis.into_module().unwrap(),
        &lowering::Linkage::default(),
        lowering::Options::default(),
    )
    .unwrap();
    let package = compiled.program.package();
    assert!(package.natives.is_empty());
    assert!(package.capabilities.is_empty());
    for function in &package.functions {
        for descriptor in &function.descriptors {
            if let bc::Descriptor::Call(call) = descriptor {
                assert!(!matches!(call.target, bc::CallTarget::Native(_)));
            }
        }
    }
}

#[test]
fn definite_builtin_calls_report_static_argument_errors_and_readonly_members() {
    for body in [
        "[1].find();",
        "[1].contains(other:1);",
        "[1].find(1,2);",
        "(1,2).contains();",
        "\"abc\".starts_with(null);",
        "[1].insert(value:2);",
        "[1].resize(unknown:2);",
        "[1].contains=||true;",
    ] {
        let code = format!("fn main(){{{body}}}");
        let analysis = analyze_module(&code, &AnalysisOptions::default(), &mut NoModules).unwrap();
        assert!(!analysis.is_ok(), "must diagnose {body}");
        assert!(!analysis.diagnostics.is_empty());
        assert!(
            analysis
                .diagnostics
                .iter()
                .all(|d| usize::from(d.range.end()) <= code.len())
        );
    }
}

#[test]
fn dynamic_table_fields_keep_their_own_function_contracts_and_primitive_locals_remain_editable() {
    let code = r#"fn main() {
        let table={len:|value|value,remove:||42};
        if table.len(7)!=7 || table.remove()!=42 {throw "own fields";}
        let local=[1];local.append(2);local.reverse();local.sort();
    }"#;
    let analysis = analyze_module(code, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
}

#[test]
fn misspelled_builtin_members_report_the_member_name() {
    for (parameter, access) in [
        ("value:array", "value.apend(1)"),
        ("value:string", "value.strats_with(\"a\")"),
        ("value:tuple", "value.lenght()"),
        ("value:array|null", "value?.apend(1)"),
    ] {
        let code = format!("fn main({parameter}){{{access};}}");
        let analysis = analyze_module(&code, &AnalysisOptions::default(), &mut NoModules).unwrap();
        assert_eq!(analysis.diagnostics.len(), 1, "{:?}", analysis.diagnostics);
        let diagnostic = &analysis.diagnostics[0];
        assert_eq!(
            diagnostic.code,
            aside_compiler::DiagnosticCode::UnknownMember
        );
        assert_eq!(
            &code[diagnostic.range],
            access.split('.').nth(1).unwrap().split('(').next().unwrap()
        );
    }
    let code =
        "fn main(value:table, dynamic:any){value.apend=||null; value.apend(); dynamic.apend();}";
    assert!(
        analyze_module(code, &AnalysisOptions::default(), &mut NoModules)
            .unwrap()
            .is_ok()
    );
}
