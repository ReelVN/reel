use aside_vm::*;

#[test]
fn sparse_historical_ranges_skip_holes_without_allocation_or_losing_live_roots() {
    let mut h = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
    let mut f = FrameSlots::new(
        &mut h,
        FrameSlotsLimits {
            initial_capacity: 4096,
            max_capacity: 4096,
            max_window_len: 1,
        },
    )
    .unwrap();
    let mut ranges = Vec::new();
    for _ in 0..4096 {
        let range = f
            .open(&mut h, RootOwner(1), RootKind::TaskFrame, 1)
            .unwrap();
        f.write(&mut h, at(range, 0), Value::NULL).unwrap();
        ranges.push(range);
    }
    let first = h.alloc_cell(Value::int(11)).unwrap();
    let last = h.alloc_cell(Value::int(29)).unwrap();
    f.write(&mut h, at(ranges[0], 0), first).unwrap();
    f.write(&mut h, at(ranges[4095], 0), last).unwrap();
    for &range in &ranges[1..4095] {
        f.release(&mut h, range).unwrap();
    }
    h.begin_collection().unwrap();
    let (mut holes, mut blocks, mut descriptors, mut values) = (0, 0, 0, 0);
    while h.phase() != GcPhase::Idle {
        let progress = step(&mut h, &f, one());
        holes += progress.range_holes;
        blocks += progress.range_hole_blocks;
        descriptors += progress.range_descriptors;
        values += progress.range_values;
    }
    assert_eq!((holes, descriptors, values), (4095, 2, 2));
    assert_eq!(blocks, 65);
    assert_eq!(h.cell_read(first).unwrap().as_int(), Some(11));
    assert_eq!(h.cell_read(last).unwrap().as_int(), Some(29));
    assert_eq!(h.stats().live_objects, 2);
    println!(
        "range historical_slots=4097 live=2 holes={holes} hole_scan_units={blocks} root_scan_units={}",
        blocks + descriptors + values
    );
    for range in [ranges[0], ranges[4095]] {
        f.release(&mut h, range).unwrap();
    }
    collect(&mut h, &f);
    assert_eq!(h.stats().live_objects, 0);
}

#[test]
fn growing_range_descriptors_preserve_windows_and_incremental_gc() {
    let mut h = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
    let mut f = FrameSlots::new(
        &mut h,
        FrameSlotsLimits {
            initial_capacity: 2_048,
            max_capacity: 2_048,
            max_window_len: 2_048,
        },
    )
    .unwrap();
    let mut windows = Vec::new();
    for i in 0..1_100 {
        let value = h.alloc_cell(Value::int(i)).unwrap();
        let range = f.open(&mut h, RootOwner(1), RootKind::Host, 1).unwrap();
        f.write(&mut h, at(range, 0), value).unwrap();
        windows.push(range);
        if i == 32 {
            h.begin_collection().unwrap();
        }
        if h.phase() != GcPhase::Idle {
            step(&mut h, &f, one());
        }
    }
    if h.phase() != GcPhase::Idle {
        finish(&mut h, &f);
    }
    collect(&mut h, &f);
    for (i, range) in windows.into_iter().enumerate() {
        assert_eq!(
            h.cell_read(f.read(at(range, 0)).unwrap()).unwrap().as_int(),
            Some(i as i64)
        );
        f.release(&mut h, range).unwrap();
    }
    collect(&mut h, &f);
    assert_eq!(h.stats().live_objects, 0);
    assert_eq!(h.stats().root_ranges, 0);
    f.close(&mut h).unwrap();
    drop(f);
    assert_eq!(h.stats().charged_bytes, h.stats().reserved_bytes);
}

