#![cfg(feature = "test-support")]
#![allow(clippy::result_large_err)]

use reel_core::{
    budget::*, clock::*, identity::*, io::*, request_ids::RequestIds, result::*, test_support::*,
    wake::WakeSignal, work::*,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn limits(requests: u32, bytes: u64) -> QueueLimits {
    QueueLimits {
        max_requests: requests,
        max_bytes: bytes,
    }
}
fn scope(epoch: u64) -> Scope {
    Scope::Session {
        session_id: SessionId(11),
        epoch: Epoch(epoch),
        owner_id: OwnerId(7),
    }
}
fn version(n: u8) -> ResourceVersion {
    ResourceVersion {
        key: AssetKey("image/window".into()),
        version: ContentVersion([n; 32]),
    }
}
fn slot() -> SaveSlot {
    SaveSlot {
        namespace: "test-game".into(),
        slot: "quick".into(),
    }
}
fn identity(n: u64) -> ProcessRequestId {
    ProcessRequestId {
        process_id: ProcessId(44),
        request_id: RequestId(n),
    }
}
fn wake() -> (WakeSignal, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    (
        WakeSignal::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
        }),
        calls,
    )
}

struct Pools {
    input: BudgetPool,
    output: BudgetPool,
    temporary: BudgetPool,
    completion: BudgetPool,
}
impl Pools {
    fn new() -> Self {
        let pool = |c| BudgetPool::new(c, limits(64, 1 << 20));
        Self {
            input: pool(BudgetCategory::CpuInput),
            output: pool(BudgetCategory::CpuOutput),
            temporary: pool(BudgetCategory::Temporary),
            completion: pool(BudgetCategory::Completion),
        }
    }
    fn bytes(&self, bytes: &[u8]) -> OwnedBytes {
        let v = bytes.to_vec();
        let t = self.input.reserve(v.capacity() as u64).unwrap();
        OwnedBytes::new(v, t).unwrap()
    }
    fn read(&self, resource: ResourceVersion, offset: u64, len: u32, scope: Scope) -> ReadRequest {
        ReadRequest {
            deadline: None,
            resource: resource.into(),
            offset,
            len,
            scope,
            priority: Priority::Visible,
            output: self.output.reserve(len.into()).unwrap(),
            completion: self.completion.reserve(IO_COMPLETION_BYTES).unwrap(),
        }
    }
    fn describe(&self, resource: ResourceVersion, scope: Scope) -> DescribeRequest {
        DescribeRequest {
            resource: resource.into(),
            scope,
            priority: Priority::Normal,
            completion: self.completion.reserve(IO_COMPLETION_BYTES).unwrap(),
        }
    }
    fn save(&self, n: u64, bytes: &[u8], level: SaveDurability) -> SaveWrite {
        SaveWrite {
            identity: identity(n),
            slot: slot(),
            commit_sequence: n,
            data: self.bytes(bytes),
            minimum_durability: level,
            completion: self.completion.reserve(IO_COMPLETION_BYTES).unwrap(),
        }
    }
    fn work(&self, data: &[u8], kind: WorkKind, scope: Scope) -> WorkRequest {
        let output = if kind.operation == WorkOperation::Encode {
            data.len()
        } else {
            1
        };
        WorkRequest {
            kind,
            input: self.bytes(data).into(),
            scope,
            priority: Priority::Normal,
            reservations: WorkReservations {
                output: self.output.reserve(output as u64).unwrap(),
                temporary: self.temporary.reserve(data.len().min(256) as u64).unwrap(),
                completion: self.completion.reserve(WORK_COMPLETION_BYTES).unwrap(),
            },
        }
    }
    fn empty(&self) {
        for pool in [&self.input, &self.output, &self.temporary, &self.completion] {
            assert_eq!((pool.usage().tickets, pool.usage().bytes), (0, 0));
        }
    }
}

