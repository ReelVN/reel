use aside_host::{FromValue, aside_compiler, aside_vm};
use aside_vm::{RootOwner, SessionEpoch, Value, bytecode as bc, executor::*};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::alloc::System;

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const SOURCE: &str = r#"
use std::math::*;
fn main() {
    [sin(0),cos(0),tan(0),asin(0),acos(1),atan(0),atan2(0,1.0),
    sqrt(4),hypot(3.0,4),pow(2,3),exp(0),ln(1),log2(8),log10(100),
    abs(-7),floor(3),ceil(3),round(3),radians(180),degrees(0)]
}
fn invalid() {pow("wrong",2)}
"#;

fn drive(
    vm: &mut Vm,
    library: &mut aside_std::StandardLibrary,
    task: TaskId,
    measure: bool,
) -> RunOutcome {
    loop {
        match vm.run_slice(task, SliceBudget::default(), || 0).unwrap() {
            RunOutcome::HostCall(ticket) => {
                let region = Region::new(GLOBAL);
                assert!(library.dispatch(vm, ticket).unwrap());
                if measure {
                    let stats = region.change();
                    assert_eq!((stats.allocations, stats.reallocations), (0, 0));
                }
            }
            RunOutcome::BudgetYield(_) => {}
            result => return result,
        }
    }
}

#[test]
fn math_accepts_int_and_float_at_native_boundary_with_float_results_and_source_errors() {
    let contracts = aside_std::contracts();
    let mut resolver = aside_compiler::semantic::MemoryResolver::default();
    for module in aside_std::modules() {
        resolver.insert(module);
    }
    assert!(
        aside_compiler::analyze_module(
            "fn strict(value:float){value} fn main(){strict(1);}",
            &Default::default(),
            &mut resolver
        )
        .unwrap()
        .into_module()
        .is_err()
    );
    let module = aside_compiler::analyze_module(SOURCE, &Default::default(), &mut resolver)
        .unwrap()
        .into_module()
        .unwrap();
    for encoded in [false, true] {
        let compiled = aside_host::lower(&module, &contracts, &[], Default::default()).unwrap();
        let initializer = compiled.entry;
        let program = if encoded {
            bc::VerifiedProgram::decode(
                &bc::encode(compiled.program.package(), bc::Limits::default()).unwrap(),
                bc::Limits::default(),
            )
            .unwrap()
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
        let invalid = function("invalid");
        let support = aside_host::runtime_support(&program, &contracts, &[]).unwrap();
        let mut library = aside_std::StandardLibrary::new(&program);
        let mut vm = Vm::load(program, support, VmLimits::default(), SessionEpoch(1)).unwrap();
        assert_eq!(<f64 as FromValue>::TYPE, bc::BuiltinType::Float);
        assert!(<f64 as FromValue>::from_value(vm.heap(), Value::int(1)).is_err());
        let task = vm
            .start(initializer, &[], RootOwner(1), bc::Context::Ordinary)
            .unwrap();
        let RunOutcome::Returned(id) = drive(&mut vm, &mut library, task, false) else {
            panic!("init")
        };
        vm.discard_delivery(id).unwrap();
        let task = vm
            .start(main, &[], RootOwner(1), bc::Context::Ordinary)
            .unwrap();
        let RunOutcome::Returned(id) = drive(&mut vm, &mut library, task, true) else {
            panic!("main")
        };
        let values = vm.delivery_value(id).unwrap();
        for (index, expected) in [
            0.,
            1.,
            0.,
            0.,
            0.,
            0.,
            0.,
            2.,
            5.,
            8.,
            1.,
            0.,
            3.,
            2.,
            7.,
            3.,
            3.,
            3.,
            std::f64::consts::PI,
            0.,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                vm.heap()
                    .sequence_get(values, index as u32)
                    .unwrap()
                    .as_float(),
                Some(expected)
            );
        }
        vm.discard_delivery(id).unwrap();
        let task = vm
            .start(invalid, &[], RootOwner(1), bc::Context::Ordinary)
            .unwrap();
        let RunOutcome::Fault(id) = drive(&mut vm, &mut library, task, false) else {
            panic!("invalid")
        };
        let fault = vm.delivery_fault(id).unwrap().unwrap();
        assert_eq!(fault.code, aside_vm::FaultCode::core(FaultKind::Type));
        let span = fault.location.source.unwrap();
        assert_eq!(
            &SOURCE[span.start as usize..span.end as usize],
            "pow(\"wrong\",2)"
        );
        vm.discard_delivery(id).unwrap();
    }
}
