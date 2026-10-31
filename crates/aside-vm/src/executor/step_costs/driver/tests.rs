use super::*;
use crate::executor::*;

fn vm() -> Vm {
    Vm::load(
        verify(fixtures::helper_native(), Limits::default()).unwrap(),
        crate::executor::review_tests::echo_registry(),
        VmLimits::default(),
        SessionEpoch(64),
    )
    .unwrap()
}
fn closed(row: DriverCost) {
    assert_eq!(row.wall_ns, row.entry_ns + row.run_ns + row.exit_ns);
    assert_eq!(
        row.run_ns,
        row.scalar_ns + row.script_ns + row.native_ns + row.loop_ns
    );
    assert!(row.budget_clock_ns <= row.loop_ns);
    assert_eq!(row.calibration_pairs, row.samples);
    assert!(row.calibration_ns <= row.entry_ns);
}

#[test]
fn selected_slices_close_independently_of_stage_eligibility_denominators() {
    let mut vm = vm();
    let task = vm
        .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
        .unwrap();
    let before = vm.driver_costs();
    let mut queries = 0;
    for _ in 0..=DRIVER_SAMPLE_STRIDE {
        assert!(matches!(
            vm.run_slice(
                task,
                SliceBudget {
                    deadline_ns: Some(0),
                    ..Default::default()
                },
                || {
                    queries += 1;
                    0
                }
            )
            .unwrap(),
            RunOutcome::BudgetYield(_)
        ));
        closed(vm.driver_costs().checked_delta(&before).unwrap());
    }
    let costs = vm.driver_costs().checked_delta(&before).unwrap();
    assert_eq!(costs.calls, 128);
    assert_eq!(costs.samples, 2);
    assert_eq!(costs.budget_clock_queries, 2);
    assert_eq!(costs.sampled_instructions, 0);
    let steps = vm.step_costs();
    assert_eq!(
        steps.iter().map(|row| row.eligible_slices).sum::<u64>(),
        128
    );
    for mut step in steps {
        step.eligible_slices = 0;
        assert_eq!(step, StepCost::default());
    }
    assert_eq!(queries, 128);
    #[cfg(feature = "execution-counts")]
    assert_eq!(vm.execution_counts().actual_budget_clock_queries, queries);
}

#[test]
fn dispatch_and_error_results_keep_a_closed_observation() {
    let mut vm = vm();
    let task = vm
        .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
        .unwrap();
    let RunOutcome::HostCall(ticket) = vm.run_slice(task, SliceBudget::default(), || 0).unwrap()
    else {
        panic!("host echo")
    };
    assert_eq!(vm.host_arguments(ticket).unwrap()[0].as_int(), Some(41));
    let costs = vm.driver_costs();
    closed(costs);
    assert_eq!(costs.samples, 1);
    assert_eq!(costs.sampled_instructions, 3);
    assert_eq!(costs.budget_clock_queries, 0);
    assert!(costs.observer_clock_queries >= 6);
    vm.respond(ticket, HostResponse::Ready(Value::int(42)))
        .unwrap();
    let RunOutcome::Returned(delivery) = vm.run_slice(task, SliceBudget::default(), || 0).unwrap()
    else {
        panic!("return")
    };
    assert_eq!(vm.delivery_value(delivery).unwrap().as_int(), Some(42));
    closed(vm.driver_costs());

    let mut cancelled = self::vm();
    let task = cancelled
        .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
        .unwrap();
    cancelled.cancel(RootOwner(1)).unwrap();
    let mut queries = 0;
    assert!(matches!(
        cancelled.run_slice(
            task,
            SliceBudget {
                deadline_ns: Some(u64::MAX),
                ..Default::default()
            },
            || {
                queries += 1;
                0
            }
        ),
        Err(VmError::Idle)
    ));
    let costs = cancelled.driver_costs();
    closed(costs);
    assert_eq!(costs.samples, 1);
    assert_eq!(costs.budget_clock_queries, queries);
    assert!(queries > 0);
    #[cfg(feature = "execution-counts")]
    assert_eq!(
        cancelled.execution_counts().actual_budget_clock_queries,
        queries
    );
}