#[test]
fn moving_tickets_charge_capacity_and_return_once_across_a_thread() {
    let p = BudgetPool::new(BudgetCategory::CpuInput, limits(1, 128));
    let mut data = Vec::with_capacity(128);
    data.extend_from_slice(b"small logical length");
    let ptr = data.as_ptr();
    let rejected = OwnedBytes::new(data, p.reserve(16).unwrap()).unwrap_err();
    assert_eq!(rejected.request.0.as_ptr(), ptr);
    assert_eq!(p.usage().bytes, 16);
    drop(rejected.request.1);
    let b = OwnedBytes::new(rejected.request.0, p.reserve(128).unwrap()).unwrap();
    assert_eq!(p.reserve(1).unwrap_err().code, RejectCode::Busy);
    std::thread::spawn(move || {
        assert_eq!(b.as_slice(), b"small logical length");
        drop(b);
    })
    .join()
    .unwrap();
    assert_eq!(p.usage().bytes, 0);
    let mut t = p.reserve(32).unwrap();
    t.resize(128).unwrap();
    assert_eq!(t.resize(129).unwrap_err().code, RejectCode::Busy);
    assert_eq!(t.bytes(), 128);
    assert_eq!(p.usage().bytes, 128);
    t.resize(16).unwrap();
    assert_eq!(p.usage().bytes, 16);
    assert_eq!(p.usage().tickets, 1);
    assert_eq!(p.usage().peak_bytes, 128);
    drop(t);
    assert_eq!(p.usage().tickets, 0);

    let big = BudgetPool::new(BudgetCategory::Temporary, limits(2, u64::MAX));
    let t = big.reserve(u64::MAX).unwrap();
    assert_eq!(big.reserve(1).unwrap_err().code, RejectCode::Busy);
    drop(t);
    assert_eq!(big.usage().bytes, 0);
}

#[test]
fn grouped_capacity_rejections_and_resize_are_atomic() {
    let group = BudgetGroup::new(100);
    let input = group.pool(BudgetCategory::CpuInput, limits(2, 70));
    let gpu = group.pool(BudgetCategory::GpuResource, limits(1, 90));
    let mut a = input.reserve(60).unwrap();
    assert_eq!(input.available_bytes(), 10);
    assert_eq!(gpu.available_bytes(), 40);
    assert_eq!(gpu.reserve(50).unwrap_err().code, RejectCode::Busy);
    assert_eq!(gpu.usage(), BudgetUsage::default());
    assert_eq!(input.reserve(11).unwrap_err().code, RejectCode::Busy);
    let zero = input.clone().reserve(0).unwrap();
    assert_eq!(input.available_bytes(), 0);
    assert_eq!(input.reserve(0).unwrap_err().code, RejectCode::Busy);
    let mut b = gpu.reserve(40).unwrap();
    assert_eq!(a.category(), BudgetCategory::CpuInput);
    assert_eq!(b.category(), BudgetCategory::GpuResource);
    assert_eq!(
        group.clone().usage(),
        BudgetUsage {
            tickets: 3,
            bytes: 100,
            peak_bytes: 100
        }
    );
    let before = (input.usage(), gpu.usage(), group.usage());
    assert_eq!(a.resize(61).unwrap_err().code, RejectCode::Busy); // Group exhausted.
    assert_eq!(a.resize(71).unwrap_err().code, RejectCode::Busy); // Domain exhausted.
    assert_eq!(a.bytes(), 60);
    assert_eq!((input.usage(), gpu.usage(), group.usage()), before);
    b.resize(20).unwrap();
    assert_eq!(gpu.available_bytes(), 0);
    assert_eq!(group.usage().bytes, 80);
    a.resize(70).unwrap();
    assert_eq!(
        group.usage(),
        BudgetUsage {
            tickets: 3,
            bytes: 90,
            peak_bytes: 100
        }
    );
    assert_eq!(input.usage().peak_bytes, 70);
    assert_eq!(gpu.usage().peak_bytes, 40);
    drop((input, gpu)); // Tickets retain their domain and group accounts.
    drop((a, b, zero));
    assert_eq!(
        group.usage(),
        BudgetUsage {
            tickets: 0,
            bytes: 0,
            peak_bytes: 100
        }
    );
}

