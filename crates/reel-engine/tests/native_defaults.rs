use reel_engine::aside::source;
use std::collections::BTreeSet;

#[test]
fn registered_literal_defaults_measure_the_real_seed() {
    let contracts = source::contracts();
    let capabilities = contracts
        .iter()
        .flat_map(|contract| &contract.required_capabilities)
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let seed = aside_host::seed(contracts, source::types(), &capabilities, 1).unwrap();
    let declared = contracts
        .iter()
        .map(|contract| {
            contract
                .parameters
                .iter()
                .filter(|parameter| parameter.default.is_some())
                .count()
        })
        .sum::<usize>();
    let helpers = seed.package().functions.len() - contracts.len();
    let bytes = aside_vm::bytecode::encode(seed.package(), Default::default())
        .unwrap()
        .len();
    eprintln!(
        "registered native defaults: declarations={declared}, helpers={helpers}, native_functions={}, functions={}, signatures={}, constants={}, symbols={}, encoded_bytes={bytes}, owned_bytes={}",
        contracts.len(),
        seed.package().functions.len(),
        seed.package().signatures.len(),
        seed.package().constants.len(),
        seed.package().symbols.len(),
        seed.stats().owned_capacity_bytes
    );
    assert_eq!(
        helpers, 0,
        "registered literals must not retain default functions"
    );
    assert!(
        seed.package()
            .signatures
            .iter()
            .flat_map(|s| &s.parameters)
            .filter_map(|p| p.default)
            .all(|d| matches!(d, aside_vm::bytecode::ParameterDefault::Constant(_)))
    );
}

#[test]
fn project_constant_interning_remaps_script_and_registered_defaults_together() {
    use aside_compiler::semantic::SourceModule;
    use aside_vm::bytecode as bc;
    let modules=[
        SourceModule {canonical_path:"game::helpers".into(),source_path:Some("helpers.aside".into()),version:"1".into(),source:r#"pub fn values(a=12,b="repeated",c=if true {a+1} else {0}) {(a,b,c)} fn literals(){12;"repeated";false;0;}"#.into()},
        SourceModule {canonical_path:"game::main".into(),source_path:Some("main.aside".into()),version:"1".into(),source:r#"use game::helpers::values; fn opening(a=12,b="repeated"){let copy="repeated";let another=12;values();narrator(copy,history:false);}"#.into()},
    ];
    let compiled = source::compile_project(&modules, &Default::default(), 1).unwrap();
    for encoded in [false, true] {
        let program = if encoded {
            bc::VerifiedProgram::decode(
                &bc::encode(compiled.compilation.program.package(), Default::default()).unwrap(),
                Default::default(),
            )
            .unwrap()
        } else {
            compiled.compilation.program.clone()
        };
        let p = program.package();
        for name in ["values", "opening"] {
            let function = p
                .functions
                .iter()
                .find(|f| p.symbols[f.name.index()] == name)
                .unwrap();
            let parameters = &p.signatures[function.signature.index()].parameters;
            for (parameter, expected) in parameters
                .iter()
                .zip([bc::Const::Int(12), bc::Const::Utf8("repeated".into())])
            {
                let Some(bc::ParameterDefault::Constant(id)) = parameter.default else {
                    panic!("literal default must reference the pool")
                };
                assert_eq!(p.constants[id.index()], expected);
            }
            if name == "values" {
                assert!(matches!(
                    parameters[2].default,
                    Some(bc::ParameterDefault::Function(_))
                ));
            }
        }
        for native in &p.natives {
            let contract = source::contracts()
                .iter()
                .find(|c| c.symbol == p.symbols[native.symbol.index()])
                .unwrap();
            for (actual, registered) in p.signatures[native.signature.index()]
                .parameters
                .iter()
                .zip(&contract.parameters)
            {
                if let Some(expected) = &registered.default {
                    let Some(bc::ParameterDefault::Constant(id)) = actual.default else {
                        panic!("registered literal must reference the pool")
                    };
                    assert_eq!(
                        &p.constants[id.index()],
                        expected,
                        "{}::{}",
                        contract.symbol,
                        registered.name
                    );
                }
            }
        }
        let capabilities = source::contracts()
            .iter()
            .flat_map(|c| &c.required_capabilities)
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let support = aside_host::runtime_support(
            &program,
            source::contracts(),
            source::types(),
            &capabilities,
        )
        .unwrap();
        aside_vm::executor::Vm::load(
            program,
            support,
            Default::default(),
            aside_vm::SessionEpoch(1),
        )
        .unwrap();
    }
}
