use super::*;

fn closed(s: &WallSnapshot) {
    assert_eq!(
        s.wall_ns,
        s.nanos.iter().sum::<u64>() + s.unscoped_ns + s.lost_ns
    );
}

#[test]
fn nested_recursive_scopes_split_at_boundaries_without_double_counting() {
    let mut s = State::new(1);
    let parent = s.enter(Phase::Session, 10);
    let child = s.enter(Phase::VmSlice, 20);
    let recursive = s.enter(Phase::VmSlice, 30);
    s.charge(40);
    let before = s.sample.clone();
    closed(&before);
    assert_eq!(before.nanos[Phase::VmSlice as usize], 20);
    s.leave(recursive, 50);
    s.leave(child, 60);
    s.leave(parent, 70);
    s.charge(80);
    let delta = s.sample.checked_delta(&before).unwrap();
    closed(&delta);
    assert_eq!(delta.wall_ns, 40);
    assert_eq!(delta.nanos[Phase::VmSlice as usize], 20);
    assert_eq!(delta.nanos[Phase::Session as usize], 10);
    assert_eq!(delta.unscoped_ns, 10);
    assert_eq!(s.sample.calls[Phase::VmSlice as usize], 2);
    assert_eq!(s.sample.clock_reads, 6);
    assert_eq!(s.sample.errors, 0);
}

#[test]
fn overflow_and_misordered_drop_remain_visible_and_partition_time() {
    let mut s = State::new(1);
    let mut tokens: Vec<_> = (0..CAPACITY + 2)
        .map(|n| s.enter(Phase::Session, n as u64))
        .collect();
    s.charge(100);
    let before = s.sample.clone();
    assert_eq!(before.errors, 2);
    assert_eq!(before.lost_ns, 100 - CAPACITY as u64);
    // Drop an outer scope while the stack has overflowed, then recover.
    s.leave(tokens.remove(0), 101);
    while let Some(token) = tokens.pop() {
        s.leave(token, 102);
    }
    s.charge(110);
    closed(&s.sample);
    assert_eq!(s.sample.errors, 3);
    assert_eq!((s.depth, s.overflow), (0, 0));
    assert_eq!(s.sample.unscoped_ns, 8);
    closed(&s.sample.checked_delta(&before).unwrap());
}

#[test]
fn opt_in_is_thread_local_and_restarting_invalidates_deltas_and_old_guards() {
    assert!(WallSnapshot::capture().is_none());
    let recording = WallRecording::start().unwrap();
    assert!(WallRecording::start().is_none());
    let outer = crate::profile::Scope::new(Phase::Session);
    let before = WallSnapshot::capture().unwrap();
    std::thread::spawn(|| {
        let _scope = crate::profile::Scope::new(Phase::Decode);
        assert!(WallSnapshot::capture().is_none());
    })
    .join()
    .unwrap();
    let after = WallSnapshot::capture().unwrap();
    let delta = after.checked_delta(&before).unwrap();
    closed(&delta);
    assert_eq!(delta.nanos[Phase::Decode as usize], 0);
    assert_eq!(delta.nanos[Phase::Session as usize], delta.wall_ns);
    drop(recording);
    assert!(WallSnapshot::capture().is_none());
    let _next = WallRecording::start().unwrap();
    drop(outer);
    let next = WallSnapshot::capture().unwrap();
    assert!(next.checked_delta(&after).is_none());
    assert_eq!(next.errors, 0);
    closed(&next);
}

#[test]
#[ignore = "timing calibration; run --release --ignored --nocapture"]
fn wall_ledger_overhead() {
    const N: usize = 100_000;
    for enabled in [false, true, true, false, false, true, true, false] {
        let recording = enabled.then(WallRecording::start);
        let start = Instant::now();
        for _ in 0..N {
            let _parent = crate::profile::Scope::new(Phase::Session);
            let _child = crate::profile::Scope::new(Phase::VmSlice);
            std::hint::black_box(1);
        }
        let elapsed = start.elapsed().as_nanos();
        let sample = WallSnapshot::capture();
        if let Some(s) = &sample {
            closed(s);
            assert_eq!(s.errors, 0);
        }
        eprintln!(
            "wall_ledger enabled={enabled} scopes={} elapsed_ns={elapsed}",
            N * 2
        );
        drop(recording);
    }
}