#[test]
fn grouped_peak_follows_overlap_and_shared_bytes_release_once() {
    let group = BudgetGroup::new(128);
    let input = group.pool(BudgetCategory::CpuInput, limits(2, 128));
    let output = group.pool(BudgetCategory::CpuOutput, limits(2, 128));
    drop(input.reserve(80).unwrap());
    drop(output.reserve(80).unwrap());
    assert_eq!(input.usage().peak_bytes + output.usage().peak_bytes, 160);
    assert_eq!(group.usage().peak_bytes, 80);
    let data = vec![9; 64];
    let address = data.as_ptr() as usize;
    let bytes = Arc::new(OwnedBytes::new(data, input.reserve(64).unwrap()).unwrap());
    let shared = bytes.clone();
    drop(bytes);
    let other = output.reserve(40).unwrap();
    assert_eq!(
        group.usage(),
        BudgetUsage {
            tickets: 2,
            bytes: 104,
            peak_bytes: 104
        }
    );
    let transferred = std::thread::spawn(move || {
        assert_eq!(shared.as_slice().as_ptr() as usize, address);
        let (data, ticket) = Arc::try_unwrap(shared).unwrap().into_parts();
        assert_eq!(data.as_ptr() as usize, address);
        OwnedBytes::new(data, ticket).unwrap()
    })
    .join()
    .unwrap();
    assert_eq!(group.usage().bytes, 104);
    assert_eq!(input.usage().tickets, 1);
    drop(transferred);
    assert_eq!(group.usage().bytes, 40);
    assert_eq!(input.usage().bytes, 0);
    drop(other);
    assert_eq!(
        group.usage(),
        BudgetUsage {
            tickets: 0,
            bytes: 0,
            peak_bytes: 104
        }
    );
}

#[test]
fn grouped_zero_byte_tickets_and_byte_overflow_preserve_accounting() {
    let group = BudgetGroup::new(u64::MAX);
    let a = group.pool(BudgetCategory::CpuInput, limits(1, u64::MAX));
    let b = group.pool(BudgetCategory::GpuResource, limits(1, u64::MAX));
    let mut large = a.reserve(u64::MAX).unwrap();
    assert_eq!(b.reserve(1).unwrap_err().code, RejectCode::Busy);
    let mut zero = b.reserve(0).unwrap();
    assert_eq!(group.usage().tickets, 2);
    assert_eq!(zero.resize(1).unwrap_err().code, RejectCode::Busy);
    assert_eq!(zero.bytes(), 0);
    large.resize(0).unwrap();
    zero.resize(u64::MAX).unwrap();
    drop((large, zero));
    assert_eq!(
        group.usage(),
        BudgetUsage {
            tickets: 0,
            bytes: 0,
            peak_bytes: u64::MAX
        }
    );
    let empty = BudgetGroup::new(0);
    let pool = empty.pool(BudgetCategory::Completion, limits(2, 1));
    let ticket = pool.reserve(0).unwrap();
    assert_eq!(pool.reserve(1).unwrap_err().code, RejectCode::Busy);
    assert_eq!(
        empty.usage(),
        BudgetUsage {
            tickets: 1,
            bytes: 0,
            peak_bytes: 0
        }
    );
    drop(ticket);
    assert_eq!(empty.usage(), BudgetUsage::default());
}

#[test]
fn grouped_domains_compete_atomically_across_threads() {
    let group = BudgetGroup::new(100);
    let enter = std::sync::Barrier::new(3);
    let reserved = std::sync::Barrier::new(3);
    let release = std::sync::Barrier::new(3);
    let (usage, successes) = std::thread::scope(|scope| {
        let workers: Vec<_> = [BudgetCategory::CpuOutput, BudgetCategory::GpuResource]
            .into_iter()
            .map(|category| {
                let pool = group.pool(category, limits(1, 100));
                let (enter, reserved, release) = (&enter, &reserved, &release);
                scope.spawn(move || {
                    enter.wait();
                    let result = pool.reserve(80);
                    reserved.wait();
                    release.wait();
                    result.is_ok()
                })
            })
            .collect();
        enter.wait();
        reserved.wait();
        let usage = group.usage();
        release.wait();
        (
            usage,
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .filter(|success| *success)
                .count(),
        )
    });
    assert_eq!(successes, 1);
    assert_eq!(
        usage,
        BudgetUsage {
            tickets: 1,
            bytes: 80,
            peak_bytes: 80
        }
    );
    assert_eq!(
        group.usage(),
        BudgetUsage {
            tickets: 0,
            bytes: 0,
            peak_bytes: 80
        }
    );
}