fn limits() -> HeapLimits {
    HeapLimits {
        max_objects: 64,
        max_roots: 4,
        max_root_ranges: 16,
        max_bytes: 1 << 20,
        ..HeapLimits::default()
    }
}
fn heap() -> Heap {
    Heap::new(SessionEpoch(7), limits()).unwrap()
}
fn pool(h: &mut Heap) -> FrameSlots {
    FrameSlots::new(
        h,
        FrameSlotsLimits {
            initial_capacity: 64,
            max_capacity: 256,
            max_window_len: 64,
        },
    )
    .unwrap()
}
fn at(range: RootRangeToken, index: u32) -> RangePosition {
    RangePosition { range, index }
}
fn one() -> GcBudget {
    GcBudget {
        units: 1,
        roots: 1,
        edges: 1,
        weak: 1,
        objects: 1,
        bytes: VALUE_CHUNK_BYTES,
    }
}
fn roots_only(units: u32) -> GcBudget {
    GcBudget {
        units,
        roots: units,
        edges: 0,
        weak: 0,
        objects: 0,
        bytes: 0,
    }
}
fn step(h: &mut Heap, f: &FrameSlots, budget: GcBudget) -> GcProgress {
    let mut result = None;
    let a = allocation_counter::measure(|| {
        result = Some(h.gc_step_with_ranges(budget, f.roots()).unwrap());
    });
    assert_eq!(a.count_total, 0, "GC must not allocate");
    let p = result.unwrap();
    assert!(p.units <= budget.units && p.roots <= budget.roots && p.edges <= budget.edges);
    assert!(p.weak <= budget.weak && p.objects <= budget.objects && p.bytes <= budget.bytes);
    assert_eq!(
        p.roots,
        p.single_root_slots + p.range_descriptors + p.range_hole_blocks + p.range_values
    );
    p
}
fn finish(h: &mut Heap, f: &FrameSlots) {
    for _ in 0..10_000 {
        if h.phase() == GcPhase::Idle {
            return;
        }
        step(h, f, one());
    }
    panic!("no bounded progress");
}
fn collect(h: &mut Heap, f: &FrameSlots) {
    h.begin_collection().unwrap();
    finish(h, f);
}
fn single_roots(h: &mut Heap, f: &FrameSlots) {
    let slots = h.root_slots();
    assert_eq!(step(h, f, roots_only(slots)).single_root_slots, slots);
    assert_eq!(step(h, f, roots_only(1)).range_holes, 1); // sentinel
}

#[test]
fn batch_root_scan_obeys_both_quotas_and_resumes_at_the_next_value() {
    let mut h = heap();
    let mut f = pool(&mut h);
    let range = f
        .open(&mut h, RootOwner(1), RootKind::TaskFrame, 8)
        .unwrap();
    let first = h.alloc_cell(Value::int(1)).unwrap();
    let last = h.alloc_cell(Value::int(2)).unwrap();
    f.write(&mut h, at(range, 1), first).unwrap();
    f.write(&mut h, at(range, 7), last).unwrap();
    h.begin_collection().unwrap();
    single_roots(&mut h, &f);

    let p = step(
        &mut h,
        &f,
        GcBudget {
            roots: 3,
            ..roots_only(7)
        },
    );
    assert_eq!((p.range_descriptors, p.range_values, p.units), (1, 2, 3));
    assert!(p.blocked);
    let p = step(
        &mut h,
        &f,
        GcBudget {
            units: 4,
            ..roots_only(7)
        },
    );
    assert_eq!((p.range_descriptors, p.range_values, p.units), (0, 4, 4));
    let p = step(&mut h, &f, roots_only(2));
    assert_eq!((p.range_descriptors, p.range_values, p.units), (0, 2, 2));

    finish(&mut h, &f);
    assert_eq!(h.cell_read(first).unwrap().as_int(), Some(1));
    assert_eq!(h.cell_read(last).unwrap().as_int(), Some(2));
}

