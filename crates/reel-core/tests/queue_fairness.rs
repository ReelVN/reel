use reel_core::{budget::QueueLimits, identity::Priority, queue::Queue};

#[test]
fn realtime_slots_include_active_and_undrained_owners_and_bursts_are_bounded() {
    let limits = QueueLimits {
        max_requests: 4,
        max_bytes: 1024,
    };
    let mut queue = Queue::<u64, u64>::with_realtime_reserve(limits, 2);
    for id in 1..=2 {
        queue.accept(id, id, 1, Priority::Visible).unwrap();
    }
    assert!(queue.accept(3, 3, 1, Priority::Visible).is_err());
    for id in 3..=4 {
        queue.accept(id, id, 1, Priority::Realtime).unwrap();
    }
    let mut order = Vec::new();
    for _ in 0..10 {
        let entry = queue.pop().unwrap();
        order.push(entry.slot.id);
        queue.requeue(entry);
    }
    assert_eq!(order, [3, 4, 3, 4, 1, 3, 4, 3, 4, 2]);
    while let Some(entry) = queue.pop() {
        queue.finish(entry.request, entry.slot);
    }
    assert_eq!(queue.usage().requests, 4);
    assert!(queue.accept(5, 5, 1, Priority::Realtime).is_err());
    let mut events = Vec::new();
    queue.drain(&mut events, 4);
    assert_eq!(queue.usage().bytes, 0);
    assert_eq!(events.len(), 4);
    // Existing normal and realtime owners can coexist up to the total ceiling.
    queue.accept(6, 6, 1, Priority::Realtime).unwrap();
    queue.accept(7, 7, 1, Priority::Realtime).unwrap();
    queue.accept(8, 8, 1, Priority::Normal).unwrap();
    queue.accept(9, 9, 1, Priority::Normal).unwrap();
    assert_eq!(queue.usage().requests, 4);
}

#[test]
fn ordinary_queue_defaults_and_zero_or_single_slot_limits_keep_their_contract() {
    let mut queue = Queue::<u8, u8>::new(QueueLimits {
        max_requests: 3,
        max_bytes: 3,
    });
    for (id, priority) in [
        (1, Priority::Prefetch),
        (2, Priority::Normal),
        (3, Priority::Visible),
    ] {
        queue.accept(u64::from(id), id, 1, priority).unwrap();
    }
    assert_eq!(
        (0..3)
            .map(|_| queue.pop().unwrap().request)
            .collect::<Vec<_>>(),
        [3, 2, 1]
    );
    for slots in [0, 1] {
        let mut queue = Queue::<u8, u8>::with_realtime_reserve(
            QueueLimits {
                max_requests: slots,
                max_bytes: 16,
            },
            2,
        );
        assert_eq!(queue.accept(1, 1, 1, Priority::Normal).is_ok(), slots == 1);
        assert!(queue.accept(2, 2, 1, Priority::Realtime).is_err());
    }
}