#[test]
fn read_pending_cancel_is_unique_and_rejection_keeps_original_under_backpressure() {
    let p = Pools::new();
    let (signal, calls) = wake();
    let mut io = MemoryAssetIo::new(limits(1, 4096), limits(1, 4096), signal.clone());
    let first = io.read(p.read(version(1), 0, 8, scope(1))).unwrap();
    let r = p.read(version(1), 0, 8, scope(2));
    let key_ptr = r.resource.key.0.as_ptr();
    let rejected = io.read(r).unwrap_err();
    assert_eq!(rejected.reason.code, RejectCode::Busy);
    assert!(rejected.reason.retryable());
    assert_eq!(rejected.request.resource.key.0.as_ptr(), key_ptr);
    assert_eq!(rejected.request.scope, scope(2));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    io.cancel(first);
    io.cancel(first);
    assert!(!io.complete_next_read());
    assert_eq!(io.read_usage().completed, 1);
    assert_eq!(p.output.usage().bytes, 8); // only rejected request remains
    let rejected = io.read(rejected.request).unwrap_err();
    assert_eq!(rejected.reason.code, RejectCode::Busy);
    let mut events = Vec::with_capacity(2);
    io.poll(&mut events, 0);
    assert!(events.is_empty());
    io.poll(&mut events, 1);
    assert_eq!(events[0].id, first);
    assert_eq!(events[0].scope, scope(1));
    assert!(matches!(events[0].result, Terminal::Cancelled));
    assert_eq!(io.read_usage().requests, 0);
    let retry = io.read(rejected.request).unwrap();
    assert_eq!(retry.0, first.0 + 1); // rejected calls did not consume accepted IDs
    io.cancel(retry);
    io.poll(&mut events, 1);
    io.poll(&mut events, 8);
    assert_eq!(events.len(), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 1); // coalesced, not one callback per event
    drop(events);
    drop(io);
    p.empty();
}

#[test]
fn fixed_versions_ranges_failures_and_stale_scope_keep_owned_output() {
    let p = Pools::new();
    let (signal, _) = wake();
    let mut io = MemoryAssetIo::new(limits(8, 16384), limits(1, 4096), signal);
    io.insert_asset(
        version(1),
        MemoryAsset {
            bytes: p.bytes(b"old-image"),
            media_type: Some("image/fixture".into()),
        },
    )
    .unwrap();
    io.insert_asset(
        version(2),
        MemoryAsset {
            bytes: p.bytes(b"new-image"),
            media_type: None,
        },
    )
    .unwrap();
    let duplicate = io
        .insert_asset(
            version(1),
            MemoryAsset {
                bytes: p.bytes(b"overwrite"),
                media_type: None,
            },
        )
        .unwrap_err();
    drop(duplicate);
    let described = io.describe(p.describe(version(1), scope(1))).unwrap();
    io.complete_next_read();
    let old = io.read(p.read(version(1), 4, 99, scope(1))).unwrap();
    io.complete_next_read();
    let missing = io.read(p.read(version(3), 0, 4, scope(2))).unwrap();
    io.complete_next_read();
    io.read(p.read(version(2), u64::MAX, 0, scope(2))).unwrap();
    io.complete_next_read();
    let invalid = io
        .read(p.read(version(2), u64::MAX, 1, scope(2)))
        .unwrap_err();
    assert_eq!(invalid.reason.code, RejectCode::InvalidRequest);
    drop(invalid);
    let mut events = Vec::with_capacity(4);
    io.poll(&mut events, 2);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].id, described);
    let Terminal::Completed(IoPayload::Described(info)) = &events[0].result else {
        panic!("describe did not succeed")
    };
    assert_eq!(
        (info.length, info.version, info.range_read, info.seekable),
        (Some(9), Some(version(1).version), true, true)
    );
    assert_eq!(info.media_type.as_deref(), Some("image/fixture"));
    assert_eq!(events[1].id, old);
    let Terminal::Completed(IoPayload::Read(result)) = &events[1].result else {
        panic!("read did not succeed")
    };
    assert_eq!(result.bytes.as_slice(), b"image");
    assert!(result.eof);
    assert_eq!(result.version, version(1).version);
    // A load advanced epoch, so the runtime releases this event instead of delivering it.
    assert_ne!(events[1].scope, scope(2));
    assert_eq!(p.output.usage().bytes, 99); // output remains charged after delivery
    events.clear();
    assert_eq!(p.output.usage().bytes, 0); // missing/empty outputs own no data
    io.poll(&mut events, 8);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].id, missing);
    assert!(
        matches!(&events[0].result, Terminal::Failed(e) if e.code == IoErrorCode::VersionUnavailable)
    );
    let Terminal::Completed(IoPayload::Read(empty)) = &events[1].result else {
        panic!("expected EOF")
    };
    assert!(empty.bytes.as_slice().is_empty() && empty.eof);
    drop(events);
    drop(io);
    p.empty();
}