#[test]
fn publication_null_initialization_and_generation_reuse_have_real_distinct_work() {
    let mut h = heap();
    let mut f = pool(&mut h);
    let range = f
        .open(&mut h, RootOwner(1), RootKind::TaskFrame, 6)
        .unwrap();
    assert_eq!(
        f.read(at(range, 0)).unwrap_err(),
        HeapError::UninitializedSlot
    );
    f.write(&mut h, at(range, 0), Value::NULL).unwrap();
    assert_eq!(f.read(at(range, 0)).unwrap().tag(), ValueTag::Null);
    assert_eq!(
        f.borrow_initialized(range).unwrap_err(),
        HeapError::UninitializedSlot
    );
    assert_eq!(
        f.borrow_initialized_window(range, 0, 1).unwrap()[0].tag(),
        ValueTag::Null
    );
    assert!(f.borrow_initialized_window(range, 3, 0).unwrap().is_empty());
    assert_eq!(
        f.borrow_initialized_window(range, 0, 2).unwrap_err(),
        HeapError::UninitializedSlot
    );
    assert_eq!(
        f.borrow_initialized_window(range, u32::MAX, 1).unwrap_err(),
        HeapError::OutOfBounds
    );
    let child = h.alloc_cell(Value::int(91)).unwrap();
    h.begin_collection().unwrap();
    single_roots(&mut h, &f);
    assert_eq!(step(&mut h, &f, one()).range_descriptors, 1);
    for _ in 0..5 {
        assert_eq!(step(&mut h, &f, one()).range_values, 1);
    }
    f.release(&mut h, range).unwrap();
    let fresh = f
        .open(&mut h, RootOwner(1), RootKind::Arguments, 2)
        .unwrap();
    assert_ne!(range, fresh);
    assert_eq!(
        f.read(at(range, 0)).unwrap_err(),
        HeapError::InvalidRootRange
    );
    assert_eq!(
        f.borrow_initialized_window(range, 0, 0).unwrap_err(),
        HeapError::InvalidRootRange
    );
    f.write(&mut h, at(fresh, 0), child).unwrap();
    f.write(&mut h, at(fresh, 1), Value::NULL).unwrap();
    // Must restart descriptor + offset zero, not continue the old offset five.
    assert_eq!(step(&mut h, &f, one()).range_descriptors, 1);
    assert_eq!(step(&mut h, &f, one()).range_values, 1);
    assert_eq!(step(&mut h, &f, one()).range_values, 1);
    finish(&mut h, &f);
    assert_eq!(h.get_field(child, 0).unwrap().as_int(), Some(91));
    assert_eq!(
        f.range_owner(&h, fresh).unwrap(),
        (RootOwner(1), RootKind::Arguments)
    );
    f.release(&mut h, fresh).unwrap();
    let empty = f.open(&mut h, RootOwner(2), RootKind::Delivery, 0).unwrap();
    h.begin_collection().unwrap();
    single_roots(&mut h, &f);
    let p = step(&mut h, &f, one());
    assert_eq!((p.range_descriptors, p.range_values, p.units), (1, 0, 1));
    finish(&mut h, &f);
    assert_eq!(h.info(child).unwrap_err(), HeapError::InvalidHandle);
    assert!(f.borrow_initialized(empty).unwrap().is_empty());
}

#[test]
fn scanned_register_overwrite_alone_preserves_an_otherwise_white_object() {
    let mut h = heap();
    let mut f = pool(&mut h);
    let r = f
        .open(&mut h, RootOwner(1), RootKind::TaskFrame, 2)
        .unwrap();
    let child = h.alloc_cell(Value::int(42)).unwrap();
    f.write(&mut h, at(r, 0), Value::NULL).unwrap();
    h.begin_collection().unwrap();
    single_roots(&mut h, &f);
    assert_eq!(step(&mut h, &f, one()).range_descriptors, 1);
    assert_eq!(step(&mut h, &f, one()).range_values, 1);
    f.write(&mut h, at(r, 0), child).unwrap();
    finish(&mut h, &f); // No weak upgrade or other operation can rescue child.
    assert_eq!(
        h.get_field(f.read(at(r, 0)).unwrap(), 0).unwrap().as_int(),
        Some(42)
    );
    f.clear(&mut h, at(r, 0)).unwrap();
    collect(&mut h, &f);
    assert_eq!(h.info(child).unwrap_err(), HeapError::InvalidHandle);
}

#[test]
fn swap_alone_preserves_the_sole_unscanned_root_moved_behind_the_cursor() {
    let mut h = heap();
    let mut f = pool(&mut h);
    let r = f
        .open(&mut h, RootOwner(1), RootKind::TaskFrame, 2)
        .unwrap();
    let child = h.alloc_cell(Value::int(43)).unwrap();
    f.write(&mut h, at(r, 0), Value::NULL).unwrap();
    f.write(&mut h, at(r, 1), child).unwrap();
    h.begin_collection().unwrap();
    single_roots(&mut h, &f);
    step(&mut h, &f, one()); // descriptor
    step(&mut h, &f, one()); // null at zero; child at one is still white
    f.swap(&mut h, at(r, 0), at(r, 1)).unwrap();
    finish(&mut h, &f);
    assert_eq!(
        h.get_field(f.read(at(r, 0)).unwrap(), 0).unwrap().as_int(),
        Some(43)
    );
    assert_eq!(f.read(at(r, 1)).unwrap().tag(), ValueTag::Null);
}

