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
            let mut resolver = aside_compiler::semantic::MemoryResolver::default();
            for module in aside_std::modules() {
                resolver.insert(module);
            }
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
            let compiled = aside_host::lower(&module, &contracts, &[], &[], &[], Default::default()).unwrap();
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
            let support = aside_host::runtime_support(&program, &contracts, &[], &[]).unwrap();
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