#[test]
fn save_pending_cancel_and_cancel_after_settlement_preserve_actual_commit() {
    let p = Pools::new();
    let (signal, _) = wake();
    let mut io = MemoryAssetIo::new(limits(1, 4096), limits(2, 8192), signal);
    let unsupported = p.save(1, b"durable", SaveDurability::Durable);
    let ptr = unsupported.data.as_slice().as_ptr();
    let rejected = io.replace_save(unsupported).unwrap_err();
    assert_eq!(rejected.reason.code, RejectCode::Unsupported);
    assert!(!rejected.reason.retryable());
    assert_eq!(rejected.request.data.as_slice().as_ptr(), ptr);
    assert!(io.saved(&slot()).is_none());
    drop(rejected);
    let reading = io.read(p.read(version(1), 0, 4, scope(1))).unwrap(); // fills read lane
    let writing = io
        .replace_save(p.save(1, b"old", SaveDurability::Visible))
        .unwrap();
    let writing2 = io
        .replace_save(p.save(2, b"new", SaveDurability::Visible))
        .unwrap();
    let duplicate_order = io
        .replace_save(p.save(1, b"backwards", SaveDurability::Visible))
        .unwrap_err();
    assert_eq!(duplicate_order.reason.code, RejectCode::InvalidRequest);
    drop(duplicate_order);
    let blocked = io
        .replace_save(p.save(3, b"later", SaveDurability::Visible))
        .unwrap_err();
    assert_eq!(blocked.reason.code, RejectCode::Busy);
    io.cancel(reading); // a session exit does not touch the process-owned save lane
    assert!(io.complete_next_save());
    assert_eq!(io.saved(&slot()).unwrap().data.as_slice(), b"old");
    io.fail_next_save();
    assert!(io.complete_next_save());
    assert_eq!(io.saved(&slot()).unwrap().data.as_slice(), b"old");
    let mut events = Vec::with_capacity(4);
    io.poll(&mut events, 2);
    assert_eq!(events.len(), 2); // combined lanes honor one total limit
    assert_eq!(events[1].id, writing);
    assert_eq!(events[1].scope, Scope::Process);
    assert_eq!(events[1].process_request, Some(identity(1)));
    assert!(
        matches!(&events[1].result, Terminal::Completed(IoPayload::Saved(c)) if c.durability == SaveDurability::Visible)
    );
    io.poll(&mut events, 1);
    assert_eq!(events[2].id, writing2);
    assert!(
        matches!(&events[2].result, Terminal::Failed(e) if e.code == IoErrorCode::StorageFailure)
    );
    let cancelled = io.replace_save(blocked.request).unwrap();
    io.cancel(cancelled);
    io.cancel(cancelled);
    assert!(!io.complete_next_save());
    io.poll(&mut events, 8);
    assert_eq!(events[3].process_request, Some(identity(3)));
    assert!(matches!(events[3].result, Terminal::Cancelled));
    assert_eq!(io.saved(&slot()).unwrap().data.as_slice(), b"old");
    let last = io
        .replace_save(p.save(4, b"final", SaveDurability::Visible))
        .unwrap();
    io.complete_next_save();
    io.cancel(last); // completed already won; it is never settled twice
    io.poll(&mut events, 8);
    assert_eq!(events.len(), 5);
    assert_eq!(io.saved(&slot()).unwrap().data.as_slice(), b"final");
    assert_eq!(p.input.usage().bytes, 5); // only current stored slot survives
    drop(events);
    drop(io);
    p.empty();
}

