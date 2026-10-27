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
