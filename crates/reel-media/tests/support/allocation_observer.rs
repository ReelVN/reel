//! Standalone probe only; production crates keep unsafe_code=forbid.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};
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
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count(|c| c.reallocations += 1);
        unsafe { System.realloc(ptr, layout, size) }
    }
}