#[test]
fn work_cancel_after_settlement_keeps_owned_terminal_under_backpressure() {
    let p = Pools::new();
    let (signal, _) = wake();
    let mut executor = CooperativeExecutor::new(FakeClock::default(), limits(1, 4096), signal);
    let data: Vec<_> = (0..1024).map(|n| (n % 251) as u8).collect();
    let id = executor
        .submit(p.work(
            &data,
            CooperativeExecutor::<FakeClock>::copy_kind(),
            scope(1),
        ))
        .unwrap();
    assert_eq!(p.input.usage().bytes, 1024);
    assert_eq!(p.output.usage().bytes, 1024);
    assert_eq!(p.temporary.usage().bytes, 256);
    let mut events = Vec::with_capacity(1);
    let budget = WorkBudget {
        max_units: 1,
        max_bytes: 100,
        deadline: None,
    };
    for _ in 0..10 {
        let progress = executor.advance(budget);
        assert_eq!(
            (progress.units, progress.bytes, progress.has_work),
            (1, 100, true)
        );
        executor.drain(&mut events, 1);
        assert!(events.is_empty());
    }
    let last = executor.advance(budget);
    assert_eq!((last.units, last.bytes, last.has_work), (1, 24, false));
    assert_eq!((p.input.usage().bytes, p.temporary.usage().bytes), (0, 0));
    assert_eq!(executor.usage().requests, 1);
    let rejected = executor
        .submit(p.work(
            b"retry",
            CooperativeExecutor::<FakeClock>::copy_kind(),
            scope(2),
        ))
        .unwrap_err();
    assert_eq!(rejected.reason.code, RejectCode::Busy);
    executor.cancel(id); // terminal completion remains unique
    executor.drain(&mut events, 0);
    assert!(events.is_empty());
    executor.drain(&mut events, 1);
    assert_eq!(events[0].id, id);
    assert_eq!(events[0].scope, scope(1));
    let Terminal::Completed(b) = &events[0].result else {
        panic!("copy did not complete")
    };
    assert_eq!(b.as_slice(), data);
    assert_eq!(p.output.usage().bytes, 1029); // delivered output + rejected request
    assert_eq!(executor.usage().bytes, 0);
    drop(events);
    let retry = executor.submit(rejected.request).unwrap();
    executor.advance(budget);
    executor.cancel(retry);
    executor.drain(&mut Vec::with_capacity(1), 1);
    drop(executor);
    p.empty();
}

#[test]
fn cooperative_deadline_priority_partial_cancel_and_checksum_failure() {
    let p = Pools::new();
    let (signal, _) = wake();
    let clock = FakeClock::default();
    clock.set_wall_time(Some(UnixMillis(-123)));
    assert_eq!(clock.wall_time(), Some(UnixMillis(-123)));
    clock.set_wall_time(None);
    clock.set_observation_step(10);
    let mut executor = CooperativeExecutor::new(clock.clone(), limits(4, 16384), signal);
    let low = executor
        .submit(p.work(
            &[2; 1024],
            CooperativeExecutor::<FakeClock>::copy_kind(),
            scope(1),
        ))
        .unwrap();
    let mut high = p.work(
        &[3; 1024],
        CooperativeExecutor::<FakeClock>::copy_kind(),
        scope(2),
    );
    high.priority = Priority::Visible;
    let high = executor.submit(high).unwrap();
    let progress = executor.advance(WorkBudget {
        max_units: 100,
        max_bytes: 4096,
        deadline: Some(MonoTime(25)),
    });
    assert_eq!(
        (progress.units, progress.bytes, progress.has_work),
        (3, 768, true)
    );
    let mut events = Vec::with_capacity(4);
    executor.cancel(high);
    executor.cancel(high);
    executor.advance(WorkBudget {
        max_units: 1,
        max_bytes: 0,
        deadline: None,
    });
    executor.drain(&mut events, 1);
    assert_eq!(events[0].id, high);
    assert!(matches!(events[0].result, Terminal::Cancelled));
    assert_eq!(p.output.usage().bytes, 1024); // all partial high output was released
    let remaining = executor.advance(WorkBudget {
        max_units: 4,
        max_bytes: 1024,
        deadline: None,
    });
    // All three deadline-limited units went to Visible; Normal still needs four.
    assert_eq!(
        (remaining.units, remaining.bytes, remaining.has_work),
        (4, 1024, false)
    );
    executor.cancel(low); // completion already won
    let bad = executor
        .submit(p.work(
            &[1, 2, 4],
            CooperativeExecutor::<FakeClock>::xor_kind(),
            Scope::Process,
        ))
        .unwrap();
    let good = executor
        .submit(p.work(
            &[1, 2, 3],
            CooperativeExecutor::<FakeClock>::xor_kind(),
            Scope::Process,
        ))
        .unwrap();
    let progress = executor.advance(WorkBudget {
        max_units: 2,
        max_bytes: 6,
        deadline: None,
    });
    assert_eq!(
        (progress.units, progress.bytes, progress.has_work),
        (2, 6, false)
    );
    executor.cancel(bad);
    executor.drain(&mut events, 8);
    assert_eq!(events.len(), 4);
    assert!(
        matches!(&events[2].result, Terminal::Failed(e) if e.code == WorkErrorCode::InvalidInput)
    );
    assert_eq!(events[2].id, bad);
    assert_eq!(events[3].id, good);
    let Terminal::Completed(bytes) = &events[3].result else {
        panic!("checksum should verify")
    };
    assert_eq!(bytes.as_slice(), &[0]);
    drop(events);
    drop(executor);
    p.empty();
}

