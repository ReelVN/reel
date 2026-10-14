//! Standalone rustc test, deliberately outside Cargo's production crate/lints.
//! The only unsafe code forwards valid GlobalAlloc arguments to System. Observers
//! never dereference pointers, panic, or format output inside allocator callbacks.
#![deny(unsafe_op_in_unsafe_fn)]

use reel_core::{
    budget::*, identity::*, request_ids::RequestIds, result::Terminal, test_support::*,
    wake::WakeSignal, work::*,
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

struct ProbeAllocator;
#[global_allocator]
static ALLOCATOR: ProbeAllocator = ProbeAllocator;
static CAPTURE_SUBMIT: AtomicBool = AtomicBool::new(false);
static IN_HOOK: AtomicBool = AtomicBool::new(false);
static STATE: Mutex<Option<State>> = Mutex::new(None);

#[derive(Clone, Copy, Debug)]
struct Record {
    bytes_still_live: usize,
    charged: u64,
    reserve_blocked: bool,
}
struct Watch {
    addr: usize,
    size: usize,
    pool: BudgetPool,
    record: Option<Record>,
}
struct State {
    watches: [Watch; 3],
}

// SAFETY: Each call forwards the original pointer and Layout to System exactly
// once. Tracking uses integer addresses only and does not change allocator state.
unsafe impl GlobalAlloc for ProbeAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: GlobalAlloc's caller supplies a valid allocation layout.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null()
            && CAPTURE_SUBMIT.load(Ordering::SeqCst)
            && layout.align() == 1
            && !IN_HOOK.swap(true, Ordering::SeqCst)
        {
            if let Ok(mut state) = STATE.lock() {
                if let Some(state) = state.as_mut() {
                    for watch in &mut state.watches[1..] {
                        if watch.addr == 0 && watch.size == layout.size() {
                            watch.addr = ptr as usize;
                        }
                    }
                }
            }
            IN_HOOK.store(false, Ordering::SeqCst);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if !IN_HOOK.swap(true, Ordering::SeqCst) {
            if let Ok(mut state) = STATE.lock() {
                if let Some(state) = state.as_mut() {
                    for watch in &mut state.watches {
                        if watch.addr == ptr as usize && watch.record.is_none() {
                            let charged = watch.pool.usage().bytes;
                            let reservation = watch.pool.reserve(watch.size as u64);
                            watch.record = Some(Record {
                                bytes_still_live: layout.size(),
                                charged,
                                reserve_blocked: reservation.is_err(),
                            });
                            drop(reservation);
                        }
                    }
                }
            }
            IN_HOOK.store(false, Ordering::SeqCst);
        }
        // SAFETY: Forward the same allocated pointer and layout after observing.
        unsafe { System.dealloc(ptr, layout) };
    }
}

#[derive(Clone, Copy, Debug)]
enum Exit {
    Completed,
    Failed,
    Cancelled,
    ExecutorDrop,
    Rejected,
}

fn pool(category: BudgetCategory, bytes: u64) -> BudgetPool {
    BudgetPool::new(
        category,
        QueueLimits {
            max_requests: 1,
            max_bytes: bytes,
        },
    )
}

