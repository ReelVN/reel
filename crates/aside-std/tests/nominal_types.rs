mod support;
use aside_host::{aside_compiler, aside_vm};
use aside_vm::{
    GcBudget, GcPhase, RootOwner, SessionEpoch, bytecode as bc,
    executor::*,
    snapshot::{SnapshotBudget, SnapshotRegistry, VmSnapshot},
};

const SOURCE: &str = r#"
use std::{random,Random};
use std::math::{curve, Curve};
fn stream(value:Random)->Random {value}
fn motion(value:Curve)->Curve {value}
fn untyped(value) {value}
fn tuple_identity(value:tuple)->tuple {value}
fn exact(value:tuple)->(int,string) {value}
fn main() {
    let rng:Random=stream(random(seed:37));
    let other=random(37);
    if !(rng is Random) || rng is Curve {throw "random nominal type";}
    let draw=rng.int;
    let alias=draw;
    if alias!=draw {throw "bound identity";}
    for index in 0..3 {
        if draw(min:0,max:1000)!=other.int(0,1000) || rng.float()!=other.float() {throw "stream state";}
    }
    let item={n:7};
    if rng.pick(items:(item,))!=item || rng.pick([item])!=item {throw "pick identity";}
    let path:Curve=motion(curve(({time:0,value:0},{time:1,value:10})));
    let sample=path.sample;
    let same_sample=sample;
    if !(path is Curve) || path is Random || sample!=same_sample || sample(time:0.25)!=2.5 {
        throw "curve nominal method";
    }
    if tuple_identity(())!=() || exact((1,"two"))!=(1,"two") || !((1,2) is tuple)
        || [] is tuple || untyped({}) is tuple {throw "generic tuple";}
    let mut rejected=0;
    try {tuple_identity(untyped([]));} catch error {rejected+=1;}
    try {exact((1,2));} catch error {rejected+=1;}
    try {stream(untyped(path));} catch error {rejected+=1;}
    if rejected!=3 {throw "runtime contracts";};
    [rng,path,draw,sample]
}
"#;

fn restore(vm: &mut Vm, task: TaskId, epoch: &mut u64) -> TaskId {
    let mut registry = SnapshotRegistry::default();
    aside_std::register_snapshots(&mut registry).unwrap();
    let mut capture = vm.snapshot_begin(&registry).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let image = VmSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
    *epoch += 1;
    let support = aside_host::runtime_support(
        vm.program(),
        &aside_std::contracts(),
        aside_std::types(),
        &[],
    )
    .unwrap();
    let mut loading = Vm::restore_begin(
        &image,
        vm.program().clone(),
        support,
        VmLimits::default(),
        SessionEpoch(*epoch),
        &registry,
    )
    .unwrap();
    while !loading.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let (loaded, map) = loading.finish().unwrap();
    *vm = loaded;
    map.task(task).unwrap()
}