#[test]
fn work_rejects_unregistered_kind_and_unreserved_output_or_temporary() {
    let p = Pools::new();
    let (signal, calls) = wake();
    let mut executor = CooperativeExecutor::new(FakeClock::default(), limits(2, 4096), signal);
    let mut request = p.work(
        b"owned",
        CooperativeExecutor::<FakeClock>::copy_kind(),
        scope(1),
    );
    let ptr = request.input.as_slice().as_ptr();
    request.kind.version = 2;
    let rejected = executor.submit(request).unwrap_err();
    assert_eq!(rejected.reason.code, RejectCode::Unsupported);
    assert_eq!(rejected.request.input.as_slice().as_ptr(), ptr);
    let mut request = rejected.request;
    request.kind.version = 1;
    request.reservations.output = p.output.reserve(4).unwrap();
    let rejected = executor.submit(request).unwrap_err();
    assert_eq!(rejected.reason.code, RejectCode::InvalidRequest);
    let mut request = rejected.request;
    request.reservations.output = p.output.reserve(5).unwrap();
    request.reservations.temporary = p.temporary.reserve(4).unwrap();
    let rejected = executor.submit(request).unwrap_err();
    assert_eq!(rejected.reason.code, RejectCode::InvalidRequest);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(executor.usage().requests, 0);
    drop(rejected);
    let empty = executor
        .submit(p.work(
            &[],
            CooperativeExecutor::<FakeClock>::copy_kind(),
            Scope::Process,
        ))
        .unwrap();
    assert_eq!(
        executor
            .advance(WorkBudget {
                max_units: 0,
                max_bytes: 0,
                deadline: None
            })
            .units,
        0
    );
    let progress = executor.advance(WorkBudget {
        max_units: 1,
        max_bytes: 0,
        deadline: None,
    });
    assert_eq!(
        (progress.units, progress.bytes, progress.has_work),
        (1, 0, false)
    );
    let mut out = Vec::with_capacity(1);
    executor.drain(&mut out, 1);
    assert_eq!(out[0].id, empty);
    drop(out);
    p.empty();
}

#[test]
fn queue_byte_limits_apply_even_when_request_slots_remain() {
    let p = Pools::new();
    let (signal, _) = wake();
    let mut io = MemoryAssetIo::new(limits(8, 700), limits(8, 700), signal);
    let rejected = io.read(p.read(version(1), 0, 1024, scope(1))).unwrap_err();
    assert_eq!(rejected.reason.code, RejectCode::Busy);
    assert_eq!(io.read_usage().requests, 0);
    assert_eq!(p.output.usage().bytes, 1024);
    drop(rejected);
    let (signal, _) = wake();
    let mut executor = CooperativeExecutor::new(FakeClock::default(), limits(8, 700), signal);
    let rejected = executor
        .submit(p.work(
            &[1; 1024],
            CooperativeExecutor::<FakeClock>::copy_kind(),
            Scope::Process,
        ))
        .unwrap_err();
    assert_eq!(rejected.reason.code, RejectCode::Busy);
    assert_eq!(executor.usage().requests, 0);
    drop(rejected);
    p.empty();
}

#[test]
fn wake_clones_coalesce_and_acknowledge_before_drain_preserves_racing_wake() {
    let (signal, calls) = wake();
    let worker = signal.clone();
    std::thread::spawn(move || {
        for _ in 0..100 {
            worker.wake();
        }
    })
    .join()
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(signal.acknowledge());
    let worker = signal.clone();
    std::thread::spawn(move || worker.wake()).join().unwrap();
    assert!(signal.is_pending());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(signal.acknowledge());
    assert!(!signal.acknowledge());
}

