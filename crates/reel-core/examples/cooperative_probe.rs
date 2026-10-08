//! Reproducible fixture cost observation; never a device/frame-rate benchmark.
use reel_core::{
    budget::*, identity::*, result::Terminal, test_support::*, wake::WakeSignal, work::*,
};
use std::time::Instant;

fn pool(category: BudgetCategory, bytes: u64) -> BudgetPool {
    BudgetPool::new(
        category,
        QueueLimits {
            max_requests: DEFAULT_COOPERATIVE_QUEUE_LIMITS.max_requests,
            max_bytes: bytes,
        },
    )
}

fn observe(label: &str, requests: u32) {
    println!("case={label} configured_requests={requests}");
    let bytes_per_request = (1 << 20) / requests as usize;
    let input_pool = pool(BudgetCategory::CpuInput, 1 << 20);
    let output_pool = pool(BudgetCategory::CpuOutput, 1 << 20);
    let temporary_pool = pool(BudgetCategory::Temporary, 256 * u64::from(requests));
    let completion_pool = pool(
        BudgetCategory::Completion,
        WORK_COMPLETION_BYTES * u64::from(requests),
    );
    let mut executor = CooperativeExecutor::new(
        FakeClock::default(),
        QueueLimits {
            max_requests: requests,
            ..DEFAULT_COOPERATIVE_QUEUE_LIMITS
        },
        WakeSignal::new(|| {}),
    );
    let mut ids = Vec::with_capacity(requests as usize);
    let mut max_submit_ns = 0;
    for _ in 0..requests {
        let data: Vec<u8> = (0..bytes_per_request).map(|n| (n % 251) as u8).collect();
        let input =
            OwnedBytes::new(data, input_pool.reserve(bytes_per_request as u64).unwrap()).unwrap();
        let request = WorkRequest {
            kind: CooperativeExecutor::<FakeClock>::copy_kind(),
            input,
            scope: Scope::Process,
            priority: Priority::Normal,
            reservations: WorkReservations {
                output: output_pool.reserve(bytes_per_request as u64).unwrap(),
                temporary: temporary_pool.reserve(256).unwrap(),
                completion: completion_pool.reserve(WORK_COMPLETION_BYTES).unwrap(),
            },
        };
        let start = Instant::now();
        ids.push(executor.submit(request).unwrap());
        max_submit_ns = max_submit_ns.max(start.elapsed().as_nanos());
    }
    assert_eq!(executor.usage().requests, requests);
    println!("max_submit_ns={max_submit_ns}");
    let mut elapsed = Vec::with_capacity(1024);
    let mut total_bytes = 0;
    loop {
        let start = Instant::now();
        let progress = executor.advance(WorkBudget {
            max_units: 4,
            max_bytes: 1024,
            deadline: None,
        });
        elapsed.push(start.elapsed().as_nanos());
        assert!(progress.units <= 4 && progress.bytes <= 1024);
        total_bytes += progress.bytes;
        if !progress.has_work {
            break;
        }
    }
    elapsed.sort_unstable();
    println!(
        "bytes={total_bytes} slices={} p50_ns={} p99_ns={} max_ns={}",
        elapsed.len(),
        elapsed[elapsed.len() / 2],
        elapsed[(elapsed.len() - 1) * 99 / 100],
        elapsed.last().unwrap()
    );
    println!(
        "peaks: input={} output={} temporary={} completion={} queue={}",
        input_pool.usage().peak_bytes,
        output_pool.usage().peak_bytes,
        temporary_pool.usage().peak_bytes,
        completion_pool.usage().peak_bytes,
        executor.usage().peak_bytes
    );
    assert_eq!(total_bytes, 1 << 20);
    let mut events = Vec::with_capacity(requests as usize);
    executor.drain(&mut events, requests as usize);
    assert_eq!(events.len(), requests as usize);
    for (event, id) in events.iter().zip(ids) {
        assert_eq!(event.id, id);
        let Terminal::Completed(bytes) = &event.result else {
            panic!("copy failed")
        };
        assert_eq!(bytes.as_slice().len(), bytes_per_request);
        for (n, b) in bytes.as_slice().iter().enumerate() {
            assert_eq!(*b, (n % 251) as u8);
        }
    }
    println!(
        "after_drain: output={} completion={} queue={}",
        output_pool.usage().bytes,
        completion_pool.usage().bytes,
        executor.usage().bytes
    );
    drop(events);
    for p in [&input_pool, &output_pool, &temporary_pool, &completion_pool] {
        assert_eq!((p.usage().tickets, p.usage().bytes), (0, 0));
    }
    println!("after_output_drop: all tickets and resident bytes returned");
}

fn main() {
    observe("single", 1);
    observe("full", DEFAULT_COOPERATIVE_QUEUE_LIMITS.max_requests);
}
