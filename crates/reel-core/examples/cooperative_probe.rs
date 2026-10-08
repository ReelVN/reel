//! Reproducible fixture cost observation; never a device/frame-rate benchmark.
use reel_core::{
    budget::*, identity::*, result::Terminal, test_support::*, wake::WakeSignal, work::*,
};
use std::time::Instant;

fn pool(category: BudgetCategory, bytes: u64) -> BudgetPool {
    BudgetPool::new(
        category,
        QueueLimits {
            max_requests: 4,
            max_bytes: bytes,
        },
    )
}

fn main() {
    let input_pool = pool(BudgetCategory::CpuInput, 1 << 20);
    let output_pool = pool(BudgetCategory::CpuOutput, 1 << 20);
    let temporary_pool = pool(BudgetCategory::Temporary, 256);
    let completion_pool = pool(BudgetCategory::Completion, WORK_COMPLETION_BYTES);
    let data: Vec<u8> = (0..1 << 20).map(|n| (n % 251) as u8).collect();
    let input = OwnedBytes::new(data, input_pool.reserve(1 << 20).unwrap()).unwrap();
    let request = WorkRequest {
        kind: CooperativeExecutor::<FakeClock>::copy_kind(),
        input,
        scope: Scope::Process,
        priority: Priority::Normal,
        reservations: WorkReservations {
            output: output_pool.reserve(1 << 20).unwrap(),
            temporary: temporary_pool.reserve(256).unwrap(),
            completion: completion_pool.reserve(WORK_COMPLETION_BYTES).unwrap(),
        },
    };
    let mut executor = CooperativeExecutor::new(
        FakeClock::default(),
        QueueLimits {
            max_requests: 1,
            max_bytes: 3 << 20,
        },
        WakeSignal::new(|| {}),
    );
    let submit = Instant::now();
    let id = executor.submit(request).unwrap();
    println!("submit_ns={}", submit.elapsed().as_nanos());
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
    let mut events = Vec::with_capacity(1);
    executor.drain(&mut events, 1);
    assert_eq!(events[0].id, id);
    let Terminal::Completed(bytes) = &events[0].result else {
        panic!("copy failed")
    };
    assert_eq!(bytes.as_slice().len(), 1 << 20);
    for (n, b) in bytes.as_slice().iter().enumerate() {
        assert_eq!(*b, (n % 251) as u8);
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