#[test]
fn service_ids_survive_adapter_replacement_and_exhaust_without_accepting() {
    let p = Pools::new();
    let ids = RequestIds::<IoId>::starting_at(u64::MAX - 1);
    let (signal, _) = wake();
    let mut old = MemoryAssetIo::with_ids(
        limits(1, 4096),
        limits(1, 4096),
        signal.clone(),
        ids.clone(),
    );
    let first = old.read(p.read(version(1), 0, 1, scope(1))).unwrap();
    let rejected = old.read(p.read(version(1), 0, 1, scope(1))).unwrap_err();
    assert_eq!(rejected.reason.code, RejectCode::Busy);
    let mut replacement = MemoryAssetIo::with_ids(limits(1, 4096), limits(1, 4096), signal, ids);
    let last = replacement
        .replace_save(p.save(1, b"last", SaveDurability::Visible))
        .unwrap();
    assert_eq!((first.0, last.0), (u64::MAX - 1, u64::MAX));
    // Different adapter and lane still use the one service counter; it cannot wrap.
    let ptr = rejected.request.resource.key.0.as_ptr();
    let exhausted = replacement.read(rejected.request).unwrap_err();
    assert_eq!(exhausted.reason.code, RejectCode::ServiceFault);
    assert!(!exhausted.reason.retryable());
    assert_eq!(exhausted.request.resource.key.0.as_ptr(), ptr);
    assert_eq!(replacement.read_usage().requests, 0);
    assert_eq!(replacement.save_usage().requests, 1);
    drop(exhausted);
    old.cancel(first);
    replacement.cancel(last);
    let mut events = Vec::with_capacity(2);
    old.poll(&mut events, 1);
    replacement.poll(&mut events, 1);
    assert_eq!((events[0].id, events[1].id), (first, last));
    assert!(
        events
            .iter()
            .all(|e| matches!(e.result, Terminal::Cancelled))
    );
    drop(events);
    drop(old);
    drop(replacement);
    p.empty();
}

#[test]
fn continuous_reads_deliver_a_ready_save_within_two_single_event_polls() {
    let p = Pools::new();
    let (signal, _) = wake();
    let mut io = MemoryAssetIo::new(limits(1, 4096), limits(1, 4096), signal);
    let save = io
        .replace_save(p.save(1, b"committed", SaveDurability::Visible))
        .unwrap();
    io.complete_next_save();
    let mut events = Vec::with_capacity(3);
    for _ in 0..2 {
        io.read(p.read(version(1), 0, 4, scope(1))).unwrap();
        io.complete_next_read(); // another read result is ready before every poll
        let before = events.len();
        io.poll(&mut events, 1);
        assert_eq!(events.len(), before + 1);
        if events.last().unwrap().id == save {
            break;
        }
    }
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].id, save);
    assert!(matches!(
        events[1].result,
        Terminal::Completed(IoPayload::Saved(_))
    ));
    assert_eq!(io.save_usage().requests, 0);
    io.poll(&mut events, 1);
    assert_eq!(events.len(), 3); // the second read is also retained and delivered
    drop(events);
    drop(io);
    p.empty();
}

#[test]
fn configured_default_full_work_queue_makes_progress_and_delivers_every_payload() {
    let p = Pools::new();
    let (signal, _) = wake();
    let mut executor = CooperativeExecutor::new(
        FakeClock::default(),
        DEFAULT_COOPERATIVE_QUEUE_LIMITS,
        signal,
    );
    let mut ids = Vec::new();
    for n in 0..DEFAULT_COOPERATIVE_QUEUE_LIMITS.max_requests {
        ids.push(
            executor
                .submit(p.work(
                    &[n as u8; 64],
                    CooperativeExecutor::<FakeClock>::copy_kind(),
                    Scope::Process,
                ))
                .unwrap(),
        );
    }
    let rejected = executor
        .submit(p.work(
            b"overflow",
            CooperativeExecutor::<FakeClock>::copy_kind(),
            Scope::Process,
        ))
        .unwrap_err();
    assert_eq!(rejected.reason.code, RejectCode::Busy);
    drop(rejected);
    let mut events = Vec::with_capacity(ids.len());
    for _ in &ids {
        let progress = executor.advance(WorkBudget {
            max_units: 1,
            max_bytes: 64,
            deadline: None,
        });
        assert_eq!((progress.units, progress.bytes), (1, 64));
        executor.drain(&mut events, 1);
    }
    for (n, e) in events.iter().enumerate() {
        assert_eq!(e.id, ids[n]);
        let Terminal::Completed(bytes) = &e.result else {
            panic!("full queue lost result")
        };
        assert_eq!(bytes.as_slice(), &[n as u8; 64]);
    }
    assert_eq!(executor.usage().requests, 0);
    drop(events);
    p.empty();
}
