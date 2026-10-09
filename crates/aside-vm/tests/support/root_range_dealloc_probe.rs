//! Standalone observer only: System forwarding, no unsafe in the storage library.
#![deny(unsafe_op_in_unsafe_fn)]
use aside_vm::*;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::{Cell, RefCell},
};

struct Probe;
#[global_allocator]
static ALLOCATOR: Probe = Probe;
#[derive(Clone, Copy, Default)]
struct Watch {
    addr: usize,
    bytes: u64,
    freed: bool,
}
#[derive(Clone, Copy, Debug, Default)]
struct Record {
    bytes: u64,
    live: u64,
    charged: u64,
    remaining: u64,
}
struct State {
    usage: Option<HeapBudgetUsage>,
    base: u64,
    capture: bool,
    watches: [Watch; 16],
    count: usize,
    records: [Record; 16],
    freed: usize,
    violation: bool,
}
thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    static IN_HOOK: Cell<bool> = const { Cell::new(false) };
}
fn observe(f: impl FnOnce(&mut State)) {
    let _ = IN_HOOK.try_with(|busy| {
        if busy.replace(true) {
            return;
        }
        let _ = STATE.try_with(|slot| {
            if let Ok(mut state) = slot.try_borrow_mut() {
                if let Some(state) = state.as_mut() {
                    f(state);
                }
            }
        });
        busy.set(false);
    });
}
// SAFETY: Forward every caller-provided pointer/Layout exactly once; observation
// uses integer addresses only and never dereferences, formats, or panics in hooks.
unsafe impl GlobalAlloc for Probe {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            observe(|s| {
                if s.capture {
                    if s.count == s.watches.len() {
                        s.violation = true;
                        return;
                    }
                    s.watches[s.count] = Watch {
                        addr: ptr as usize,
                        bytes: layout.size() as u64,
                        freed: false,
                    };
                    s.count += 1;
                }
            });
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        observe(|s| {
            if let Some(i) = s.watches[..s.count]
                .iter()
                .position(|w| w.addr == ptr as usize && !w.freed)
            {
                let live: u64 = s.watches[..s.count]
                    .iter()
                    .filter(|w| !w.freed)
                    .map(|w| w.bytes)
                    .sum();
                let Some(usage) = s.usage.as_ref() else {
                    s.violation = true;
                    return;
                };
                let charged = usage.charged_bytes();
                if charged < s.base + live || s.freed == s.records.len() {
                    s.violation = true;
                    return;
                }
                s.records[s.freed] = Record {
                    bytes: layout.size() as u64,
                    live,
                    charged,
                    remaining: usage.remaining_bytes(),
                };
                s.freed += 1;
                s.watches[i].freed = true;
            }
        });
        unsafe { System.dealloc(ptr, layout) };
    }
}
fn limits(bytes: u64) -> HeapLimits {
    HeapLimits {
        max_objects: 8,
        max_roots: 2,
        max_root_ranges: 8,
        max_bytes: bytes,
        ..HeapLimits::default()
    }
}
fn policy(capacity: u32) -> FrameSlotsLimits {
    FrameSlotsLimits {
        initial_capacity: capacity,
        max_capacity: 256,
        max_window_len: 64,
    }
}
fn install(h: &Heap) {
    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            usage: Some(h.budget_usage()),
            base: h.stats().reserved_bytes,
            capture: true,
            watches: [Watch::default(); 16],
            records: [Record::default(); 16],
            count: 0,
            freed: 0,
            violation: false,
        })
    });
}
fn capture(set: bool) {
    STATE.with(|s| s.borrow_mut().as_mut().unwrap().capture = set);
}
fn records() -> usize {
    STATE.with(|s| s.borrow().as_ref().unwrap().freed)
}
fn costs() -> (u64, u64) {
    let mut h = Heap::new(SessionEpoch(1), limits(1 << 20)).unwrap();
    let f = FrameSlots::new(&mut h, policy(64)).unwrap();
    (h.stats().reserved_bytes, f.stats().charged_bytes)
}
fn report(label: &str) {
    let state = STATE.with(|s| s.borrow_mut().take().unwrap());
    assert!(
        !state.violation,
        "budget refunded while allocator still owns bytes"
    );
    assert_eq!(
        state.count, state.freed,
        "all captured physical buffers released"
    );
    assert!(state.freed > 0);
    println!("{label}: observed {} real deallocations", state.freed);
    for record in &state.records[..state.freed] {
        assert!(record.charged >= state.base + record.live);
        println!("  {record:?}");
        assert!(record.bytes > 0);
    }
}

