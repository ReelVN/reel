//! Standalone probe only; production crates keep unsafe_code=forbid.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Instant,
};
pub(crate) static BACKGROUND_ENABLED: AtomicBool = AtomicBool::new(false);
pub(crate) static TARGET_LAYOUT: AtomicUsize = AtomicUsize::new(0);
pub(crate) static WORKER_PCM_FREES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static WRONG_THREAD_PCM_FREES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static WORKER_PCM_FREE_NS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static WORKER_LARGEST_FREE: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PHYSICAL_FREE_ORDER_CHECKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static WRONG_PHYSICAL_FREE_ORDER: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXPECTED_CHARGE: AtomicUsize = AtomicUsize::new(0);
pub(crate) static OBSERVED_POOL: Mutex<Option<reel_core::budget::BudgetPool>> = Mutex::new(None);
thread_local! { static BACKGROUND_REENTRY: Cell<bool> = const { Cell::new(false) }; }
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct Counts {
    pub allocations: usize,
    pub deallocations: usize,
    pub reallocations: usize,
    pub freed_bytes: usize,
    pub largest_free: usize,
}
thread_local! { pub(crate) static ENABLED: Cell<bool> = const { Cell::new(false) }; pub(crate) static COUNTS: Cell<Counts> = const { Cell::new(Counts { allocations: 0, deallocations: 0, reallocations: 0, freed_bytes: 0, largest_free: 0 }) }; }
fn count(update: impl FnOnce(&mut Counts)) {
    if ENABLED.try_with(Cell::get).unwrap_or(false) {
        let _ = COUNTS.try_with(|cell| {
            let mut counts = cell.get();
            update(&mut counts);
            cell.set(counts);
        });
    }
}
fn check_charge() {
    let held = OBSERVED_POOL.lock().is_ok_and(|pool| {
        pool.as_ref().is_some_and(|pool| {
            let usage = pool.usage();
            usage.tickets == 1 && usage.bytes == EXPECTED_CHARGE.load(Ordering::Acquire) as u64
        })
    });
    PHYSICAL_FREE_ORDER_CHECKS.fetch_add(1, Ordering::AcqRel);
    if !held {
        WRONG_PHYSICAL_FREE_ORDER.fetch_add(1, Ordering::Release);
    }
}
struct Observer;
#[global_allocator]
static ALLOCATOR: Observer = Observer;
// SAFETY: Forward every original valid pointer/Layout to System exactly once.
unsafe impl GlobalAlloc for Observer {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count(|c| c.allocations += 1);
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count(|c| c.allocations += 1);
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        count(|c| {
            c.deallocations += 1;
            c.freed_bytes += layout.size();
            c.largest_free = c.largest_free.max(layout.size());
        });
        let track = BACKGROUND_ENABLED.load(Ordering::Acquire) && !BACKGROUND_REENTRY.replace(true);
        let worker = track && std::thread::current().name() == Some("reel-audio-reclaim");
        let pcm = track && layout.size() == TARGET_LAYOUT.load(Ordering::Acquire);
        if pcm {
            check_charge();
        }
        let begin = (worker && pcm).then(Instant::now);
        unsafe { System.dealloc(ptr, layout) }
        let elapsed = begin.map(|begin| begin.elapsed());
        if pcm {
            check_charge();
        }
        if worker {
            WORKER_LARGEST_FREE.fetch_max(layout.size(), Ordering::AcqRel);
        }
        if pcm {
            if worker {
                let ns = elapsed.unwrap().as_nanos().min(usize::MAX as u128) as usize;
                WORKER_PCM_FREE_NS.fetch_max(ns, Ordering::AcqRel);
                WORKER_PCM_FREES.fetch_add(1, Ordering::Release);
            } else {
                WRONG_THREAD_PCM_FREES.fetch_add(1, Ordering::Release);
            }
        }
        if track {
            BACKGROUND_REENTRY.set(false);
        }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count(|c| c.reallocations += 1);
        unsafe { System.realloc(ptr, layout, size) }
    }
}
