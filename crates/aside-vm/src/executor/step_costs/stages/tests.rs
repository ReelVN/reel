use super::*;
use crate::HeapBudgetUsage;

fn hotspots() -> Hotspots {
    Hotspots::new(&HeapBudgetUsage::new(u64::MAX)).unwrap()
}

#[test]
fn exact_denominators_and_queries_distinguish_unmeasured_and_completed_samples() {
    let stages = Stages::default();
    let hotspots = hotspots();
    stages.begin_slice();
    for _ in 0..255 {
        let start = stages.start(0);
        stages.finish(0, start, None, &hotspots);
    }
    stages.end_slice();
    assert!(stages.start(0).is_none());
    let row = stages.snapshot()[0];
    assert_eq!(row.calls, 256);
    assert_eq!(row.eligible_slices, 1);
    assert_eq!(row.eligible_calls, 255);
    assert_eq!((row.selected_calls, row.samples), (3, 3));
    assert_eq!(row.calls - row.selected_calls, 253);
    assert_eq!(row.clock_queries, 8); // Three pairs and one publication pair.
    assert_eq!(row.overhead_samples, 1);
    assert_eq!(row.location_missing_samples, 3);
    assert_eq!(row.location_dropped_samples, 0);
}

#[test]
fn slices_rotate_one_eligible_stage_and_other_nested_stages_only_count_calls() {
    let stages = Stages::default();
    let hotspots = hotspots();
    for _ in 0..12 {
        stages.begin_slice();
        for phase in 0..PHASES {
            let start = stages.start(phase);
            stages.finish(phase, start, None, &hotspots);
        }
        stages.end_slice();
    }
    for row in stages.snapshot() {
        assert_eq!(
            (row.calls, row.eligible_slices, row.eligible_calls),
            (12, 2, 2)
        );
        assert_eq!(
            (row.selected_calls, row.samples, row.clock_queries),
            (1, 1, 4)
        );
    }
}

#[test]
fn unfinished_samples_and_disabling_selection_are_not_reported_as_zero_duration() {
    let stages = Stages::default();
    stages.begin_slice();
    assert!(stages.start(0).is_some());
    stages.end_slice();
    let row = stages.snapshot()[0];
    assert_eq!(
        (row.selected_calls, row.samples, row.clock_queries),
        (1, 0, 1)
    );
    assert_eq!(row.selected_calls - row.samples, 1);
    stages.force(Some(None));
    stages.begin_slice();
    assert!(stages.start(1).is_none());
    stages.end_slice();
    let row = stages.snapshot()[1];
    assert_eq!(
        (row.calls, row.eligible_slices, row.clock_queries),
        (1, 0, 0)
    );
}

#[test]
fn bounded_location_losses_keep_the_completed_stage_duration_and_exact_loss_count() {
    let stages = Stages::default();
    let hotspots = hotspots();
    let rejected = (0..100_000)
        .find_map(|pc| {
            let point = StepPoint {
                pc,
                ..Default::default()
            };
            (!hotspots.record(point, 0, 1)).then_some(point)
        })
        .unwrap();
    stages.begin_slice();
    let start = stages.start(0);
    stages.finish(0, start, Some(rejected), &hotspots);
    stages.end_slice();
    let row = stages.snapshot()[0];
    assert_eq!(
        (
            row.samples,
            row.location_dropped_samples,
            row.location_missing_samples
        ),
        (1, 1, 0)
    );
}

#[test]
fn field_order_and_checked_deltas_cover_every_new_metric() {
    let before = StepCost::default();
    let after = StepCost {
        calls: 1,
        samples: 2,
        sample_wall_ns: 3,
        eligible_slices: 4,
        eligible_calls: 5,
        selected_calls: 6,
        clock_queries: 7,
        location_dropped_samples: 8,
        location_missing_samples: 9,
        overhead_samples: 10,
        overhead_wall_ns: 11,
    };
    assert_eq!(after.metrics(), std::array::from_fn(|i| i as u64 + 1));
    assert_eq!(StepCost::METRIC_NAMES[6], "clock_queries");
    assert_eq!(after.checked_delta(&before), Some(after));
    assert!(before.checked_delta(&after).is_none());
}