fn observe(exit: Exit) -> [Record; 3] {
    const INPUT: usize = 65_537;
    let input_pool = pool(BudgetCategory::CpuInput, INPUT as u64);
    let output_size = if matches!(exit, Exit::Failed) {
        1
    } else {
        INPUT
    };
    let output_pool = pool(BudgetCategory::CpuOutput, output_size as u64);
    let temporary_pool = pool(BudgetCategory::Temporary, 256);
    let completion_pool = pool(BudgetCategory::Completion, WORK_COMPLETION_BYTES);
    let data = vec![1; INPUT]; // odd length makes XOR verification genuinely fail
    let input_addr = data.as_ptr() as usize;
    let input = OwnedBytes::new(data, input_pool.reserve(INPUT as u64).unwrap()).unwrap();
    let kind = if matches!(exit, Exit::Failed) {
        CooperativeExecutor::<FakeClock>::xor_kind()
    } else {
        CooperativeExecutor::<FakeClock>::copy_kind()
    };
    let request = WorkRequest {
        kind,
        input: input.into(),
        scope: Scope::Process,
        priority: Priority::Normal,
        reservations: WorkReservations {
            output: output_pool.reserve(output_size as u64).unwrap(),
            temporary: temporary_pool.reserve(256).unwrap(),
            completion: completion_pool.reserve(WORK_COMPLETION_BYTES).unwrap(),
        },
    };
    let ids = RequestIds::<WorkId>::starting_at(u64::MAX);
    if matches!(exit, Exit::Rejected) {
        ids.accept((), |_, _| Ok(())).unwrap();
    }
    let mut executor = CooperativeExecutor::with_ids(
        FakeClock::default(),
        QueueLimits {
            max_requests: 1,
            max_bytes: 1 << 20,
        },
        WakeSignal::new(|| {}),
        ids,
    );
    *STATE.lock().unwrap() = Some(State {
        watches: [
            Watch {
                addr: input_addr,
                size: INPUT,
                pool: input_pool.clone(),
                record: None,
            },
            Watch {
                addr: 0,
                size: output_size,
                pool: output_pool.clone(),
                record: None,
            },
            Watch {
                addr: 0,
                size: 256,
                pool: temporary_pool.clone(),
                record: None,
            },
        ],
    });
    CAPTURE_SUBMIT.store(true, Ordering::SeqCst);
    let submitted = executor.submit(request);
    CAPTURE_SUBMIT.store(false, Ordering::SeqCst);
    let mut events = Vec::with_capacity(1);
    match exit {
        Exit::Completed | Exit::Failed => {
            submitted.unwrap();
            executor.advance(WorkBudget {
                max_units: u32::MAX,
                max_bytes: u64::MAX,
                deadline: None,
            });
            executor.drain(&mut events, 1);
            assert_eq!(events.len(), 1);
            assert!(match exit {
                Exit::Completed => matches!(events[0].result, Terminal::Completed(_)),
                _ => matches!(events[0].result, Terminal::Failed(_)),
            });
        }
        Exit::Cancelled => {
            let id = submitted.unwrap();
            executor.advance(WorkBudget {
                max_units: 1,
                max_bytes: 256,
                deadline: None,
            });
            executor.cancel(id);
            executor.drain(&mut events, 1);
            assert!(matches!(events[0].result, Terminal::Cancelled));
        }
        Exit::ExecutorDrop => {
            submitted.unwrap();
        }
        Exit::Rejected => {
            let rejected = submitted.unwrap_err();
            assert_eq!(
                rejected.request.input.as_slice().as_ptr() as usize,
                input_addr
            );
            drop(rejected);
        }
    }
    drop(events);
    drop(executor);
    let state = STATE.lock().unwrap().take().unwrap();
    let records = state
        .watches
        .map(|w| w.record.expect("allocation was not observed"));
    for p in [&input_pool, &output_pool, &temporary_pool, &completion_pool] {
        assert_eq!(p.usage().bytes, 0);
    }
    records
}

#[test]
fn buffers_remain_charged_until_system_dealloc_on_every_exit() {
    let mut violations = 0;
    for exit in [
        Exit::Completed,
        Exit::Failed,
        Exit::Cancelled,
        Exit::ExecutorDrop,
        Exit::Rejected,
    ] {
        let records = observe(exit);
        println!(
            "{exit:?}: input={:?} output={:?} scratch={:?}",
            records[0], records[1], records[2]
        );
        for r in records {
            if r.charged < r.bytes_still_live as u64 || !r.reserve_blocked {
                violations += 1;
            }
        }
    }
    assert_eq!(
        violations, 0,
        "a pool returned capacity before System.dealloc"
    );
}
