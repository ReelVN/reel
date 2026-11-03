use super::*;
use crate::executor::step_costs::Part;
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
    assert!(row.budget_clock_ns <= row.run_ns);
    let parts = row.lifecycle;
    assert_eq!(parts.errors, 0);
    assert_eq!(parts.run_ns, row.run_ns);
    assert_eq!(
        parts.unattributed_by_outer_ns.iter().sum::<u64>(),
        parts.unattributed_ns
    );
    for (other, outer) in parts.unattributed_by_outer_ns.into_iter().zip([
        row.scalar_ns,
        row.script_ns,
        row.native_ns,
        row.loop_ns,
    ]) {
        assert!(
            other <= outer,
            "unattributed time exceeds its outer dispatch"
        );
    }
    assert_eq!(
        parts.parts.iter().map(|part| part.wall_ns).sum::<u64>()
            + parts.unattributed_ns
            + parts.observer_ns,
        row.run_ns
    );
    assert_eq!(row.calibration_pairs, row.samples);
    assert!(row.calibration_ns <= row.entry_ns);
    assert_eq!(row.loop_gap_errors, 0);
    assert_eq!(
        row.loop_ns,
        row.first_dispatch_gap_ns
            + row.scalar_reentry_gap_ns
            + row.classified_dispatch_gap_ns
            + row.final_dispatch_gap_ns
    );
    assert!(row.final_dispatch_gaps <= row.samples);
    assert!(row.first_dispatch_gaps <= row.final_dispatch_gaps);
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
    assert_eq!(
        costs.observer_clock_queries,
        16 + costs.lifecycle.observer_clock_queries
    );
    assert_eq!(costs.loop_iterations, 2);
    assert_eq!(costs.first_dispatch_gaps, 0);
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
    // This real main -> helper -> native path enters fallback dispatch and
    // opens nested named operations after its outer phase has been selected.
    for part in [Part::CallPushScript, Part::RootOpen, Part::NativePrepare] {
        assert!(costs.lifecycle.parts[part as usize].calls > 0);
    }
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
    assert!(costs.loop_cancelling > 0);
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
fn gap_edges_reuse_driver_queries_and_unobserved_control_marks_are_erased() {
    let state = State::default();
    state.set_sampling(Some(false));
    assert!(state.begin().is_none());
    state.control(Control::Iteration);
    state.control(Control::Cancelling);
    assert_eq!(state.snapshot(1, 2).loop_iterations, 0);
    state.set_sampling(Some(true));
    let mut slice = state.begin();
    state.begin_run(&mut slice);
    for phase in 0..3 {
        let span = state.start_span(phase);
        state.control(Control::Iteration);
        state.finish_span(span);
    }
    state.end_run(&mut slice);
    state.finish(slice, SliceUsage::default(), 0, 0);
    let row = state.snapshot(1, 2);
    closed(row);
    // Existing reads: three entry/calibration, run begin/end, three span pairs,
    // and the final slice end. Gaps/control marks add no clock query.
    assert_eq!(
        row.observer_clock_queries,
        12 + row.lifecycle.observer_clock_queries
    );
    assert_eq!(row.loop_iterations, 3);
    assert_eq!(row.loop_cancelling, 0);
    assert_eq!(row.first_dispatch_gaps, 1);
    assert_eq!(row.classified_dispatch_gaps, 2);
    assert_eq!(row.final_dispatch_gaps, 1);
}

#[test]
fn operation_sampling_preserves_supplied_clocks_meter_and_native_results() {
    let trace = |sampling| {
        let mut vm = vm();
        vm.step_costs.set_driver_sampling(Some(sampling));
        let task = vm
            .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
            .unwrap();
        let mut budget = SliceBudget {
            instructions: 1,
            work: 1,
            allocation_bytes: 0,
            deadline_ns: Some(u64::MAX),
        };
        let mut trace = Vec::new();
        for _ in 0..1000 {
            let mut clocks = 0;
            let outcome = vm
                .run_slice(task, budget, || {
                    clocks += 1;
                    0
                })
                .unwrap();
            let kind = match outcome {
                RunOutcome::BudgetYield(_) => 0,
                RunOutcome::HostCall(_) => 1,
                RunOutcome::Returned(_) => 2,
                _ => panic!("unexpected native fixture result"),
            };
            trace.push((vm.last_usage, clocks, kind));
            match outcome {
                RunOutcome::BudgetYield(usage) => {
                    budget.work = budget.work.max(usage.required_work);
                    budget.allocation_bytes = usage.required_allocation;
                }
                RunOutcome::HostCall(ticket) => {
                    assert_eq!(vm.host_arguments(ticket).unwrap()[0].as_int(), Some(41));
                    vm.respond(ticket, HostResponse::Ready(Value::int(42)))
                        .unwrap();
                }
                RunOutcome::Returned(delivery) => {
                    assert_eq!(vm.delivery_value(delivery).unwrap().as_int(), Some(42));
                    closed(vm.driver_costs());
                    return trace;
                }
                _ => unreachable!(),
            }
        }
        panic!("native fixture did not progress")
    };
    assert_eq!(trace(false), trace(true));
}

struct RestoreTiming(bool);
impl Drop for RestoreTiming {
    fn drop(&mut self) {
        set_profiling_enabled(self.0);
    }
}

#[test]
fn thread_timing_gate_is_latched_at_slice_entry_and_off_does_not_observe() {
    let _restore = RestoreTiming(set_profiling_enabled(false));
    let mut vm = vm();
    vm.step_costs.set_driver_sampling(Some(true));
    let task = vm
        .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
        .unwrap();
    let budget = SliceBudget {
        deadline_ns: Some(0),
        ..Default::default()
    };
    let before = vm.driver_costs();
    let mut clocks = 0;
    assert!(matches!(
        vm.run_slice(task, budget, || {
            clocks += 1;
            set_profiling_enabled(true);
            0
        })
        .unwrap(),
        RunOutcome::BudgetYield(_)
    ));
    assert_eq!(vm.driver_costs(), before);
    assert_eq!(vm.step_costs(), [StepCost::default(); STEP_NAMES.len()]);
    assert!(matches!(
        vm.run_slice(task, budget, || {
            clocks += 1;
            set_profiling_enabled(false);
            0
        })
        .unwrap(),
        RunOutcome::BudgetYield(_)
    ));
    let observed = vm.driver_costs();
    assert_eq!(
        (
            observed.calls,
            observed.samples,
            observed.budget_clock_queries
        ),
        (1, 1, 1)
    );
    closed(observed);
    assert!(matches!(
        vm.run_slice(task, budget, || {
            clocks += 1;
            0
        })
        .unwrap(),
        RunOutcome::BudgetYield(_)
    ));
    assert_eq!(vm.driver_costs(), observed);
    assert_eq!(clocks, 3);
}

#[test]
fn timing_gate_is_local_to_the_vm_execution_thread() {
    let _restore = RestoreTiming(set_profiling_enabled(false));
    assert!(
        std::thread::spawn(|| {
            let prior = set_profiling_enabled(false);
            set_profiling_enabled(prior);
            prior
        })
        .join()
        .unwrap()
    );
    assert!(!set_profiling_enabled(false));
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