#[test]
#[ignore = "release observer ablation; counters-only is not a profiling-free binary"]
fn calibrate_rotating_stage_observer() {
    use crate::executor::*;
    assert!(
        !std::hint::black_box(cfg!(debug_assertions)),
        "use --release"
    );
    let n = 50_000;
    let mut expected_work = None;
    // One executable avoids code-layout changes between these timer ablations.
    // Counters-only retains every stage calls increment and profiling code paths.
    for (name, stages, driver) in [
        ("combined", true, true),
        ("counters-only", false, false),
        ("counters-only", false, false),
        ("combined", true, true),
        ("stages-only", true, false),
        ("driver-only", false, true),
        ("driver-only", false, true),
        ("stages-only", true, false),
    ] {
        let mut vm = Vm::load(
            verify(fixtures::helper_native(), Limits::default()).unwrap(),
            crate::executor::review_tests::echo_registry(),
            VmLimits::default(),
            SessionEpoch(64),
        )
        .unwrap();
        vm.step_costs
            .set_step_phase(if stages { None } else { Some(None) });
        vm.step_costs
            .set_driver_sampling(if driver { None } else { Some(false) });
        let drive = |vm: &mut Vm| {
            let task = vm
                .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
                .unwrap();
            let RunOutcome::HostCall(ticket) =
                vm.run_slice(task, SliceBudget::default(), || 0).unwrap()
            else {
                panic!("echo entry")
            };
            vm.respond(ticket, HostResponse::Ready(Value::int(42)))
                .unwrap();
            let RunOutcome::Returned(delivery) =
                vm.run_slice(task, SliceBudget::default(), || 0).unwrap()
            else {
                panic!("echo return")
            };
            assert_eq!(vm.delivery_value(delivery).unwrap().as_int(), Some(42));
            vm.discard_delivery(delivery).unwrap();
        };
        for _ in 0..64 {
            drive(&mut vm);
        }
        let before = vm.driver_costs();
        let steps = vm.step_costs();
        let instructions = vm.stats().cumulative_instructions;
        let work = vm.stats().cumulative_work;
        let start = Instant::now();
        for _ in 0..n {
            drive(&mut vm);
        }
        let wall_ns = start.elapsed().as_nanos();
        let row = vm.driver_costs().checked_delta(&before).unwrap();
        let deltas = std::array::from_fn::<_, PHASES, _>(|phase| {
            vm.step_costs()[phase].checked_delta(&steps[phase]).unwrap()
        });
        assert_eq!(vm.stats().cumulative_instructions - instructions, n * 5);
        let work_used = vm.stats().cumulative_work - work;
        if let Some(expected) = expected_work {
            assert_eq!(work_used, expected);
        } else {
            expected_work = Some(work_used);
        }
        assert_eq!(row.calls, n * 2);
        for step in deltas {
            assert_eq!(
                step.clock_queries,
                step.selected_calls + step.samples + 2 * step.overhead_samples
            );
            assert_eq!(step.selected_calls, step.samples);
        }
        println!(
            "stage-calibration mode={name} n={n} wall_ns={wall_ns} instructions={} work={} driver_queries={} stage_queries={} eligible_slices={} selected={} location_dropped={} location_missing={} overhead_samples={} overhead_wall_ns={}",
            vm.stats().cumulative_instructions - instructions,
            vm.stats().cumulative_work - work,
            row.observer_clock_queries,
            deltas.iter().map(|s| s.clock_queries).sum::<u64>(),
            deltas.iter().map(|s| s.eligible_slices).sum::<u64>(),
            deltas.iter().map(|s| s.selected_calls).sum::<u64>(),
            deltas
                .iter()
                .map(|s| s.location_dropped_samples)
                .sum::<u64>(),
            deltas
                .iter()
                .map(|s| s.location_missing_samples)
                .sum::<u64>(),
            deltas.iter().map(|s| s.overhead_samples).sum::<u64>(),
            deltas.iter().map(|s| s.overhead_wall_ns).sum::<u64>()
        );
    }
}
