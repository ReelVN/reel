use super::*;
use std::time::Duration;

#[test]
fn existing_timestamps_partition_every_gap_without_reading_a_clock() {
    let state = State::default();
    let start = Instant::now();
    let at = |ns| start + Duration::from_nanos(ns);
    state.begin(start);
    for (phase, began, ended) in [
        (0, 10, 20),
        (0, 30, 40),
        (1, 50, 60),
        (2, 80, 90),
        (0, 100, 130),
    ] {
        state.start_span(phase, at(began));
        state.end_span(at(ended));
    }
    state.control(Control::Iteration);
    state.control(Control::Iteration);
    state.control(Control::FaultBuild);
    state.end(at(150));
    let mut row = DriverCost {
        loop_ns: 80,
        ..Default::default()
    };
    state.publish(&mut row);
    assert_eq!(
        [
            row.first_dispatch_gap_ns,
            row.scalar_reentry_gap_ns,
            row.classified_dispatch_gap_ns,
            row.final_dispatch_gap_ns,
        ],
        [10, 20, 30, 20]
    );
    assert_eq!(
        [
            row.first_dispatch_gaps,
            row.scalar_reentry_gaps,
            row.classified_dispatch_gaps,
            row.final_dispatch_gaps,
        ],
        [1, 2, 2, 1]
    );
    assert_eq!(row.loop_gap_errors, 0);
    assert_eq!((row.loop_iterations, row.loop_fault_build), (2, 1));
    assert_eq!(row.observer_clock_queries, 0);
}

#[test]
fn no_dispatch_and_reset_preserve_the_empty_run_boundary() {
    let state = State::default();
    let began = Instant::now();
    state.begin(began);
    state.control(Control::Cancelling);
    state.end(began + Duration::from_nanos(25));
    let mut row = DriverCost {
        loop_ns: 25,
        ..Default::default()
    };
    state.publish(&mut row);
    assert_eq!(row.first_dispatch_gaps, 0);
    assert_eq!(
        (row.final_dispatch_gap_ns, row.final_dispatch_gaps),
        (25, 1)
    );
    assert_eq!(row.loop_cancelling, 1);
    assert_eq!(row.loop_gap_errors, 0);
    state.begin(began);
    state.end(began);
    row = DriverCost::default();
    state.publish(&mut row);
    assert_eq!(row.loop_cancelling, 0);
    assert_eq!(row.final_dispatch_gaps, 1);
    assert_eq!(row.loop_gap_errors, 0);
}

#[test]
fn invalid_nesting_time_order_and_total_are_reported() {
    let began = Instant::now();
    let state = State::default();
    state.begin(began);
    state.start_span(0, began + Duration::from_micros(10));
    state.start_span(1, began + Duration::from_micros(20));
    state.end_span(began + Duration::from_micros(30));
    state.start_span(0, began + Duration::from_micros(25));
    state.end_span(began + Duration::from_micros(40));
    state.end(began + Duration::from_micros(50));
    let mut row = DriverCost::default();
    state.publish(&mut row);
    assert_eq!(row.loop_gap_errors, 4, "{row:?}");
}