#[test]
fn move_alone_preserves_the_sole_unscanned_root_and_its_initialization_state() {
    let mut h = heap();
    let mut f = pool(&mut h);
    let r = f
        .open(&mut h, RootOwner(1), RootKind::TaskFrame, 2)
        .unwrap();
    let child = h.alloc_cell(Value::int(44)).unwrap();
    f.write(&mut h, at(r, 1), child).unwrap();
    h.begin_collection().unwrap();
    single_roots(&mut h, &f);
    step(&mut h, &f, one());
    step(&mut h, &f, one()); // uninitialized null is also one root Value unit
    f.move_value(&mut h, at(r, 1), at(r, 0)).unwrap();
    assert_eq!(f.read(at(r, 1)).unwrap_err(), HeapError::UninitializedSlot);
    finish(&mut h, &f);
    assert_eq!(
        h.get_field(f.read(at(r, 0)).unwrap(), 0).unwrap().as_int(),
        Some(44)
    );
    f.move_value(&mut h, at(r, 0), at(r, 0)).unwrap();
    collect(&mut h, &f);
    assert_eq!(h.get_field(child, 0).unwrap().as_int(), Some(44));
}

#[test]
fn every_gc_step_interleaves_frame_push_pop_reuse_writes_and_relocation() {
    let mut h = heap();
    let mut f = pool(&mut h);
    let base = f
        .open(&mut h, RootOwner(1), RootKind::TaskFrame, 8)
        .unwrap();
    let native = f
        .open(&mut h, RootOwner(1), RootKind::NativeContinuation, 2)
        .unwrap();
    let delivery = f.open(&mut h, RootOwner(1), RootKind::Delivery, 1).unwrap();
    let mut cells = Vec::new();
    for i in 0..8 {
        let v = h.alloc_cell(Value::int(i as i64)).unwrap();
        f.write(&mut h, at(base, i), v).unwrap();
        cells.push(v);
    }
    let cycle = h.alloc_array(&cells).unwrap();
    h.set_field(cells[0], 0, cycle).unwrap();
    f.copy_value(&mut h, at(base, 1), at(native, 0)).unwrap();
    h.begin_collection().unwrap();
    let mut n = 0;
    while h.phase() != GcPhase::Idle {
        step(&mut h, &f, one());
        let temp = f
            .open(&mut h, RootOwner(2), RootKind::Arguments, 2)
            .unwrap();
        f.write(&mut h, at(temp, 0), cells[n % 8]).unwrap();
        f.copy_value(&mut h, at(temp, 0), at(temp, 1)).unwrap();
        f.move_value(&mut h, at(temp, 1), at(delivery, 0)).unwrap();
        f.swap(&mut h, at(native, 0), at(native, 1)).unwrap();
        f.write(&mut h, at(native, 0), cells[n % 8]).unwrap();
        f.release(&mut h, temp).unwrap();
        assert_eq!(
            f.read(at(temp, 0)).unwrap_err(),
            HeapError::InvalidRootRange
        );
        if n % 7 == 0 {
            let cost = f
                .relocate(&mut h, if n % 2 == 0 { 32 } else { 64 })
                .unwrap();
            assert_eq!(cost.values, 11);
        }
        n += 1;
        assert!(n < 5_000);
    }
    assert!(n > 20);
    for (i, &v) in cells.iter().enumerate().skip(1) {
        assert!(f.read(at(base, i as u32)).unwrap().same_bits(v));
        assert_eq!(h.get_field(v, 0).unwrap().as_int(), Some(i as i64));
    }
    assert!(h.get_field(cells[0], 0).unwrap().same_bits(cycle));
    f.release(&mut h, base).unwrap();
    f.release(&mut h, native).unwrap();
    f.release(&mut h, delivery).unwrap();
    collect(&mut h, &f);
    assert_eq!(h.stats().live_objects, 0);
    assert_eq!(h.stats().pending_bytes, 0);
}

