//! Editor-retained identities must remain executable through the project linker.
use aside_compiler::{
    AnalysisOptions,
    semantic::{NoModules, SourceModule, analyze_project},
};
use aside_vm::{GcBudget, GcPhase, RootOwner, SessionEpoch, bytecode as bc, executor::*};

#[test]
fn imported_class_reexports_and_bound_static_aliases_execute_in_source_and_asb() {
    let source = r#"
    use sample::api::{Counter as Imported};
    use sample::model::singleton;
    fn main() {
        let construct = Imported;
        let counter: Imported = construct(start: 5);
        let add = counter.add;
        let zero = Imported.zero;
        if counter.value != 5 || add(amount: 3) != 8 || zero() != 0 || singleton.value != 9 {
            throw "imported class identity";
        }
        counter.add() + Imported.zero()
    }
"#;
    let modules = [
        SourceModule {
            canonical_path: "sample::model".into(),
            source_path: Some("model.aside".into()),
            version: "1".into(),
            source: r#"
        pub class Counter {
            let value: int;
            fn init(self, start: int = 4) { self.value = start; }
            fn add(self, amount: int = 2) -> int { self.value + amount }
            static fn zero() -> int { 0 }
        }
        pub let singleton: Counter = Counter(start: 9);
    "#
            .into(),
        },
        SourceModule {
            canonical_path: "sample::api".into(),
            source_path: Some("api.aside".into()),
            version: "1".into(),
            source: "pub use sample::model::Counter;".into(),
        },
        SourceModule {
            canonical_path: "sample::main".into(),
            source_path: Some("main.aside".into()),
            version: "1".into(),
            source: source.into(),
        },
    ];
    let mut analysis =
        analyze_project(&modules, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
    let plan = std::mem::take(&mut analysis.plan);
    let modules = analysis.into_modules().unwrap();
    for encoded in [false, true] {
        let compiled = aside_host::lower_project(
            modules.clone(),
            &plan,
            &[],
            &[],
            &[],
            &[],
            Default::default(),
        )
        .unwrap();
        let initializer = compiled.initializer;
        let program = if encoded {
            let bytes = bc::encode(compiled.program.package(), bc::Limits::default()).unwrap();
            bc::VerifiedProgram::decode(&bytes, bc::Limits::default()).unwrap()
        } else {
            compiled.program
        };
        let main = bc::FunctionId(
            program
                .package()
                .functions
                .iter()
                .position(|function| program.package().symbols[function.name.index()] == "main")
                .unwrap() as u32,
        );
        let mut vm = Vm::load(
            program,
            bc::RuntimeSupport::default(),
            VmLimits::default(),
            SessionEpoch(1),
        )
        .unwrap();
        for function in [initializer, main] {
            let task = vm
                .start(function, &[], RootOwner(1), bc::Context::Ordinary)
                .unwrap();
            loop {
                match vm
                    .run_slice(
                        task,
                        SliceBudget {
                            instructions: 1,
                            ..Default::default()
                        },
                        || 0,
                    )
                    .unwrap()
                {
                    RunOutcome::BudgetYield(_) => {
                        if vm.heap().phase() == GcPhase::Idle {
                            vm.begin_collection().unwrap();
                        }
                        vm.gc_step(GcBudget {
                            units: 1,
                            ..Default::default()
                        })
                        .unwrap();
                    }
                    RunOutcome::Returned(delivery) => {
                        if function == main {
                            assert_eq!(vm.delivery_value(delivery).unwrap().as_int(), Some(7));
                        }
                        vm.discard_delivery(delivery).unwrap();
                        break;
                    }
                    RunOutcome::Fault(delivery) => {
                        panic!("{:?}", vm.delivery_fault(delivery).unwrap())
                    }
                    other => panic!("{other:?}"),
                }
            }
        }
    }
}
