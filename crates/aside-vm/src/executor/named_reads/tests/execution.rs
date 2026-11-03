//! Keep the named-key oracle's inputs while varying only active Script dispatch.
use super::*;

#[test]
fn active_named_cursor_keeps_deadline_queries_roots_and_result_between_driver_handoffs() {
    for host in [false, true] {
        for long in [false, true] {
            let run = |legacy| {
                let (mut vm, target, _) = load(host, true, long, true, false);
                vm.legacy_body_clock = true;
                vm.legacy_script_execution = legacy;
                let task = vm
                    .start(FunctionId(0), &[target], RootOwner(1), Context::Ordinary)
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
                    panic!("named lookup did not finish")
                };
                assert_eq!(vm.delivery_value(delivery).unwrap().as_int(), Some(42));
                (
                    vm.last_usage,
                    clocks,
                    vm.work_probe.driver_phase_entries.get(),
                    vm.work_probe.script_execution_steps.get(),
                )
            };
            let (usage, clocks, current, steps) = run(false);
            let (old_usage, old_clocks, baseline, _) = run(true);
            assert_eq!((usage, clocks), (old_usage, old_clocks));
            assert!(steps[2] > 0);
            assert!(current[2] < baseline[2]);
            println!(
                "active-named host={host} long={long} driver={baseline:?}->{current:?} retained_steps={steps:?}"
            );
        }
    }
}