#[test]
fn standard_nominal_methods_and_generic_tuple_survive_asb_gc_and_every_slice_restore() {
    let module =
        aside_compiler::analyze_module(SOURCE, &Default::default(), &mut support::resolver())
            .unwrap()
            .into_module()
            .unwrap();
    let contracts = aside_std::contracts();
    let compiled = aside_host::lower(
        &module,
        &contracts,
        &aside_std::api().functions,
        aside_std::types(),
        &[],
        Default::default(),
    )
    .unwrap();
    let entry = bc::FunctionId(
        compiled
            .program
            .package()
            .functions
            .iter()
            .position(|function| {
                compiled.program.package().symbols[function.name.index()] == "main"
            })
            .unwrap() as u32,
    );
    let bytes = bc::encode(compiled.program.package(), bc::Limits::default()).unwrap();
    for (encoded, snapshots) in [(false, false), (true, true)] {
        let program = if encoded {
            bc::VerifiedProgram::decode(&bytes, bc::Limits::default()).unwrap()
        } else {
            compiled.program.clone()
        };
        let runtime =
            aside_host::runtime_support(&program, &contracts, aside_std::types(), &[]).unwrap();
        let mut vm = Vm::load(program, runtime, VmLimits::default(), SessionEpoch(1)).unwrap();
        let mut library = aside_std::StandardLibrary::new(vm.program());
        let mut epoch = 1;
        for function in [compiled.entry, entry] {
            let mut task = vm
                .start(function, &[], RootOwner(1), bc::Context::Ordinary)
                .unwrap();
            let mut budget = SliceBudget {
                instructions: 1,
                work: 1,
                allocation_bytes: 0,
                deadline_ns: None,
            };
            for step in 0..100_000 {
                let outcome = vm.run_slice(task, budget, || 0).unwrap();
                match outcome {
                    RunOutcome::HostCall(ticket) => {
                        library.dispatch(&mut vm, ticket, 1).unwrap().unwrap();
                    }
                    RunOutcome::BudgetYield(usage) => {
                        budget.work = usage.required_work.max(1);
                        budget.allocation_bytes = usage.required_allocation;
                    }
                    RunOutcome::Returned(delivery) => {
                        if function == entry {
                            let result = vm.delivery_value(delivery).unwrap();
                            for index in 0..2 {
                                let object = vm.heap().sequence_get(result, index).unwrap();
                                assert_eq!(
                                    vm.heap()
                                        .table_len(vm.heap().host_fields(object).unwrap())
                                        .unwrap(),
                                    0,
                                    "methods live on their type, without per-instance fields"
                                );
                            }
                        }
                        vm.discard_delivery(delivery).unwrap();
                        break;
                    }
                    other => panic!(
                        "step {step}: {other:?} {:?}",
                        match other {
                            RunOutcome::Fault(id) => vm.delivery_fault(id).unwrap(),
                            _ => None,
                        }
                    ),
                }
                if vm.heap().phase() == GcPhase::Idle {
                    vm.begin_collection().unwrap();
                }
                vm.gc_step(GcBudget {
                    units: 1,
                    ..Default::default()
                })
                .unwrap();
                if snapshots {
                    task = restore(&mut vm, task, &mut epoch);
                    library = aside_std::StandardLibrary::new(vm.program());
                }
                assert!(step < 99_999, "execution must make progress");
            }
        }
    }
}

#[test]
fn precise_standard_signatures_reject_known_invalid_values_before_execution() {
    for expression in [
        "random(seed:1.0)",
        "random().int(min:0.0,max:2)",
        "random().int(0)",
        "random().float(1)",
        "random().pick(\"text\")",
        "curve({})",
        "curve([]).sample(null)",
        "random().int=null",
        "curve([]).sample=null",
    ] {
        let source = format!("use std::{{random}};use std::math::curve;fn main(){{{expression};}}");
        let analysis =
            aside_compiler::analyze_module(&source, &Default::default(), &mut support::resolver())
                .unwrap();
        assert!(
            !analysis.is_ok(),
            "invalid signature accepted: {expression}"
        );
    }
    for source in [
        "fn take(value:tuple){} fn main(){take([]);}",
        "fn take(value:tuple){} fn main(){take(3);}",
        "fn result()->tuple {[]}",
        "fn empty(value:()){} fn main(){empty((1,));}",
    ] {
        assert!(
            !aside_compiler::analyze_module(source, &Default::default(), &mut support::resolver())
                .unwrap()
                .is_ok(),
            "generic tuple widened another contract: {source}"
        );
    }
    let documents = aside_std::api().documents("std");
    let text = documents
        .documents
        .iter()
        .map(|document| document.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    for declaration in [
        "class Random",
        "class Curve",
        "fn int(min: int, max: int) -> int",
        "fn float() -> float",
        "fn pick(items: array | tuple) -> any",
        "fn sample(time: int | float) -> float",
    ] {
        assert!(text.contains(declaration), "missing {declaration}: {text}");
    }
    assert_eq!(
        aside_host::hint(&bc::BuiltinType::Tuple),
        aside_host::source::named("tuple")
    );
}
