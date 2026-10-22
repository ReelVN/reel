use aside_host::{NativeContract, NativeParameter, native};
use aside_vm::{RootOwner, SessionEpoch, bytecode as bc, executor::*};

fn parameter(name: &str, ty: bc::BuiltinType, default: Option<bc::Const>) -> NativeParameter {
    NativeParameter {
        name: name.into(),
        ty,
        default,
    }
}
fn contract(name: &str, parameters: Vec<NativeParameter>) -> NativeContract {
    NativeContract {
        symbol: format!("sample::{name}"),
        abi_version: 1,
        parameters,
        rest: None,
        result: bc::BuiltinType::Any,
        effects: bc::Effects(0),
        required_capabilities: vec![],
    }
}
fn default(program: &bc::VerifiedProgram, native: usize, index: usize) -> bc::FunctionId {
    let package = program.package();
    package.signatures[package.natives[native].signature.index()].parameters[index]
        .default
        .unwrap()
}

#[test]
fn literal_sharing_preserves_constant_type_and_the_complete_prefix_contract() {
    use bc::{BuiltinType as T, Const as C};
    let base = vec![
        parameter("x", T::Any, None),
        parameter("value", T::Any, Some(C::Null)),
    ];
    let mut contracts = vec![
        contract("first", base.clone()),
        contract("second", base.clone()),
    ];
    let mut changed = base.clone();
    changed[0].name = "other".into();
    contracts.push(contract("name", changed));
    let mut changed = base.clone();
    changed[0].ty = T::Int;
    contracts.push(contract("prefix_type", changed));
    let mut changed = base.clone();
    changed[1].ty = T::Null;
    contracts.push(contract("result_type", changed));
    let mut changed = base.clone();
    changed[1].default = Some(C::Int(7));
    contracts.push(contract("value", changed));
    let mut changed = base;
    changed[0].default = Some(C::Int(3));
    contracts.push(contract("earlier_default", changed));
    let seed = aside_host::seed(&contracts, &[], &[], 1).unwrap();
    let shared = default(&seed, 0, 1);
    assert_eq!(shared, default(&seed, 1, 1));
    assert_eq!(
        shared,
        default(&seed, 6, 1),
        "earlier defaults are not part of the thunk's input contract"
    );
    for native in 2..6 {
        assert_ne!(shared, default(&seed, native, 1));
    }
    assert_eq!(seed.package().constants, [C::Null, C::Int(7), C::Int(3)]);
    let support = aside_host::runtime_support(&seed, &contracts, &[], &[]).unwrap();
    Vm::load(seed.clone(), support, Default::default(), SessionEpoch(1)).unwrap();
    let mut forged = seed.package().clone();
    let other = default(&seed, 5, 1);
    let signature = forged.natives[0].signature.index();
    forged.signatures[signature].parameters[1].default = Some(other);
    let forged = bc::verify(forged, Default::default()).unwrap();
    assert!(
        aside_host::runtime_support(&forged, &contracts, &[], &[]).is_err(),
        "a different registered constant cannot be substituted"
    );
    let mut forged = seed.package().clone();
    let helper_signature = forged.functions[shared.index()].signature.index();
    forged.signatures[helper_signature].effects = bc::Effects(bc::Effects::ALLOCATE);
    let forged = bc::verify(forged, Default::default()).unwrap();
    assert!(
        aside_host::runtime_support(&forged, &contracts, &[], &[]).is_err(),
        "shared literal admission retains the effect-free contract"
    );
}

#[native(symbol = "sample::first")]
fn first(receiver: i64, #[aside(default = "shared")] label: String) -> String {
    format!("{receiver}:{label}")
}
#[native(symbol = "sample::second")]
fn second(receiver: i64, #[aside(default = "shared")] label: String) -> String {
    format!("{receiver}:{label}")
}

