mod support;
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
fn untyped(value) {value}
fn invalid() {pow(untyped("wrong"),2)}
fn convert(value) -> int {to_int(value)}
fn limit(value, minimum, maximum) {clamp(value, minimum, maximum)}
fn extrema(a, b) { (min(a, b), max(a, b)) }
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
                library.dispatch(vm, ticket, 1).unwrap().unwrap();
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
fn math_preserves_numeric_contracts_without_allocation_and_reports_source_errors() {
    let contracts = aside_std::contracts();
    let mut resolver = support::resolver();
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
    assert!(
        !aside_compiler::analyze_module(
            "use std::math::pow; fn main(){pow(\"wrong\",2);}",
            &Default::default(),
            &mut resolver,
        )
        .unwrap()
        .is_ok(),
        "known invalid numeric arguments must fail during source analysis"
    );
    let module = aside_compiler::analyze_module(SOURCE, &Default::default(), &mut resolver)
        .unwrap()
        .into_module()
        .unwrap();
    for (encoded, leaves) in [false, true]
        .into_iter()
        .flat_map(|encoded| [false, true].map(|leaves| (encoded, leaves)))
    {
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
        let convert = function("convert");
        let limit = function("limit");
        let extrema = function("extrema");
        let mut support =
            aside_host::runtime_support(&program, &contracts, aside_std::types(), &[]).unwrap();
        if !leaves {
            for native in &mut support.natives {
                native.read_only_leaf = None;
            }
        }
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
        let precise = Value::int(9_007_199_254_740_993);
        let rounded = Value::float(9_007_199_254_740_992.);
        let zero = Value::int(0);
        let one = Value::int(1);
        let fraction = Value::float(0.5);
        let nan = Value::float(f64::NAN);
        let negative_zero = Value::float(-0.0);
        for (args, expected) in [
            ([precise, rounded], [rounded, precise]),
            ([zero, negative_zero], [zero, zero]),
            ([negative_zero, zero], [negative_zero, negative_zero]),
            ([nan, one], [one, one]),
            ([one, nan], [one, one]),
        ] {
            let task = vm
                .start(extrema, &args, RootOwner(1), bc::Context::Ordinary)
                .unwrap();
            let RunOutcome::Returned(id) = drive(&mut vm, &mut library, task, false) else {
                panic!("extrema: {args:?}")
            };
            let values = vm.delivery_value(id).unwrap();
            for (index, expected) in expected.into_iter().enumerate() {
                let value = vm.heap().sequence_get(values, index as u32).unwrap();
                assert!(value.same_bits(expected));
            }
            vm.discard_delivery(id).unwrap();
        }
        for (args, expected) in [
            ([fraction, zero, one], Some(fraction)),
            ([precise, zero, rounded], Some(rounded)),
            ([rounded, precise, Value::int(i64::MAX)], Some(precise)),
            ([nan, zero, one], Some(nan)),
            ([zero, one, zero], None),
            ([zero, nan, one], None),
        ] {
            let task = vm
                .start(limit, &args, RootOwner(1), bc::Context::Ordinary)
                .unwrap();
            let id = match (
                drive(&mut vm, &mut library, task, expected.is_some()),
                expected,
            ) {
                (RunOutcome::Returned(id), Some(expected)) => {
                    assert!(vm.delivery_value(id).unwrap().same_bits(expected));
                    id
                }
                (RunOutcome::Fault(id), None) => {
                    assert_eq!(
                        vm.delivery_fault(id).unwrap().unwrap().code,
                        aside_vm::FaultCode::core(FaultKind::Type)
                    );
                    id
                }
                (outcome, expected) => panic!("clamp {args:?}: {outcome:?}, expected {expected:?}"),
            };
            vm.discard_delivery(id).unwrap();
        }
        for (input, expected) in [
            (Value::int(i64::MIN), Some(i64::MIN)),
            (Value::int(i64::MAX), Some(i64::MAX)),
            (
                Value::int(9_007_199_254_740_993),
                Some(9_007_199_254_740_993),
            ),
            (Value::float(2.9), Some(2)),
            (Value::float(-2.9), Some(-2)),
            (Value::float(-0.0), Some(0)),
            (Value::float(i64::MIN as f64), Some(i64::MIN)),
            (
                Value::float(9_223_372_036_854_774_784.0),
                Some(9_223_372_036_854_774_784),
            ),
            (Value::float(i64::MAX as f64), None),
            (
                Value::float(f64::from_bits((i64::MIN as f64).to_bits() + 1)),
                None,
            ),
            (Value::float(f64::NAN), None),
            (Value::float(f64::INFINITY), None),
            (Value::float(f64::NEG_INFINITY), None),
            (Value::bool(true), None),
        ] {
            let task = vm
                .start(convert, &[input], RootOwner(1), bc::Context::Ordinary)
                .unwrap();
            let outcome = drive(&mut vm, &mut library, task, expected.is_some());
            let id = match (outcome, expected) {
                (RunOutcome::Returned(id), Some(expected)) => {
                    assert_eq!(vm.delivery_value(id).unwrap().as_int(), Some(expected));
                    id
                }
                (RunOutcome::Fault(id), None) => {
                    let fault = vm.delivery_fault(id).unwrap().unwrap();
                    assert_eq!(fault.code, aside_vm::FaultCode::core(FaultKind::Type));
                    let span = fault.location.source.unwrap();
                    assert_eq!(
                        &SOURCE[span.start as usize..span.end as usize],
                        "to_int(value)"
                    );
                    id
                }
                (outcome, expected) => {
                    panic!("conversion {input:?}: {outcome:?}, expected {expected:?}")
                }
            };
            vm.discard_delivery(id).unwrap();
        }
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
            "pow(untyped(\"wrong\"),2)"
        );
        vm.discard_delivery(id).unwrap();
    }
}
