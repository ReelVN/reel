mod support;
use aside_host::{aside_compiler, aside_vm};
use aside_vm::{RootOwner, SessionEpoch, bytecode as bc, executor::*};

#[test]
fn decimal_formatting_rounds_actual_values_and_preserves_integer_precision_source_asb() {
    let source = r#"
        use std::number::format;
        use std::math::{sqrt, pow};
        fn dynamic(value) { value }
        fn main() {
            for pair in [
                [format(2.675), "2.67"], [format(-2.675), "-2.67"],
                [format(12.25, 1), "12.2"], [format(12.75, 1), "12.8"],
                [format(124.5, 0), "124"], [format(-125.5, 0), "-126"],
                [format(0.0), "0.00"], [format(-0.0), "-0.00"],
                [format(-0.001), "-0.00"], [format(1, 3), "1.000"],
                [format(9223372036854775807, 0), "9223372036854775807"],
                [format(-9223372036854775808, 2), "-9223372036854775808.00"],
                [format(9223372036854775807, 19, "scientific"), "9.223372036854775807e18"],
                [format(-9223372036854775808, 19, "scientific"), "-9.223372036854775808e18"],
                [format(125, 2, "scientific"), "1.2e2"],
                [format(135, 2, "scientific"), "1.4e2"],
                [format(995, 2, "scientific"), "1.0e3"],
                [format(9.9999, precision: 4, mode: "scientific"), "1.000e1"],
                [format(0.00099999, 4, "scientific"), "1.000e-3"],
                [format(-0.0, 4, "scientific"), "-0.000e0"],
                [format(0, 1, "scientific"), "0e0"],
                [format(pow(2, -1074), 4, "scientific"), "4.941e-324"],
                [format(sqrt(-1)), "NaN"], [format(pow(10, 400)), "inf"],
                [format(-pow(10, 400), 4, "scientific"), "-inf"],
            ] {
                if pair[0] != pair[1] { throw f"format: {pair[0]} != {pair[1]}"; }
            }
            if format(1.0, 1024).byte_len() != 1026 { throw "maximum precision"; }
            let mut errors = 0;
            for precision in [-1, 1025] {
                try { format(1, precision); } catch error { errors += 1; }
            }
            try { format(1, 0, "scientific"); } catch error { errors += 1; }
            try { format(1, mode: "unknown"); } catch error { errors += 1; }
            try { format(dynamic("wrong")); } catch error { errors += 1; }
            try { format(1, precision: dynamic(1.5)); } catch error { errors += 1; }
            try { format(1, mode: dynamic(1)); } catch error { errors += 1; }
            if errors != 7 { throw "invalid formatting arguments accepted"; }
        }
    "#;
    let contracts = aside_std::contracts();
    let module =
        aside_compiler::analyze_module(source, &Default::default(), &mut support::resolver())
            .unwrap()
            .into_module()
            .unwrap();
    for encoded in [false, true] {
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
        let main = bc::FunctionId(
            program
                .package()
                .functions
                .iter()
                .position(|function| program.package().symbols[function.name.index()] == "main")
                .unwrap() as u32,
        );
        let support =
            aside_host::runtime_support(&program, &contracts, aside_std::types(), &[]).unwrap();
        let mut library = aside_std::StandardLibrary::new(&program);
        let mut limits = VmLimits::default();
        limits.heap.max_mutation_bytes = 7;
        let mut vm = Vm::load(program, support, limits, SessionEpoch(1)).unwrap();
        for function in [initializer, main] {
            let task = vm
                .start(function, &[], RootOwner(1), bc::Context::Ordinary)
                .unwrap();
            loop {
                match vm.run_slice(task, SliceBudget::default(), || 0).unwrap() {
                    RunOutcome::HostCall(ticket) => {
                        library.dispatch(&mut vm, ticket, 1).unwrap().unwrap();
                    }
                    RunOutcome::BudgetYield(_) => {}
                    RunOutcome::Returned(delivery) => {
                        vm.discard_delivery(delivery).unwrap();
                        break;
                    }
                    other => panic!("formatting source/ASB={encoded}: {other:?}"),
                }
            }
        }
    }
}