#[test]
fn delivery_bare_reads_keep_roots_and_transfers_or_abandonment_release_them() {
    let mut h = heap();
    let mut f = pool(&mut h);
    let src = f
        .open(&mut h, RootOwner(1), RootKind::TaskFrame, 1)
        .unwrap();
    let dst = f.open(&mut h, RootOwner(1), RootKind::Delivery, 1).unwrap();
    let child = h.alloc_cell(Value::int(55)).unwrap();
    f.write(&mut h, at(src, 0), child).unwrap();
    h.begin_collection().unwrap();
    for _ in 0..7 {
        step(&mut h, &f, one());
    }
    f.move_value(&mut h, at(src, 0), at(dst, 0)).unwrap();
    f.release(&mut h, src).unwrap();
    finish(&mut h, &f);
    let bare = f.borrow_initialized(dst).unwrap()[0];
    collect(&mut h, &f); // Borrow/read did not consume the delivery root.
    assert_eq!(h.get_field(bare, 0).unwrap().as_int(), Some(55));
    let owned = f
        .move_to_host_root(&mut h, at(dst, 0), RootOwner(9))
        .unwrap();
    f.release(&mut h, dst).unwrap();
    collect(&mut h, &f);
    assert!(h.root_value(owned).unwrap().same_bits(bare));
    h.release_root(owned).unwrap();
    collect(&mut h, &f);
    assert_eq!(h.info(bare).unwrap_err(), HeapError::InvalidHandle);

    let abandoned = f.open(&mut h, RootOwner(1), RootKind::Delivery, 1).unwrap();
    let result = h.alloc_cell(Value::int(56)).unwrap();
    f.write(&mut h, at(abandoned, 0), result).unwrap();
    f.release(&mut h, abandoned).unwrap();
    collect(&mut h, &f);
    assert_eq!(h.info(result).unwrap_err(), HeapError::InvalidHandle);
}

#[test]
fn owner_cancellation_walks_single_and_range_slots_between_gc_steps() {
    let mut h = heap();
    let mut f = pool(&mut h);
    let child = h.alloc_cell(Value::int(72)).unwrap();
    let keep = f
        .open(&mut h, RootOwner(1), RootKind::TaskFrame, 1)
        .unwrap();
    f.write(&mut h, at(keep, 0), child).unwrap();
    let mut revoked = Vec::new();
    for kind in [
        RootKind::Arguments,
        RootKind::NativeContinuation,
        RootKind::Exception,
        RootKind::Delivery,
    ] {
        let r = f.open(&mut h, RootOwner(2), kind, 1).unwrap();
        f.write(&mut h, at(r, 0), child).unwrap();
        revoked.push(r);
    }
    let host = h.add_root(RootOwner(2), RootKind::Host, child).unwrap();
    let mut single_cursor = 0;
    let mut ranges_cursor = RangeReleaseCursor::default();
    let mut revoked_count = 0;
    h.begin_collection().unwrap();
    loop {
        h.release_owner_step(RootOwner(2), &mut single_cursor, 1);
        let p = f
            .release_owner_step(&mut h, RootOwner(2), &mut ranges_cursor, 1)
            .unwrap();
        assert!(p.slots <= 1);
        revoked_count += p.ranges;
        step(&mut h, &f, one());
        if p.complete && single_cursor == h.root_slots() {
            break;
        }
    }
    assert_eq!(revoked_count, 4);
    finish(&mut h, &f);
    assert_eq!(h.root_value(host).unwrap_err(), HeapError::InvalidRoot);
    for r in revoked {
        assert_eq!(f.read(at(r, 0)).unwrap_err(), HeapError::InvalidRootRange);
    }
    assert_eq!(h.get_field(child, 0).unwrap().as_int(), Some(72));
    assert_eq!(
        f.release_owner_step(&mut h, RootOwner(3), &mut ranges_cursor, 1)
            .unwrap_err(),
        HeapError::InvalidRangeCursor
    );
    f.release(&mut h, keep).unwrap();
    collect(&mut h, &f);
    assert_eq!(h.info(child).unwrap_err(), HeapError::InvalidHandle);
}

#[test]
fn legacy_entry_rejects_missing_provider_in_all_phases_and_weak_cutoff_is_shared() {
    let mut h = heap();
    let mut f = pool(&mut h);
    let white = h.alloc_cell(Value::int(1)).unwrap();
    let live = h.alloc_cell(Value::int(2)).unwrap();
    let weak = h.downgrade(white).unwrap();
    h.alloc_weakref(white).unwrap();
    let r = f
        .open(&mut h, RootOwner(1), RootKind::TaskFrame, 1)
        .unwrap();
    f.write(&mut h, at(r, 0), live).unwrap();
    for phase in [GcPhase::Idle, GcPhase::Mark, GcPhase::Weak, GcPhase::Sweep] {
        if phase == GcPhase::Mark {
            h.begin_collection().unwrap();
        }
        while h.phase() != phase {
            let budget = GcBudget {
                objects: 0,
                ..one()
            };
            step(&mut h, &f, budget);
        }
        assert_eq!(
            h.gc_step(GcBudget { units: 0, ..one() }).unwrap_err(),
            HeapError::MissingRootProvider
        );
        assert_eq!(h.phase(), phase);
        assert_eq!(
            h.gc_step(one()).unwrap_err(),
            HeapError::MissingRootProvider
        );
        assert_eq!(h.phase(), phase);
        if matches!(phase, GcPhase::Weak | GcPhase::Sweep) {
            assert!(h.upgrade(weak).unwrap().is_none());
            let retained = f.read(at(r, 0)).unwrap();
            assert_eq!(
                f.write(&mut h, at(r, 0), white).unwrap_err(),
                HeapError::UnconfirmedLive
            );
            assert!(f.read(at(r, 0)).unwrap().same_bits(retained));
            let new = h.alloc_cell(Value::int(3)).unwrap();
            f.write(&mut h, at(r, 0), new).unwrap();
            assert_eq!(h.get_field(new, 0).unwrap().as_int(), Some(3));
        }
    }
    finish(&mut h, &f);
    assert_eq!(h.info(white).unwrap_err(), HeapError::InvalidHandle);
}

