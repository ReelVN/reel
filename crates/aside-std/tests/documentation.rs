mod support;
use aside_host::{aside_compiler, aside_vm};
use aside_vm::{RootOwner, SessionEpoch, bytecode as bc, executor::*};

#[test]
fn source_documentation_examples_execute_against_registered_signatures() {
    let contracts = aside_std::contracts();
    for entry in aside_std::documentation() {
        assert!(
            !entry.markdown.trim().is_empty(),
            "{} has no documentation",
            entry.contract.symbol
        );
        for example in aside_compiler::documentation::examples(entry.markdown) {
            let context = format!(
                "{} documentation line {}",
                entry.contract.symbol, example.line
            );
            let mut resolver = support::resolver();
            let analyzed = aside_compiler::analyze_module(
                example.source,
                &aside_compiler::AnalysisOptions {
                    hosts: aside_std::prelude(),
                    ..Default::default()
                },
                &mut resolver,
            )
            .unwrap();
            let module = analyzed
                .into_module()
                .unwrap_or_else(|error| panic!("{context}: {error:?}"));
            let compiled = aside_host::lower(
                &module,
                &contracts,
                &aside_std::api().functions,
                aside_std::types(),
                &[],
                Default::default(),
            )
            .unwrap();
            let initializer = compiled.entry;
            let bytes = bc::encode(compiled.program.package(), bc::Limits::default()).unwrap();
            let program = bc::VerifiedProgram::decode(&bytes, bc::Limits::default()).unwrap();
            let main = bc::FunctionId(
                program
                    .package()
                    .functions
                    .iter()
                    .position(|function| program.package().symbols[function.name.index()] == "main")
                    .expect("documentation examples declare main") as u32,
            );
            let support =
                aside_host::runtime_support(&program, &contracts, aside_std::types(), &[]).unwrap();
            let mut library = aside_std::StandardLibrary::new(&program);
            let mut vm = Vm::load(program, support, VmLimits::default(), SessionEpoch(1)).unwrap();
            for function in [initializer, main] {
                let task = vm
                    .start(function, &[], RootOwner(1), bc::Context::Ordinary)
                    .unwrap();
                loop {
                    match vm.run_slice(task, SliceBudget::default(), || 0).unwrap() {
                        RunOutcome::HostCall(ticket) => {
                            assert!(
                                library.dispatch(&mut vm, ticket, 64).unwrap().is_some(),
                                "{context}"
                            );
                        }
                        RunOutcome::BudgetYield(_) => {}
                        RunOutcome::Returned(delivery) => {
                            vm.discard_delivery(delivery).unwrap();
                            break;
                        }
                        outcome => panic!("{context}: {outcome:?}"),
                    }
                }
            }
        }
    }
}

#[test]
fn generated_declarations_keep_validated_numbers_and_known_table_results_precise() {
    use aside_compiler::semantic::TypeHint;
    let api = aside_std::api();
    let contracts = aside_std::contracts();
    let mut math_functions = 0;
    for declaration in &api.functions {
        let symbol = aside_host::source::canonical(&declaration.symbol);
        if symbol.starts_with("std::math::") && !symbol.starts_with("std::math::curve") {
            math_functions += 1;
            let source = declaration.symbol.signature.as_ref().unwrap();
            assert_eq!(source.result, TypeHint::Float, "{symbol}");
            let runtime = contracts
                .iter()
                .find(|contract| contract.symbol == symbol)
                .unwrap();
            assert_eq!(
                source.parameters.len(),
                runtime.parameters.len(),
                "{symbol}"
            );
            for (author, abi) in source.parameters.iter().zip(&runtime.parameters) {
                assert_eq!(
                    author.ty,
                    aside_host::source::number(),
                    "{symbol}::{}",
                    author.name
                );
                assert_eq!(
                    abi.ty,
                    bc::BuiltinType::Any,
                    "Rust validates numeric inputs at the same runtime boundary"
                );
            }
        }
    }
    assert_eq!(math_functions, 20);
    assert_eq!(
        api.native("std::time::parts")
            .symbol
            .signature
            .as_ref()
            .unwrap()
            .result,
        TypeHint::Table
    );
    assert_eq!(
        api.native("std::random::pick")
            .symbol
            .signature
            .as_ref()
            .unwrap()
            .result,
        TypeHint::Any,
        "a selected sequence element retains its arbitrary type and identity"
    );
    let documents = api.documents("std");
    let generated = documents
        .documents
        .iter()
        .map(|document| document.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    for signature in [
        "fn sin(value: int | float) -> float",
        "fn atan2(y: int | float, x: int | float) -> float",
        "fn hypot(x: int | float, y: int | float) -> float",
        "fn pow(base: int | float, exponent: int | float) -> float",
        "fn parts(timestamp: int, timezone: string | null = null) -> table",
    ] {
        assert!(
            generated.contains(signature),
            "missing {signature}: {generated}"
        );
    }
}