#[test]
fn invalid_task_is_entry_only_and_differences_reject_other_vms() {
    let mut vm = vm();
    let mut other = self::vm();
    let task = other
        .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
        .unwrap();
    assert!(vm.run_slice(task, SliceBudget::default(), || 0).is_err());
    let costs = vm.driver_costs();
    closed(costs);
    assert_eq!(
        (costs.calls, costs.entry_failures, costs.samples),
        (1, 1, 1)
    );
    assert_eq!(costs.wall_ns, costs.entry_ns);
    assert_eq!(costs.run_ns + costs.exit_ns, 0);
    assert!(costs.checked_delta(&other.driver_costs()).is_none());
    let mut lower = costs;
    lower.calls = 0;
    assert!(lower.checked_delta(&costs).is_none());
}

#[test]
fn supplied_clock_latency_belongs_to_loop_in_the_same_observation() {
    let mut vm = vm();
    let task = vm
        .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
        .unwrap();
    assert!(matches!(
        vm.run_slice(
            task,
            SliceBudget {
                deadline_ns: Some(0),
                ..Default::default()
            },
            || {
                std::thread::sleep(std::time::Duration::from_millis(2));
                0
            }
        )
        .unwrap(),
        RunOutcome::BudgetYield(_)
    ));
    let row = vm.driver_costs();
    closed(row);
    assert_eq!(row.budget_clock_queries, 1);
    assert!(row.budget_clock_ns >= 1_000_000);
    assert_eq!(row.run_ns, row.loop_ns);
    assert_eq!(row.scalar_ns + row.script_ns + row.native_ns, 0);
}

#[test]
#[ignore = "host observer calibration; raw overhead, not target performance"]
fn calibrate_whole_slice_observer() {
    let n = 10_000;
    // The test-only switch leaves rotating stage eligibility active in every
    // case. An infinite deadline keeps work/outcomes equal despite extra clocks.
    for (name, sampling) in [
        ("off", Some(false)),
        ("stride127", None),
        ("all", Some(true)),
        ("all", Some(true)),
        ("stride127", None),
        ("off", Some(false)),
    ] {
        let mut vm = vm();
        vm.step_costs.set_driver_sampling(sampling);
        let epoch = Instant::now();
        let drive = |vm: &mut Vm| {
            let task = vm
                .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
                .unwrap();
            let budget = SliceBudget {
                deadline_ns: Some(u64::MAX),
                ..Default::default()
            };
            let RunOutcome::HostCall(ticket) = vm
                .run_slice(task, budget, || epoch.elapsed().as_nanos() as u64)
                .unwrap()
            else {
                panic!("echo entry")
            };
            vm.respond(ticket, HostResponse::Ready(Value::int(42)))
                .unwrap();
            let RunOutcome::Returned(delivery) = vm
                .run_slice(task, budget, || epoch.elapsed().as_nanos() as u64)
                .unwrap()
            else {
                panic!("echo return")
            };
            assert_eq!(vm.delivery_value(delivery).unwrap().as_int(), Some(42));
            vm.discard_delivery(delivery).unwrap();
        };
        for _ in 0..32 {
            drive(&mut vm);
        }
        let before = vm.driver_costs();
        let instructions = vm.stats().cumulative_instructions;
        let work = vm.stats().cumulative_work;
        let start = Instant::now();
        for _ in 0..n {
            drive(&mut vm);
        }
        let wall = start.elapsed().as_nanos();
        let row = vm.driver_costs().checked_delta(&before).unwrap();
        closed(row);
        assert_eq!(vm.stats().cumulative_instructions - instructions, n * 5);
        assert_eq!(row.calls, n * 2);
        println!(
            "driver-calibration mode={name} n={n} wall_ns={wall} instructions={} work={} costs={row:?}",
            vm.stats().cumulative_instructions - instructions,
            vm.stats().cumulative_work - work
        );
    }
}