#[test]
fn unexpected_drop_or_wrong_provider_never_silently_collects_and_close_preserves_errors() {
    let mut h = heap();
    let mut other = heap();
    let mut f = pool(&mut h);
    let r = f
        .open(&mut h, RootOwner(1), RootKind::TaskFrame, 1)
        .unwrap();
    let v = h.alloc_cell(Value::int(81)).unwrap();
    f.write(&mut h, at(r, 0), v).unwrap();
    let before = h.budget_usage().charged_bytes();
    assert_eq!(f.close(&mut other).unwrap_err(), HeapError::WrongHeap);
    assert_eq!(h.budget_usage().charged_bytes(), before);
    assert!(f.read(at(r, 0)).unwrap().same_bits(v));
    assert!(matches!(
        FrameSlots::new(
            &mut h,
            FrameSlotsLimits {
                initial_capacity: 1,
                max_capacity: 1,
                max_window_len: 1
            }
        ),
        Err(HeapError::RootProviderAttached)
    ));
    assert_eq!(
        other.gc_step_with_ranges(one(), f.roots()).unwrap_err(),
        HeapError::WrongHeap
    );
    f.close(&mut h).unwrap();
    f.close(&mut h).unwrap();
    let mut replacement = pool(&mut h);
    let fresh = replacement
        .open(&mut h, RootOwner(2), RootKind::TaskFrame, 1)
        .unwrap();
    assert_eq!(
        replacement.read(at(r, 0)).unwrap_err(),
        HeapError::InvalidRootRange
    );
    assert_eq!(
        h.gc_step_with_ranges(one(), f.roots()).unwrap_err(),
        HeapError::WrongRootProvider
    );
    f.close(&mut h).unwrap(); // Old idempotent close cannot revoke the new provider.
    replacement.write(&mut h, at(fresh, 0), v).unwrap();
    h.begin_collection().unwrap();
    step(&mut h, &replacement, one());
    drop(replacement);
    let usage = h.budget_usage();
    assert_eq!(h.stats().root_ranges, 1); // fail closed; descriptors are not empty roots
    assert_eq!(
        h.gc_step(one()).unwrap_err(),
        HeapError::MissingRootProvider
    );
    assert_eq!(h.phase(), GcPhase::Mark);
    assert!(matches!(
        FrameSlots::new(
            &mut h,
            FrameSlotsLimits {
                initial_capacity: 1,
                max_capacity: 1,
                max_window_len: 1
            }
        ),
        Err(HeapError::MissingRootProvider)
    ));
    assert!(h.info(v).is_ok()); // No destructive sweep ran through the missing provider.
    drop(f);
    assert_eq!(usage.charged_bytes(), h.stats().reserved_bytes);
    drop(h);
    assert!(usage.charged_bytes() > 0); // The observation's Rc book is still physically owned.
}

#[test]
fn empty_pool_drop_allows_reattachment_and_heap_drop_invalidates_pool_access() {
    let mut h = heap();
    let f = pool(&mut h);
    drop(f);
    let mut f = pool(&mut h);
    let r = f
        .open(&mut h, RootOwner(1), RootKind::TaskFrame, 1)
        .unwrap();
    f.write(&mut h, at(r, 0), Value::int(3)).unwrap();
    drop(h);
    assert_eq!(f.read(at(r, 0)).unwrap_err(), HeapError::HeapUnavailable);
}