#[test]
fn shared_literal_calls_and_dependent_script_defaults_survive_asb_gc_and_every_slice_restore() {
    let first = first::binding();
    let second = second::binding();
    let contracts = [first.contract.clone(), second.contract.clone()];
    let code = r#"fn dependent(a=4,b=a+1){a+b}
        fn main(){
            if first(7)!="7:shared" || second(receiver:8)!="8:shared" {throw "shared default";}
            let alias=first;
            if alias(9)!="9:shared" || second(10,"explicit")!="10:explicit" {throw "alias/default binding";}
            if dependent()!=9 || dependent(10)!=21 {throw "dependent script default";}
            42
        }"#;
    let module = aside_compiler::analyze_module(
        code,
        &aside_compiler::AnalysisOptions {
            hosts: contracts.iter().map(NativeContract::external).collect(),
            ..Default::default()
        },
        &mut aside_compiler::semantic::NoModules,
    )
    .unwrap()
    .into_module()
    .unwrap();
    let compilation =
        aside_host::lower(&module, &contracts, &[], &[], &[], Default::default()).unwrap();
    let bytes = bc::encode(compilation.program.package(), Default::default()).unwrap();
    let program = bc::VerifiedProgram::decode(&bytes, Default::default()).unwrap();
    assert_eq!(default(&program, 0, 1), default(&program, 1, 1));
    let main = bc::FunctionId(
        program
            .package()
            .functions
            .iter()
            .position(|function| program.package().symbols[function.name.index()] == "main")
            .unwrap() as u32,
    );
    let support = aside_host::runtime_support(&program, &contracts, &[], &[]).unwrap();
    for restore in [false, true] {
        let mut vm = Vm::load(
            program.clone(),
            support.clone(),
            Default::default(),
            SessionEpoch(1),
        )
        .unwrap();
        let mut epoch = 1;
        for entry in [compilation.entry, main] {
            let mut task = vm
                .start(entry, &[], RootOwner(1), bc::Context::Ordinary)
                .unwrap();
            let mut budget = SliceBudget {
                instructions: 1,
                work: 1,
                allocation_bytes: 0,
                deadline_ns: None,
            };
            loop {
                match vm.run_slice(task, budget, || 0).unwrap() {
                    RunOutcome::BudgetYield(usage) => {
                        budget.work = usage.required_work.max(1);
                        budget.allocation_bytes = usage.required_allocation;
                    }
                    RunOutcome::HostCall(ticket) => match ticket.native.0 {
                        0 => (first.invoke)(&mut (), &mut vm, ticket).unwrap(),
                        1 => (second.invoke)(&mut (), &mut vm, ticket).unwrap(),
                        _ => unreachable!(),
                    },
                    RunOutcome::Returned(delivery) => {
                        if entry == main {
                            assert_eq!(vm.delivery_value(delivery).unwrap().as_int(), Some(42));
                        }
                        vm.discard_delivery(delivery).unwrap();
                        break;
                    }
                    other => panic!("shared defaults failed: {other:?}"),
                }
                vm.begin_collection().unwrap();
                while vm.heap().phase() != aside_vm::GcPhase::Idle {
                    vm.gc_step(Default::default()).unwrap();
                }
                if restore {
                    use aside_vm::snapshot::{SnapshotBudget, SnapshotRegistry, VmSnapshot};
                    let registry = SnapshotRegistry::default();
                    let mut capture = vm.snapshot_begin(&registry).unwrap();
                    while !capture.step(SnapshotBudget::default()).unwrap().complete {}
                    let image =
                        VmSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
                    epoch += 1;
                    let mut loading = Vm::restore_begin(
                        &image,
                        program.clone(),
                        support.clone(),
                        Default::default(),
                        SessionEpoch(epoch),
                        &registry,
                    )
                    .unwrap();
                    while !loading.step(SnapshotBudget::default()).unwrap().complete {}
                    let (restored, map) = loading.finish().unwrap();
                    vm = restored;
                    task = map.task(task).unwrap();
                }
            }
        }
    }
}
