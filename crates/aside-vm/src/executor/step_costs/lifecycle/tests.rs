use super::*;
use std::time::Duration;

#[test]
fn nested_operations_partition_run_and_keep_observer_markers_separate() {
    let s = State::default();
    let base = Instant::now();
    let at = |n| base + Duration::from_nanos(n);
    s.begin_at(base);
    let parent = s.enter_at(Part::CallPushScript, at(5));
    s.marker(at(5), at(7), 2);
    let child = s.enter_at(Part::RootOpen, at(10));
    s.marker(at(10), at(11), 2);
    s.leave_at(child, None, at(20));
    s.marker(at(20), at(23), 2);
    s.leave_at(parent, None, at(30));
    s.marker(at(30), at(31), 2);
    s.end(at(40));
    let row = s.snapshot();
    assert_eq!(
        row.parts[Part::RootOpen as usize],
        LifecyclePart {
            calls: 1,
            wall_ns: 9
        }
    );
    assert_eq!(
        row.parts[Part::CallPushScript as usize],
        LifecyclePart {
            calls: 1,
            wall_ns: 10
        }
    );
    assert_eq!(
        (
            row.run_ns,
            row.unattributed_ns,
            row.observer_ns,
            row.observer_clock_queries,
            row.errors
        ),
        (40, 14, 7, 8, 0)
    );
    assert_eq!(row.unattributed_by_outer_ns, [0, 0, 0, 14]);
}

#[test]
fn outer_boundaries_partition_only_unattributed_time_across_nested_parts() {
    let s = State::default();
    let base = Instant::now();
    let at = |n| base + Duration::from_nanos(n);
    s.begin_at(base);
    s.outer_at(0, at(10));
    let parent = s.enter_at(Part::CallPushScript, at(15));
    s.marker(at(15), at(17), 2);
    // Named operations retain ownership when an outer dispatch changes.
    s.outer_at(1, at(20));
    let child = s.enter_at(Part::RootOpen, at(25));
    s.marker(at(25), at(27), 2);
    s.leave_at(child, None, at(35));
    s.marker(at(35), at(36), 2);
    s.leave_at(parent, None, at(40));
    s.marker(at(40), at(42), 2);
    s.outer_end(at(50));
    s.outer_at(2, at(60));
    s.outer_end(at(70));
    s.outer_at(0, at(80));
    s.outer_end(at(90));
    s.end(at(100));
    let row = s.snapshot();
    assert_eq!(row.unattributed_by_outer_ns, [15, 8, 10, 40]);
    assert_eq!(row.unattributed_ns, 73);
    assert_eq!(row.parts[Part::CallPushScript as usize].wall_ns, 12);
    assert_eq!(row.parts[Part::RootOpen as usize].wall_ns, 8);
    assert_eq!(
        (
            row.run_ns,
            row.observer_ns,
            row.observer_clock_queries,
            row.errors
        ),
        (100, 7, 8, 0)
    );
    let mut totals = LifecycleCost::default();
    totals.add(&row);
    let before = totals;
    totals.add(&row);
    assert_eq!(totals.checked_delta(&before), Some(row));
    let mut regressed = totals;
    regressed.unattributed_by_outer_ns[1] = before.unattributed_by_outer_ns[1] - 1;
    assert!(regressed.checked_delta(&before).is_none());
    s.begin_at(at(100));
    s.end(at(103));
    assert_eq!(s.snapshot().unattributed_by_outer_ns, [0, 0, 0, 3]);
}

#[test]
fn classification_at_completion_retains_exclusive_time() {
    let s = State::default();
    let base = Instant::now();
    s.begin_at(base);
    let span = s.enter_at(Part::ClassSpecError, base);
    s.marker(base, base, 2);
    let end = base + Duration::from_nanos(7);
    s.leave_at(span, Some(Part::ClassSpecMethod), end);
    s.marker(end, end, 2);
    s.end(end);
    let row = s.snapshot();
    assert_eq!(
        row.parts[Part::ClassSpecError as usize],
        LifecyclePart::default()
    );
    assert_eq!(
        row.parts[Part::ClassSpecMethod as usize],
        LifecyclePart {
            calls: 1,
            wall_ns: 7
        }
    );
    assert_eq!(row.errors, 0);
    let mut totals = LifecycleCost::default();
    totals.add(&row);
    let before = totals;
    totals.add(&row);
    assert_eq!(totals.checked_delta(&before), Some(row));
    assert!(before.checked_delta(&totals).is_none());
}

#[test]
fn inactive_runs_do_not_issue_observer_clocks() {
    let s = State::default();
    assert!(s.start(Part::RootOpen).is_none());
    s.finish(None, None);
    let now = Instant::now();
    s.outer_at(0, now);
    s.outer_end(now);
    assert_eq!(s.snapshot(), LifecycleCost::default());
}

#[test]
fn unfinished_or_misnested_operations_are_detected() {
    let s = State::default();
    let base = Instant::now();
    s.begin_at(base);
    let parent = s.enter_at(Part::CallPushScript, base);
    s.marker(base, base, 2);
    let _child = s.enter_at(Part::RootOpen, base);
    s.marker(base, base, 2);
    s.leave_at(parent, None, base);
    s.marker(base, base, 2);
    s.end(base + Duration::from_nanos(1));
    assert!(s.snapshot().errors >= 2);
    s.reset();
    assert!(s.start(Part::RootOpen).is_none());
}