#[test]
fn dropping_a_live_pool_refuses_collection_and_reattachment_in_every_phase() {
    for phase in [GcPhase::Idle, GcPhase::Mark, GcPhase::Weak, GcPhase::Sweep] {
        let mut h = heap();
        let mut f = pool(&mut h);
        let r = f.open(&mut h, RootOwner(1), RootKind::Delivery, 1).unwrap();
        let v = h.alloc_cell(Value::int(23)).unwrap();
        h.alloc_weakref(v).unwrap();
        f.write(&mut h, at(r, 0), v).unwrap();
        if phase != GcPhase::Idle {
            h.begin_collection().unwrap();
        }
        while h.phase() != phase {
            step(
                &mut h,
                &f,
                GcBudget {
                    objects: 0,
                    ..one()
                },
            );
        }
        drop(f);
        assert_eq!(
            h.gc_step(GcBudget { units: 0, ..one() }).unwrap_err(),
            HeapError::MissingRootProvider
        );
        assert_eq!(
            h.gc_step(one()).unwrap_err(),
            HeapError::MissingRootProvider
        );
        assert_eq!(h.phase(), phase);
        assert!(matches!(
            FrameSlots::new(
                &mut h,
                FrameSlotsLimits {
                    initial_capacity: 1,
                    max_capacity: 1,
                    max_window_len: 1
                }
            ),
            Err(HeapError::MissingRootProvider)
        ));
        assert_eq!(h.get_field(v, 0).unwrap().as_int(), Some(23));
    }
}

#[test]
fn failed_host_delivery_or_foreign_writes_leave_the_source_root_intact() {
    let mut h = Heap::new(
        SessionEpoch(1),
        HeapLimits {
            max_roots: 1,
            ..limits()
        },
    )
    .unwrap();
    let mut f = pool(&mut h);
    let r = f.open(&mut h, RootOwner(1), RootKind::Delivery, 1).unwrap();
    let v = h.alloc_cell(Value::int(88)).unwrap();
    f.write(&mut h, at(r, 0), v).unwrap();
    let host = h
        .add_root(RootOwner(3), RootKind::Host, Value::NULL)
        .unwrap();
    assert_eq!(
        f.move_to_host_root(&mut h, at(r, 0), RootOwner(2))
            .unwrap_err(),
        HeapError::RootCapacity
    );
    let mut foreign = heap();
    let alien = foreign.alloc_cell(Value::int(99)).unwrap();
    assert_eq!(
        f.write(&mut h, at(r, 0), alien).unwrap_err(),
        HeapError::WrongHeap
    );
    assert_eq!(
        f.clear(&mut foreign, at(r, 0)).unwrap_err(),
        HeapError::WrongHeap
    );
    assert!(f.read(at(r, 0)).unwrap().same_bits(v));
    collect(&mut h, &f);
    assert_eq!(h.get_field(v, 0).unwrap().as_int(), Some(88));
    h.release_root(host).unwrap();
    let owned = f.move_to_host_root(&mut h, at(r, 0), RootOwner(2)).unwrap();
    f.close(&mut h).unwrap();
    h.begin_collection().unwrap();
    while h.phase() != GcPhase::Idle {
        h.gc_step(one()).unwrap();
    }
    assert_eq!(
        h.get_field(h.root_value(owned).unwrap(), 0)
            .unwrap()
            .as_int(),
        Some(88)
    );
}

#[test]
fn close_and_reattach_during_a_partial_scan_resets_generation_and_cursor_scopes() {
    let mut h = heap();
    let mut f = pool(&mut h);
    let r = f
        .open(&mut h, RootOwner(1), RootKind::TaskFrame, 6)
        .unwrap();
    let child = h.alloc_cell(Value::int(32)).unwrap();
    h.begin_collection().unwrap();
    single_roots(&mut h, &f);
    step(&mut h, &f, one());
    for _ in 0..5 {
        step(&mut h, &f, one());
    }
    let mut cursor = RangeReleaseCursor::default();
    f.release_owner_step(&mut h, RootOwner(1), &mut cursor, 0)
        .unwrap();
    f.close(&mut h).unwrap();
    let mut fresh_pool = pool(&mut h);
    let fresh = fresh_pool
        .open(&mut h, RootOwner(1), RootKind::Delivery, 1)
        .unwrap();
    assert_eq!(
        fresh_pool.read(at(r, 0)).unwrap_err(),
        HeapError::InvalidRootRange
    );
    assert_eq!(
        fresh_pool
            .release_owner_step(&mut h, RootOwner(1), &mut cursor, 1)
            .unwrap_err(),
        HeapError::WrongRootProvider
    );
    fresh_pool.write(&mut h, at(fresh, 0), child).unwrap();
    assert_eq!(step(&mut h, &fresh_pool, one()).range_descriptors, 1);
    assert_eq!(step(&mut h, &fresh_pool, one()).range_values, 1);
    finish(&mut h, &fresh_pool);
    assert_eq!(h.get_field(child, 0).unwrap().as_int(), Some(32));
    let mut foreign = heap();
    let mut foreign_pool = pool(&mut foreign);
    // Attachments start at 1 in each heap; a numeric attachment alone is insufficient.
    assert_eq!(
        foreign_pool
            .release_owner_step(&mut foreign, RootOwner(1), &mut cursor, 1)
            .unwrap_err(),
        HeapError::WrongHeap
    );
}

