use aside_vm::bytecode as bc;

#[test]
fn supplied_only_declarations_preserve_optional_presence_and_explicit_value_types() {
    use aside_compiler::semantic::{NoModules, TypeHint};
    use aside_host::source::{Api, Declaration, ParameterDefault};
    let contract = aside_host::NativeContract {
        symbol: "sample::record".into(),
        abi_version: 1,
        parameters: [
            ("receiver", bc::BuiltinType::Int, None),
            ("width", bc::BuiltinType::Any, Some(bc::Const::Null)),
            ("height", bc::BuiltinType::Any, Some(bc::Const::Int(41))),
            (
                "label",
                bc::BuiltinType::Any,
                Some(bc::Const::Utf8("stable".into())),
            ),
        ]
        .into_iter()
        .map(|(name, ty, default)| aside_host::NativeParameter {
            name: name.into(),
            ty,
            default,
        })
        .collect(),
        rest: None,
        result: bc::BuiltinType::Int,
        effects: bc::Effects(bc::Effects::HOST),
        required_capabilities: Vec::new(),
    };
    let mut declaration = Declaration::native(&contract, "Record supplied properties.").types(
        TypeHint::Int,
        &[
            ("width", TypeHint::Int),
            ("height", TypeHint::Int),
            ("label", TypeHint::String),
        ],
    );
    declaration.supplied_only(&["width"]);
    assert_eq!(contract.parameters[1].default, Some(bc::Const::Null));
    assert!(matches!(
        declaration.parameter_defaults.get("width"),
        Some(ParameterDefault::Omitted)
    ));
    assert_eq!(
        declaration.parameter_defaults["height"].value(),
        Some(&bc::Const::Int(41))
    );
    let api = Api {
        functions: vec![declaration.clone()],
        ..Default::default()
    };
    let documents = api.documents("sample");
    assert!(documents.documents.iter().any(|document| {
        document
            .text
            .contains("width?: int, height: int = 41, label: string = \"stable\"")
    }));
    let options = aside_compiler::AnalysisOptions {
        hosts: vec![declaration.symbol],
        ..Default::default()
    };
    let valid = aside_compiler::analyze_module(
        "fn main(){record(7);record(7,width:3);}",
        &options,
        &mut NoModules,
    )
    .unwrap();
    assert!(valid.is_ok(), "{:?}", valid.diagnostics);
    let source = "fn main(){record(7,width:null);}";
    let invalid = aside_compiler::analyze_module(source, &options, &mut NoModules).unwrap();
    assert!(
        invalid.diagnostics.iter().any(|diagnostic| diagnostic.code
            == aside_compiler::DiagnosticCode::TypeMismatch
            && &source[usize::from(diagnostic.range.start())..usize::from(diagnostic.range.end())]
                == "width:null"),
        "{:?}",
        invalid.diagnostics
    );
}