#[test]
fn relocation_overlap_reuse_shrink_and_close_keep_charges_until_system_free() {
    let (base, external) = costs();
    let mut h = Heap::new(SessionEpoch(1), limits(base + external + 128 * 16 + 16)).unwrap();
    install(&h);
    let mut f = FrameSlots::new(&mut h, policy(64)).unwrap();
    capture(false);
    let r = f
        .open(&mut h, RootOwner(1), RootKind::TaskFrame, 16)
        .unwrap();
    for i in 0..16 {
        f.write(
            &mut h,
            RangePosition { range: r, index: i },
            Value::int(i as i64),
        )
        .unwrap();
    }
    let before = h.stats().charged_bytes;
    let mut cursor = RangeReleaseCursor::default();
    while !f
        .release_owner_step(&mut h, RootOwner(1), &mut cursor, 1)
        .unwrap()
        .complete
    {}
    assert_eq!(records(), 0);
    assert_eq!(h.stats().charged_bytes, before);
    let r = f
        .open(&mut h, RootOwner(1), RootKind::Delivery, 16)
        .unwrap();
    f.write(&mut h, RangePosition { range: r, index: 0 }, Value::int(42))
        .unwrap();
    capture(true);
    f.relocate(&mut h, 128).unwrap();
    capture(false);
    STATE.with(|s| assert_eq!(s.borrow().as_ref().unwrap().records[0].remaining, 0));
    let count = records();
    assert!(matches!(
        f.relocate(&mut h, 256),
        Err(HeapError::ResourceLimit { .. })
    ));
    assert_eq!(records(), count);
    assert_eq!(
        f.read(RangePosition { range: r, index: 0 })
            .unwrap()
            .as_int(),
        Some(42)
    );
    capture(true);
    f.relocate(&mut h, 64).unwrap();
    capture(false);
    let mut wrong = Heap::new(SessionEpoch(1), limits(1 << 20)).unwrap();
    let before = records();
    assert_eq!(f.close(&mut wrong).unwrap_err(), HeapError::WrongHeap);
    assert_eq!(records(), before);
    f.close(&mut h).unwrap();
    drop(f);
    assert_eq!(h.stats().external_bytes, 0);
    report("relocation/reuse/shrink/close");
}

#[test]
fn failed_bitmap_reservation_releases_scratch_after_its_real_deallocation() {
    let (base, external) = costs();
    let mut h = Heap::new(SessionEpoch(1), limits(base + external + 64 * 16)).unwrap();
    install(&h);
    let mut f = FrameSlots::new(&mut h, policy(64)).unwrap();
    let r = f
        .open(&mut h, RootOwner(1), RootKind::TaskFrame, 1)
        .unwrap();
    f.write(&mut h, RangePosition { range: r, index: 0 }, Value::int(17))
        .unwrap();
    let before = h.stats().charged_bytes;
    assert!(matches!(
        f.relocate(&mut h, 64),
        Err(HeapError::ResourceLimit { requested: 8, .. })
    ));
    capture(false);
    assert_eq!(h.stats().charged_bytes, before);
    assert_eq!(f.stats().capacity, 64);
    assert_eq!(
        f.read(RangePosition { range: r, index: 0 })
            .unwrap()
            .as_int(),
        Some(17)
    );
    STATE.with(|s| assert_eq!(s.borrow().as_ref().unwrap().records[0].remaining, 0));
    f.close(&mut h).unwrap();
    drop(f);
    assert_eq!(h.stats().external_bytes, 0);
    report("failed relocation scratch");
}

#[test]
fn rejected_constructor_retains_no_attachment_and_refunds_after_deallocation() {
    let (base, external) = costs();
    let mut h = Heap::new(SessionEpoch(1), limits(base + external - 1)).unwrap();
    install(&h);
    assert!(matches!(
        FrameSlots::new(&mut h, policy(64)),
        Err(HeapError::ResourceLimit { .. })
    ));
    capture(false);
    assert_eq!(h.stats().charged_bytes, base);
    capture(true);
    let mut f = FrameSlots::new(&mut h, policy(32)).unwrap();
    capture(false);
    f.close(&mut h).unwrap();
    drop(f);
    assert_eq!(h.stats().charged_bytes, base);
    report("constructor rollback and reattach");
}

#[test]
fn unexpected_pool_drop_releases_bytes_but_keeps_fail_closed_descriptors() {
    let mut h = Heap::new(SessionEpoch(1), limits(1 << 20)).unwrap();
    install(&h);
    let mut f = FrameSlots::new(&mut h, policy(64)).unwrap();
    capture(false);
    let r = f
        .open(&mut h, RootOwner(1), RootKind::TaskFrame, 1)
        .unwrap();
    f.write(&mut h, RangePosition { range: r, index: 0 }, Value::int(17))
        .unwrap();
    drop(f);
    assert_eq!(h.stats().external_bytes, 0);
    assert_eq!(h.stats().root_ranges, 1);
    assert_eq!(
        h.gc_step(GcBudget::default()).unwrap_err(),
        HeapError::MissingRootProvider
    );
    report("unexpected pool drop");
}

#[test]
fn heap_teardown_keeps_its_buffers_charged_and_invalidates_the_remaining_pool() {
    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            usage: None,
            base: 0,
            capture: true,
            watches: [Watch::default(); 16],
            count: 0,
            records: [Record::default(); 16],
            freed: 0,
            violation: false,
        })
    });
    let mut h = Heap::new(SessionEpoch(1), limits(1 << 20)).unwrap();
    STATE.with(|s| s.borrow_mut().as_mut().unwrap().usage = Some(h.budget_usage()));
    let mut f = FrameSlots::new(&mut h, policy(64)).unwrap();
    capture(false);
    let r = f
        .open(&mut h, RootOwner(1), RootKind::TaskFrame, 1)
        .unwrap();
    f.write(&mut h, RangePosition { range: r, index: 0 }, Value::int(7))
        .unwrap();
    drop(h);
    assert_eq!(
        f.read(RangePosition { range: r, index: 0 }).unwrap_err(),
        HeapError::HeapUnavailable
    );
    drop(f);
    let state = STATE.with(|s| s.borrow_mut().take().unwrap());
    assert!(!state.violation);
    assert_eq!(state.count, state.freed + 1); // The observer still owns the Rc byte book.
    assert!(!state.watches[0].freed);
    assert_eq!(
        state.usage.as_ref().unwrap().charged_bytes(),
        state.watches[0].bytes
    );
    println!(
        "heap teardown: {} physical deallocations, only {}-byte observer book remains",
        state.freed, state.watches[0].bytes
    );
    for record in &state.records[..state.freed] {
        assert!(record.charged >= record.live);
        println!("  {record:?}");
    }
}
