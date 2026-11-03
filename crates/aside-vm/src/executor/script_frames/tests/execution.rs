//! Reuse the original publication oracle with only active Script dispatch varied.
use super::*;

#[test]
fn active_script_matches_the_legacy_driver_at_budget_fault_native_and_gc_exits() {
    for case in 0..4 {
        for encoded in [false, true] {
            for work in [0, 1, 12, 31, 32, 128] {
                for (clock, instructions) in [(0, 1), (0, 5), (0, 128), (1, 1), (1, 128), (2, 128)]
                {
                    let trace = |legacy| {
                        let mut vm = load(case, encoded, false);
                        vm.legacy_body_clock = true;
                        vm.legacy_script_execution = legacy;
                        trace_vm(vm, case, work, clock, instructions)
                    };
                    assert_eq!(
                        trace(false),
                        trace(true),
                        "case={case} encoded={encoded} work={work} clock={clock} instructions={instructions}"
                    );
                }
            }
        }
    }
}

#[test]
fn active_script_retains_preparation_and_normal_exit_between_real_call_boundaries() {
    let run = |legacy| {
        let mut vm = load(1, true, false);
        vm.legacy_body_clock = true;
        vm.legacy_script_execution = legacy;
        let task = vm
            .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
            .unwrap();
        let mut clocks = 0;
        let outcome = vm
            .run_slice(
                task,
                SliceBudget {
                    instructions: u32::MAX,
                    work: u64::MAX,
                    allocation_bytes: u64::MAX,
                    deadline_ns: Some(u64::MAX),
                },
                || {
                    clocks += 1;
                    0
                },
            )
            .unwrap();
        let RunOutcome::Returned(delivery) = outcome else {
            panic!("unbounded call fixture")
        };
        let value = vm.delivery_value(delivery).unwrap();
        assert_eq!(
            vm.heap
                .sequence_get(vm.heap.sequence_get(value, 0).unwrap(), 0)
                .unwrap()
                .as_int(),
            Some(43)
        );
        (
            observe(&vm, clocks),
            vm.work_probe.driver_phase_entries.get(),
            vm.work_probe.script_execution_steps.get(),
        )
    };
    let (actual, current, steps) = run(false);
    let (old, baseline, _) = run(true);
    assert_eq!(actual, old);
    assert!(steps[1] > 0 && steps[3] > 0);
    assert!(current[1] < baseline[1]);
    assert!(current[3] < baseline[3]);
    println!(
        "active-script categories=body,preparation,named,exit,equality,other,native driver={baseline:?}->{current:?} retained_steps={steps:?}"
    );
}