#[test]
fn soft_deadlines_refresh_after_pause_and_reduce_trace_misses_without_changing_priority() {
    use reel_core::io::ReadDeadline;
    use std::time::{Duration, Instant};
    let now = Instant::now();
    // One millisecond per dispatch; FIFO makes the two short-buffer readers late.
    let mut fifo_misses = 0;
    let mut edf = Queue::<u64, u64>::with_realtime_reserve(
        QueueLimits {
            max_requests: 5,
            max_bytes: 5,
        },
        1,
    );
    for (id, due) in [(0, 9), (1, 8), (2, 1), (3, 2)] {
        fifo_misses += u64::from(id > due);
        edf.accept_before(
            id,
            id,
            1,
            Priority::Realtime,
            Some(ReadDeadline::at(now + Duration::from_millis(due))),
        )
        .unwrap();
    }
    let mut order = Vec::new();
    for elapsed in 0..4 {
        let entry = edf.pop_at(now + Duration::from_millis(elapsed)).unwrap();
        order.push(entry.request);
        edf.finish(entry.request, entry.slot);
    }
    assert_eq!(order, [2, 3, 1, 0]);
    assert_eq!(edf.usage().deadline_misses, 0);
    assert_eq!(fifo_misses, 2);
    println!(
        "four-request 1ms service trace: FIFO misses={fifo_misses}, EDF misses={}",
        edf.usage().deadline_misses
    );

    let hint = ReadDeadline::at(now - Duration::from_secs(1));
    let mut queue = Queue::<u8, u8>::new(QueueLimits {
        max_requests: 2,
        max_bytes: 2,
    });
    queue
        .accept_before(1, 1, 1, Priority::Realtime, Some(hint.clone()))
        .unwrap();
    queue
        .accept_before(
            2,
            2,
            1,
            Priority::Realtime,
            Some(ReadDeadline::at(now + Duration::from_secs(2))),
        )
        .unwrap();
    hint.pause(true);
    hint.buffered(Duration::ZERO);
    assert!(
        hint.get().is_none(),
        "worker cannot republish while suspended"
    );
    let entry = queue.pop_at(now).unwrap();
    assert_eq!(entry.request, 2);
    queue.requeue(entry);
    hint.pause(false);
    hint.buffered(Duration::ZERO);
    assert_eq!(
        queue.pop_at(now).unwrap().request,
        1,
        "pending reader sees refreshed deadline"
    );
}

#[test]
fn deadline_metadata_lives_until_last_queued_handle_and_rate_uses_buffer_time() {
    use reel_core::{
        budget::{BudgetCategory, BudgetPool},
        io::ReadDeadline,
    };
    use std::time::{Duration, Instant};
    let pool = BudgetPool::new(
        BudgetCategory::Metadata,
        QueueLimits {
            max_requests: 1,
            max_bytes: 1024,
        },
    );
    let hint = ReadDeadline::charged(&pool).unwrap();
    let reader = hint.clone();
    let bytes = pool.usage().bytes;
    assert!(bytes > 0);
    hint.rate(4.);
    let before = Instant::now();
    hint.buffered(Duration::from_secs(4));
    let after = Instant::now();
    assert!(hint.get().unwrap() >= before + Duration::from_secs(1));
    assert!(hint.get().unwrap() <= after + Duration::from_secs(1));
    hint.pause(true);
    drop(hint);
    assert_eq!(pool.usage().bytes, bytes);
    drop(reader);
    assert_eq!(pool.usage().bytes, 0);
}

#[test]
#[ignore = "CPU steady-state queue dispatch measurement; run release with nocapture"]
fn steady_state_dispatch_cost() {
    use reel_core::io::ReadDeadline;
    use std::{
        hint::black_box,
        time::{Duration, Instant},
    };
    let iterations = 200_000;
    for depth in [1, 16, 256] {
        for deadline_handles in [false, true] {
            let mut samples = Vec::new();
            for _ in 0..3 {
                let mut queue = Queue::<u64, u64>::new(QueueLimits {
                    max_requests: depth,
                    max_bytes: u64::from(depth),
                });
                for id in 0..u64::from(depth) {
                    queue
                        .accept_before(
                            id,
                            id,
                            1,
                            Priority::Normal,
                            deadline_handles.then(|| {
                                ReadDeadline::at(Instant::now() + Duration::from_secs(60))
                            }),
                        )
                        .unwrap();
                }
                let start = Instant::now();
                for _ in 0..iterations {
                    let entry = black_box(&mut queue).pop().unwrap();
                    black_box(entry.request);
                    queue.requeue(entry);
                }
                samples.push(start.elapsed().as_nanos() / iterations);
                assert_eq!(queue.usage().requests, depth);
            }
            samples.sort_unstable();
            println!(
                "dispatch depth={depth} handles={deadline_handles} median_ns={} (pop+requeue, {iterations} iterations)",
                samples[1]
            );
        }
    }
}
