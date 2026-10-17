use aside_host::{aside_compiler, aside_vm};
use aside_vm::{GcBudget, GcPhase, RootOwner, SessionEpoch, bytecode as bc, executor::*};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::alloc::System;

// One test owns this process, so native allocation measurements are isolated.
#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const SOURCE: &str = r#"
use std::random;
fn detached() { random(seed: 19).int }
fn main() {
    let rng = random(seed: -42);
    let draw = rng.int;
    let values = [];
    for index in 0..32 {
        values.append(draw(-9223372036854775807, 9223372036854775807));
        values.append(rng.float());
    }
    let mut rejected = 0;
    try { draw(3, 3); } catch error { rejected += 1; }
    try { draw(4, -4); } catch error { rejected += 1; }
    try { rng.pick([]); } catch error { rejected += 1; }
    try { rng.pick("text"); } catch error { rejected += 1; }
    try { random(seed: "text"); } catch error { rejected += 1; }
    try { rng.int = null; } catch error { rejected += 1; }
    if rejected != 6 { throw "random error boundaries"; }
    let item = {count: 0};
    let pick = rng.pick;
    pick((item,)).count = 8;
    pick([item]).count += 1;
    if item.count != 9 { throw "pick copied its element"; }
    let other = random(seed: -42);
    if other.int(-9223372036854775807, 9223372036854775807) != values[0] {
        throw "stream independence";
    }
    let retained = detached();
    values.append(retained(0, 1000));
    values.append(retained(0, 1000));
    [values, rng]
}
fn forbidden(rng) { rng.int(0, 100); }
fn invalid(rng) { rng.int(3, 3); }
fn resume(rng) { rng.int(0, 100) }
"#;

fn drive(vm: &mut Vm, library: &mut aside_std::StandardLibrary, task: TaskId) -> RunOutcome {
    for _ in 0..100_000 {
        let outcome = vm
            .run_slice(
                task,
                SliceBudget {
                    instructions: 3,
                    ..Default::default()
                },
                || 0,
            )
            .unwrap();
        if vm.heap().phase() == GcPhase::Idle {
            vm.begin_collection().unwrap();
        }
        vm.gc_step(GcBudget {
            units: 1,
            ..Default::default()
        })
        .unwrap();
        match outcome {
            RunOutcome::HostCall(ticket) => {
                let package = vm.program().package();
                let measured = package.symbols
                    [package.natives[ticket.native.index()].symbol.index()]
                    == "std::random::float";
                let region = Region::new(GLOBAL);
                assert!(library.dispatch(vm, ticket).unwrap());
                if measured {
                    let stats = region.change();
                    assert_eq!(stats.allocations, 0, "random draw: {stats:?}");
                    assert_eq!(stats.reallocations, 0, "random draw: {stats:?}");
                }
            }
            RunOutcome::BudgetYield(_) => {}
            outcome => return outcome,
        }
    }
    panic!("script did not finish")
}

#[test]
fn independent_library_preserves_streams_methods_and_faults_across_bytecode_and_gc() {
    let contracts = aside_std::contracts();
    let mut resolver = aside_compiler::semantic::MemoryResolver::default();
    for module in aside_std::modules() {
        resolver.insert(module);
    }
    let module = aside_compiler::analyze_module(
        SOURCE,
        &aside_compiler::AnalysisOptions::default(),
        &mut resolver,
    )
    .unwrap()
    .into_module()
    .unwrap();
    for encoded in [false, true] {
        let compiled = aside_host::lower(&module, &contracts, &[], Default::default()).unwrap();
        let initializer = compiled.entry;
        let program = if encoded {
            let bytes = bc::encode(compiled.program.package(), bc::Limits::default()).unwrap();
            bc::VerifiedProgram::decode(&bytes, bc::Limits::default()).unwrap()
        } else {
            compiled.program
        };
        let function = |name: &str| {
            bc::FunctionId(
                program
                    .package()
                    .functions
                    .iter()
                    .position(|f| program.package().symbols[f.name.index()] == name)
                    .unwrap() as u32,
            )
        };
        let main = function("main");
        let forbidden = function("forbidden");
        let invalid = function("invalid");
        let resume = function("resume");
        let support = aside_host::runtime_support(&program, &contracts, &[]).unwrap();
        let mut library = aside_std::StandardLibrary::new(&program);
        let mut vm = Vm::load(program, support, VmLimits::default(), SessionEpoch(1)).unwrap();
        let task = vm
            .start(initializer, &[], RootOwner(1), bc::Context::Ordinary)
            .unwrap();
        let RunOutcome::Returned(delivery) = drive(&mut vm, &mut library, task) else {
            panic!("initializer");
        };
        vm.discard_delivery(delivery).unwrap();
        let task = vm
            .start(main, &[], RootOwner(1), bc::Context::Ordinary)
            .unwrap();
        let outcome = drive(&mut vm, &mut library, task);
        let RunOutcome::Returned(delivery) = outcome else {
            let fault = match outcome {
                RunOutcome::Fault(id) => vm.delivery_fault(id).unwrap(),
                _ => None,
            };
            panic!("main: {outcome:?} {fault:?}");
        };
        let result = vm.delivery_value(delivery).unwrap();
        let values = vm.heap().sequence_get(result, 0).unwrap();
        let rng = vm.heap().sequence_get(result, 1).unwrap();
        let mut oracle = fastrand::Rng::with_seed((-42i64) as u64);
        for index in 0..32 {
            assert_eq!(
                vm.heap().sequence_get(values, index * 2).unwrap().as_int(),
                Some(oracle.i64(-9223372036854775807..9223372036854775807))
            );
            assert_eq!(
                vm.heap()
                    .sequence_get(values, index * 2 + 1)
                    .unwrap()
                    .as_float(),
                Some(oracle.f64())
            );
        }
        oracle.u32(0..1);
        oracle.u32(0..1);
        let mut detached = fastrand::Rng::with_seed(19);
        for index in 64..66 {
            assert_eq!(
                vm.heap().sequence_get(values, index).unwrap().as_int(),
                Some(detached.i64(0..1000))
            );
        }
        for (function, context, kind, expression) in [
            (
                forbidden,
                bc::Context::Derived,
                FaultKind::Effect,
                "rng.int(0, 100)",
            ),
            (
                invalid,
                bc::Context::Ordinary,
                FaultKind::Type,
                "rng.int(3, 3)",
            ),
        ] {
            let task = vm.start(function, &[rng], RootOwner(1), context).unwrap();
            let outcome = drive(&mut vm, &mut library, task);
            let RunOutcome::Fault(failure) = outcome else {
                panic!("expected {kind:?}: {outcome:?}");
            };
            let fault = vm.delivery_fault(failure).unwrap().unwrap();
            assert_eq!(fault.code, aside_vm::FaultCode::core(kind));
            let span = fault.location.source.unwrap();
            assert!(SOURCE[span.start as usize..span.end as usize].contains(expression));
            vm.discard_delivery(failure).unwrap();
        }
        let task = vm
            .start(resume, &[rng], RootOwner(1), bc::Context::Ordinary)
            .unwrap();
        let RunOutcome::Returned(last) = drive(&mut vm, &mut library, task) else {
            panic!("resume");
        };
        assert_eq!(
            vm.delivery_value(last).unwrap().as_int(),
            Some(oracle.i64(0..100))
        );
        vm.discard_delivery(last).unwrap();
        vm.discard_delivery(delivery).unwrap();
    }
}