#[test]
fn fragmented_capacity_relocation_and_failed_mutations_keep_roots_and_full_pool_charges() {
    let mut h = heap();
    let mut f = FrameSlots::new(
        &mut h,
        FrameSlotsLimits {
            initial_capacity: 8,
            max_capacity: 16,
            max_window_len: 8,
        },
    )
    .unwrap();
    let a = f
        .open(&mut h, RootOwner(1), RootKind::TaskFrame, 3)
        .unwrap();
    let b = f
        .open(&mut h, RootOwner(1), RootKind::TaskFrame, 3)
        .unwrap();
    let v = h.alloc_cell(Value::int(9)).unwrap();
    f.write(&mut h, at(b, 0), v).unwrap();
    let charged = h.stats().charged_bytes;
    f.release(&mut h, a).unwrap();
    assert_eq!(h.stats().charged_bytes, charged); // Pool holes remain physically resident.
    assert_eq!(
        f.open(&mut h, RootOwner(1), RootKind::Arguments, 4)
            .unwrap_err(),
        HeapError::WindowCapacity
    );
    assert_eq!(
        f.relocate(&mut h, 2).unwrap_err(),
        HeapError::WindowCapacity
    );
    assert!(f.read(at(b, 0)).unwrap().same_bits(v));
    let cost = f.relocate(&mut h, 8).unwrap();
    assert_eq!(cost.values, 3);
    assert!(cost.peak_charged_bytes >= charged + cost.new_buffer_bytes);
    let c = f
        .open(&mut h, RootOwner(1), RootKind::Arguments, 4)
        .unwrap();
    assert_eq!(f.read(at(c, 0)).unwrap_err(), HeapError::UninitializedSlot);
    assert_eq!(
        f.copy_value(&mut h, at(b, 0), at(c, 4)).unwrap_err(),
        HeapError::OutOfBounds
    );
    assert!(f.read(at(b, 0)).unwrap().same_bits(v));
    collect(&mut h, &f);
    assert_eq!(h.get_field(v, 0).unwrap().as_int(), Some(9));
    f.close(&mut h).unwrap();
    assert_eq!(f.stats().capacity, 0);
    assert_eq!(h.stats().external_bytes, f.stats().metadata_bytes);
    drop(f);
    assert_eq!(h.stats().external_bytes, 0);
}

#[test]
fn numeric_register_loop_uses_real_write_read_and_initialization_operations_without_allocations() {
    for size in [1, 16, 64] {
        let mut h = heap();
        let mut f = pool(&mut h);
        let r = f
            .open(&mut h, RootOwner(1), RootKind::TaskFrame, size)
            .unwrap();
        h.begin_collection().unwrap();
        let before = h.stats();
        let allocation = allocation_counter::measure(|| {
            for n in 0..10_000 {
                let index = std::hint::black_box(n % size);
                f.write(&mut h, at(r, index), Value::int(n as i64)).unwrap();
                assert_eq!(f.read(at(r, index)).unwrap().as_int(), Some(n as i64));
                f.clear(&mut h, at(r, index)).unwrap();
                assert_eq!(
                    f.read(at(r, index)).unwrap_err(),
                    HeapError::UninitializedSlot
                );
            }
        });
        assert_eq!(allocation.count_total, 0);
        assert_eq!(h.stats().live_objects, before.live_objects);
        assert_eq!(h.stats().roots, before.roots);
        assert_eq!(h.stats().root_ranges, 1);
        assert_eq!(h.stats().charged_bytes, before.charged_bytes);
        finish(&mut h, &f);
    }
}
