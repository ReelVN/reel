use aside_host::{aside_compiler, aside_vm};
use aside_vm::{RootOwner, SessionEpoch, bytecode as bc, executor::*};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::{alloc::System, time::Instant};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

fn drive(vm: &mut Vm, library: &mut aside_std::StandardLibrary, task: TaskId) -> (DeliveryId, u64) {
    let mut native_work = 0;
    loop {
        match vm.run_slice(task, SliceBudget::default(), || 0).unwrap() {
            RunOutcome::HostCall(ticket) => {
                native_work += u64::from(library.dispatch(vm, ticket, 64).unwrap().unwrap());
            }
            RunOutcome::BudgetYield(_) => {}
            RunOutcome::Returned(id) => return (id, native_work),
            other => panic!("library execution: {other:?}"),
        }
    }
}

#[test]
fn independent_curves_construct_and_sample_without_native_allocations() {
    for count in [4, 128, 4096] {
        let source = format!(
            r#"
        use std::math::curve;
        fn main() {{
            let keys=[];
            for i in 0..{count} {{keys.append({{time:i,value:i*2}});}}
            curve(keys)
        }}
        fn samples(value) {{
            let mut sum=0.0;
            for i in 0..1000 {{sum+=value.sample(i%{count});}}
            sum
        }}"#
        );
        let contracts = aside_std::contracts();
        let mut resolver = aside_compiler::semantic::MemoryResolver::default();
        for module in aside_std::modules() {
            resolver.insert(module);
        }
        let module = aside_compiler::analyze_module(&source, &Default::default(), &mut resolver)
            .unwrap()
            .into_module()
            .unwrap();
        let compiled = aside_host::lower(&module, &contracts, &[], Default::default()).unwrap();
        let initializer = compiled.entry;
        let bytes = bc::encode(compiled.program.package(), bc::Limits::default()).unwrap();
        let program = bc::VerifiedProgram::decode(&bytes, bc::Limits::default()).unwrap();
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
        let samples = function("samples");
        let support = aside_host::runtime_support(&program, &contracts, &[]).unwrap();
        let mut library = aside_std::StandardLibrary::new(&program);
        let mut vm = Vm::load(program, support, VmLimits::default(), SessionEpoch(1)).unwrap();
        let init = vm
            .start(initializer, &[], RootOwner(1), bc::Context::Ordinary)
            .unwrap();
        let (delivery, _) = drive(&mut vm, &mut library, init);
        vm.discard_delivery(delivery).unwrap();
        let main = vm
            .start(main, &[], RootOwner(1), bc::Context::Ordinary)
            .unwrap();
        let mut construction = stats_alloc::Stats::default();
        let mut construction_work = 0;
        let mut construction_ns = 0;
        let mut construction_chunks = 0;
        let delivery = loop {
            match vm.run_slice(main, SliceBudget::default(), || 0).unwrap() {
                RunOutcome::HostCall(ticket) => {
                    let start = Instant::now();
                    let chunks = vm.heap().stats().chunk_allocations;
                    let region = Region::new(GLOBAL);
                    construction_work += library.dispatch(&mut vm, ticket, 64).unwrap().unwrap();
                    let stats = region.change();
                    construction.allocations += stats.allocations;
                    construction.reallocations += stats.reallocations;
                    construction_chunks += vm.heap().stats().chunk_allocations - chunks;
                    construction_ns += start.elapsed().as_nanos();
                }
                RunOutcome::BudgetYield(_) => {}
                RunOutcome::Returned(id) => break id,
                other => panic!("construct {other:?}"),
            }
        };
        let value = vm.delivery_value(delivery).unwrap();
        // Warm the reusable native call window before measuring the complete loop.
        let task = vm
            .start(samples, &[value], RootOwner(1), bc::Context::Ordinary)
            .unwrap();
        let (warm, _) = drive(&mut vm, &mut library, task);
        vm.discard_delivery(warm).unwrap();
        let task = vm
            .start(samples, &[value], RootOwner(1), bc::Context::Ordinary)
            .unwrap();
        let mut sample_allocations = 0;
        let mut sample_reallocations = 0;
        let mut sample_count = 0;
        let chunks = vm.heap().stats().chunk_allocations;
        let whole = Region::new(GLOBAL);
        let start = Instant::now();
        loop {
            match vm.run_slice(task, SliceBudget::default(), || 0).unwrap() {
                RunOutcome::HostCall(ticket) => {
                    let region = Region::new(GLOBAL);
                    library.dispatch(&mut vm, ticket, 64).unwrap().unwrap();
                    let stats = region.change();
                    sample_allocations += stats.allocations;
                    sample_reallocations += stats.reallocations;
                    sample_count += 1;
                }
                RunOutcome::BudgetYield(_) => {}
                RunOutcome::Returned(id) => {
                    let expected: f64 = (0..1000).map(|i| f64::from(i % count * 2)).sum();
                    assert_eq!(vm.delivery_value(id).unwrap().as_float(), Some(expected));
                    vm.discard_delivery(id).unwrap();
                    break;
                }
                other => panic!("sample {other:?}"),
            }
        }
        let elapsed = start.elapsed();
        let whole = whole.change();
        assert_eq!((sample_allocations, sample_reallocations), (0, 0));
        assert_eq!(sample_count, 1000);
        eprintln!(
            "curve keys={count}: construct allocations={} reallocations={} VM_chunks={construction_chunks} native_work={construction_work} ns={construction_ns}; samples=1000 native_allocations={sample_allocations} native_reallocations={sample_reallocations}; complete_loop allocations={} reallocations={} VM_chunks={} elapsed={elapsed:?}",
            construction.allocations,
            construction.reallocations,
            whole.allocations,
            whole.reallocations,
            vm.heap().stats().chunk_allocations - chunks
        );
        vm.discard_delivery(delivery).unwrap();
    }
}
