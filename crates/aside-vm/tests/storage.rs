use aside_vm::*;

fn heap(objects: u32) -> Heap {
    Heap::new(
        SessionEpoch(7),
        HeapLimits {
            max_objects: objects,
            max_roots: 32,
            max_bytes: 64 * 1024 * 1024,
            ..HeapLimits::default()
        },
    )
    .unwrap()
}
fn root(h: &mut Heap, value: Value) -> RootToken {
    h.add_root(RootOwner(1), RootKind::Host, value).unwrap()
}
fn tiny() -> GcBudget {
    GcBudget {
        roots: 1,
        edges: 1,
        weak: 1,
        objects: 1,
        release_edges: 1,
        bytes: VALUE_CHUNK_BYTES,
        units: 1,
    }
}
fn finish(h: &mut Heap, budget: GcBudget) {
    for _ in 0..1_000_000 {
        if h.phase() == GcPhase::Idle {
            return;
        }
        let p = h.gc_step(budget);
        assert!(p.units <= budget.units && p.roots <= budget.roots && p.edges <= budget.edges);
        assert!(
            p.weak <= budget.weak
                && p.objects <= budget.objects
                && p.release_edges <= budget.release_edges
                && p.bytes <= budget.bytes
        );
    }
    panic!("collection failed to make bounded progress");
}
fn collect(h: &mut Heap) {
    h.begin_collection().unwrap();
    finish(h, GcBudget::default());
}
fn reach_phase(h: &mut Heap, phase: GcPhase) {
    while h.phase() != phase {
        assert_ne!(h.phase(), GcPhase::Idle);
        let mut budget = tiny();
        if phase == GcPhase::Sweep {
            budget.objects = 0;
        }
        h.gc_step(budget);
    }
}

#[test]
fn scalar_boundaries_nan_negative_zero_and_copy_are_lossless() {
    assert_eq!(std::mem::size_of::<Value>(), 16);
    assert_eq!(std::mem::align_of::<Value>(), std::mem::align_of::<u64>());
    for n in [
        i64::MIN,
        i64::MIN + 1,
        -(1 << 53) - 1,
        -1,
        0,
        1,
        (1 << 53) + 1,
        i64::MAX,
    ] {
        let v = Value::int(n);
        assert_eq!(v.as_int(), Some(n));
        assert!(v.as_float().is_none());
        assert!(v.same_bits(v));
    }
    for bits in [
        0,
        1 << 63,
        1,
        0x0010_0000_0000_0000,
        f64::MAX.to_bits(),
        f64::INFINITY.to_bits(),
        f64::NEG_INFINITY.to_bits(),
        0x7ff8_0000_0000_0001,
        0xfff0_0000_0000_0001,
        0x7fff_ffff_ffff_ffff,
    ] {
        let value = Value::float(f64::from_bits(bits));
        assert_eq!(value.as_float().unwrap().to_bits(), bits);
    }
    assert!(Value::NULL.as_bool().is_none());
    assert_eq!(Value::bool(false).as_bool(), Some(false));
    assert_eq!(Value::bool(true).as_bool(), Some(true));
    let mut h = heap(8);
    let a = h.alloc_array(&[]).unwrap();
    let info = allocation_counter::measure(|| {
        let mut copied = a;
        for _ in 0..100_000 {
            copied = std::hint::black_box(copied);
        }
        assert!(copied.same_bits(a));
    });
    assert_eq!(info.count_total, 0);
    collect(&mut h); // Copies/handles are not implicit roots.
    assert_eq!(h.info(a).unwrap_err(), HeapError::InvalidHandle);
}

#[test]
fn full_handle_round_trip_and_foreign_or_restored_sessions_are_rejected() {
    let mut a = heap(4);
    let mut b = heap(4);
    let v = a.alloc_array(&[]).unwrap();
    let handle = a.handle(v).unwrap();
    assert!(a.import_handle(handle).unwrap().same_bits(v));
    assert_eq!(b.import_handle(handle).unwrap_err(), HeapError::WrongHeap);
    assert_eq!(b.alloc_cell(v).unwrap_err(), HeapError::WrongHeap);
    assert_eq!(
        b.add_root(RootOwner(1), RootKind::Host, v).unwrap_err(),
        HeapError::WrongHeap
    );
    let mut wrong_epoch = handle;
    wrong_epoch.epoch = SessionEpoch(8);
    assert_eq!(
        a.import_handle(wrong_epoch).unwrap_err(),
        HeapError::WrongEpoch
    );
    let mut even = handle;
    even.key &= !(1 << 32);
    assert_eq!(a.import_handle(even).unwrap_err(), HeapError::InvalidHandle);
    let restored = Heap::new(
        SessionEpoch(8),
        HeapLimits {
            max_objects: 4,
            ..HeapLimits::default()
        },
    )
    .unwrap();
    assert_eq!(
        restored.import_handle(handle).unwrap_err(),
        HeapError::WrongHeap
    );
    let r = root(&mut a, v);
    assert_eq!(b.root_value(r).unwrap_err(), HeapError::WrongHeap);
}

#[test]
fn shared_cells_tuple_aliases_and_cycles_are_real_storage() {
    let mut h = heap(32);
    let cell = h.alloc_cell(Value::int(i64::MAX)).unwrap();
    let left = h.alloc_record(ObjectKind::Closure, &[cell]).unwrap();
    let right = h.alloc_record(ObjectKind::Closure, &[cell]).unwrap();
    let pair = h.alloc_tuple(&[left, right, cell]).unwrap();
    let token = root(&mut h, pair);
    h.set_field(h.get_field(left, 0).unwrap(), 0, Value::int(i64::MIN))
        .unwrap();
    assert_eq!(
        h.get_field(h.get_field(right, 0).unwrap(), 0)
            .unwrap()
            .as_int(),
        Some(i64::MIN)
    );
    assert_eq!(
        h.set_field(pair, 0, Value::NULL).unwrap_err(),
        HeapError::WrongKind
    );
    assert_eq!(h.downgrade(pair).unwrap_err(), HeapError::WrongKind);
    let array = h.alloc_array(&[pair]).unwrap();
    h.set_field(cell, 0, array).unwrap();
    collect(&mut h);
    assert_eq!(h.stats().live_objects, 5);
    h.release_root(token).unwrap();
    collect(&mut h);
    assert_eq!(h.stats().live_objects, 0);
    assert_eq!(h.stats().pending_bytes, 0);
}

#[test]
fn already_scanned_root_updates_and_late_root_publication_use_the_barrier() {
    let mut h = heap(32);
    let old = h.alloc_cell(Value::int(1)).unwrap();
    let replacement = h.alloc_cell(Value::int(2)).unwrap();
    let late = h.alloc_cell(Value::int(3)).unwrap();
    let token = root(&mut h, old);
    h.begin_collection().unwrap();
    // Includes sentinel plus first registered root; subsequent change is behind cursor.
    h.gc_step(GcBudget {
        roots: 2,
        units: 2,
        ..tiny()
    });
    h.set_root(token, replacement).unwrap();
    let late_token = root(&mut h, late);
    h.release_root(late_token).unwrap();
    finish(&mut h, tiny());
    assert!(h.info(replacement).is_ok() && h.info(late).is_ok());
    assert!(h.root_value(token).unwrap().same_bits(replacement));
    collect(&mut h); // Conservative floating garbage disappears in the next cycle.
    assert_eq!(h.stats().live_objects, 1);
}

#[test]
fn container_reordering_and_root_changes_between_every_single_step_preserve_edges() {
    let mut h = heap(512);
    let array = h.alloc_array(&[]).unwrap();
    let array_root = root(&mut h, array);
    let mut cells = Vec::new();
    for n in 0..200 {
        let cell = h.alloc_cell(Value::int(n)).unwrap();
        h.array_push(array, cell).unwrap();
        cells.push(cell);
    }
    let changing = root(&mut h, Value::NULL);
    h.begin_collection().unwrap();
    let mut saw_partial = false;
    let mut n = 0;
    while h.phase() != GcPhase::Idle {
        let p = h.gc_step(tiny());
        if p.edges > 0 {
            saw_partial = true;
        }
        // Target references are all strongly reachable, including during cutoff phases.
        h.array_swap(array, 0, 199).unwrap();
        h.set_root(changing, cells[n % cells.len()]).unwrap();
        let popped = h.array_pop(array).unwrap().unwrap();
        h.array_push(array, popped).unwrap();
        n += 1;
        assert!(n < 10_000);
    }
    assert!(saw_partial);
    for (n, cell) in cells.iter().copied().enumerate() {
        assert_eq!(h.get_field(cell, 0).unwrap().as_int(), Some(n as i64));
    }
    assert_eq!(h.stats().live_objects, 201);
    h.release_root(array_root).unwrap();
    h.release_root(changing).unwrap();
    collect(&mut h);
    assert_eq!(h.stats().live_objects, 0);
}

#[test]
fn initial_edges_insertions_and_weak_upgrades_during_marking_are_shaded() {
    let mut h = heap(32);
    let a = h.alloc_array(&[Value::NULL]).unwrap();
    let _r = root(&mut h, a);
    let child = h.alloc_cell(Value::int(11)).unwrap();
    let weak_child = h.alloc_cell(Value::int(12)).unwrap();
    let weak = h.downgrade(weak_child).unwrap();
    h.begin_collection().unwrap();
    while h.gc_step(tiny()).edges == 0 && h.phase() == GcPhase::Mark {}
    // Empty a may have finished; stay in marking by using another new object.
    if h.phase() == GcPhase::Mark {
        let fresh = h.alloc_array(&[child]).unwrap();
        h.array_push(a, fresh).unwrap();
        let upgraded = h.upgrade(weak).unwrap().unwrap();
        h.array_push(a, upgraded).unwrap();
    } else {
        panic!("test must mutate before cutoff");
    }
    finish(&mut h, tiny());
    assert!(h.info(child).is_ok() && h.info(weak_child).is_ok());
}

#[test]
fn unified_weak_cutoff_refuses_white_targets_through_the_last_sweep_step() {
    let mut h = heap(32);
    let white = h.alloc_array(&[]).unwrap();
    let white_weak = h.downgrade(white).unwrap();
    let live = h.alloc_array(&[]).unwrap();
    let live_weak = h.downgrade(live).unwrap();
    let _live_root = root(&mut h, live);
    let reference = h.alloc_weakref(white).unwrap();
    let _ref_root = root(&mut h, reference);
    h.begin_collection().unwrap();
    reach_phase(&mut h, GcPhase::Weak);
    // white still has a generation-valid slot, but the shared cutoff already applies.
    assert!(h.upgrade(white_weak).unwrap().is_none());
    assert!(h.upgrade_weakref(reference).unwrap().is_none());
    assert_eq!(h.alloc_cell(white).unwrap_err(), HeapError::UnconfirmedLive);
    assert_eq!(
        h.add_root(RootOwner(1), RootKind::Host, white).unwrap_err(),
        HeapError::UnconfirmedLive
    );
    assert_eq!(
        h.array_push(live, white).unwrap_err(),
        HeapError::UnconfirmedLive
    );
    let after_cutoff = h.alloc_array(&[live]).unwrap();
    let new_root = root(&mut h, after_cutoff);
    let new_weak = h.downgrade(after_cutoff).unwrap();
    while h.phase() != GcPhase::Idle {
        assert!(h.upgrade(white_weak).unwrap().is_none());
        assert!(h.upgrade(live_weak).unwrap().is_some());
        assert!(h.upgrade(new_weak).unwrap().is_some());
        h.gc_step(tiny());
    }
    assert!(h.upgrade(white_weak).unwrap().is_none());
    assert!(h.upgrade_weakref(reference).unwrap().is_none());
    assert!(h.info(after_cutoff).is_ok());
    h.release_root(new_root).unwrap();
    collect(&mut h);
    assert!(h.upgrade(new_weak).unwrap().is_none());
}

#[test]
fn weak_stage_new_allocations_do_not_displace_old_sweep_records() {
    let mut h = heap(32);
    let mut old = Vec::new();
    for _ in 0..5 {
        old.push(h.alloc_array(&[]).unwrap());
    }
    h.begin_collection().unwrap();
    reach_phase(&mut h, GcPhase::Weak);
    let mut fresh = Vec::new();
    while h.phase() != GcPhase::Idle {
        if h.phase() == GcPhase::Weak || h.phase() == GcPhase::Sweep {
            fresh.push(h.alloc_array(&[]).unwrap());
        }
        h.gc_step(tiny());
    }
    for value in old {
        assert_eq!(h.info(value).unwrap_err(), HeapError::InvalidHandle);
    }
    for value in fresh {
        assert!(h.info(value).is_ok());
    }
    collect(&mut h);
    assert_eq!(h.stats().live_objects, 0);
}

#[test]
fn large_container_and_utf8_buffer_release_obey_independent_edge_byte_budgets() {
    let mut h = heap(8);
    let array = h.alloc_array(&[]).unwrap();
    for n in 0..4096 {
        h.array_push(array, Value::int(n)).unwrap();
    }
    let text = h.alloc_text_buffer("序\0").unwrap();
    let line = "e\u{301}👩‍👩‍👧‍👦对白";
    for _ in 0..4096 {
        h.text_append(text, line).unwrap();
    }
    let mut prefix = [0; 64];
    let read = h.read_bytes(text, 0, &mut prefix).unwrap();
    assert_eq!(&prefix[..4], "序\0".as_bytes());
    assert_eq!(read, 64);
    h.begin_collection().unwrap();
    reach_phase(&mut h, GcPhase::Sweep);
    let before = h.stats().payload_bytes;
    let p = h.gc_step(GcBudget {
        objects: 1,
        release_edges: 0,
        bytes: 0,
        units: 1,
        ..tiny()
    });
    assert_eq!(p.invalidated_objects, 1);
    assert_eq!(h.info(array).unwrap_err(), HeapError::InvalidHandle);
    assert_eq!(h.stats().pending_bytes, 64 * VALUE_CHUNK_BYTES);
    assert_eq!(h.stats().payload_bytes, before);
    let too_small = GcBudget {
        objects: 0,
        bytes: VALUE_CHUNK_BYTES - 1,
        release_edges: 64,
        units: 128,
        ..tiny()
    };
    let p = h.gc_step(too_small);
    assert_eq!(p.release_edges, 64);
    assert_eq!(p.bytes, 0);
    assert!(p.blocked);
    let p = h.gc_step(GcBudget {
        objects: 0,
        release_edges: 0,
        bytes: VALUE_CHUNK_BYTES,
        units: 1,
        ..tiny()
    });
    assert_eq!(p.bytes, VALUE_CHUNK_BYTES);
    finish(&mut h, tiny());
    assert_eq!(h.stats().pending_bytes, 0);
    assert_eq!(h.stats().payload_bytes, 0);
    assert_eq!(h.stats().charged_bytes, h.stats().reserved_bytes);
}

#[test]
fn deep_chains_and_unrooted_cycles_release_without_recursion_or_gc_allocation() {
    let mut h = heap(12_000);
    let mut chain = Value::NULL;
    for _ in 0..10_000 {
        chain = h.alloc_cell(chain).unwrap();
    }
    let a = h.alloc_array(&[]).unwrap();
    let b = h.alloc_array(&[a]).unwrap();
    h.array_push(a, b).unwrap();
    let r = root(&mut h, chain);
    collect(&mut h);
    assert_eq!(h.stats().live_objects, 10_000);
    h.release_root(r).unwrap();
    let info = allocation_counter::measure(|| {
        collect(&mut h);
    });
    assert_eq!(
        info.count_total, 0,
        "reserved collector storage must not grow"
    );
    assert_eq!(h.stats().live_objects, 0);
    assert_eq!(h.stats().allocation_queue_len, 0);
}

#[test]
fn bounded_owner_release_invalidates_tokens_without_host_or_script_callbacks() {
    let mut h = heap(16);
    let a = h.alloc_cell(Value::int(1)).unwrap();
    let one = h
        .add_root(RootOwner(1), RootKind::NativeContinuation, a)
        .unwrap();
    let other = h.add_root(RootOwner(2), RootKind::Host, a).unwrap();
    assert_eq!(
        h.root_owner(one).unwrap(),
        (RootOwner(1), RootKind::NativeContinuation)
    );
    let mut cursor = 0;
    let mut count = 0;
    while cursor < h.root_slots() {
        count += h.release_owner_step(RootOwner(1), &mut cursor, 1);
    }
    assert_eq!(count, 1);
    assert_eq!(h.root_value(one).unwrap_err(), HeapError::InvalidRoot);
    collect(&mut h);
    assert!(h.info(a).is_ok());
    h.release_root(other).unwrap();
    collect(&mut h);
    assert_eq!(h.stats().live_objects, 0);
}

#[test]
fn budgets_include_reserved_queues_pending_payload_and_backpressure() {
    assert!(matches!(
        Heap::new(
            SessionEpoch(1),
            HeapLimits {
                max_bytes: 1,
                ..HeapLimits::default()
            }
        ),
        Err(HeapError::ResourceLimit { .. })
    ));
    let mut probe = heap(2);
    let reserved = probe.stats().reserved_bytes;
    let limits = HeapLimits {
        max_objects: 2,
        max_roots: 32,
        max_bytes: reserved + VALUE_CHUNK_BYTES,
        ..HeapLimits::default()
    };
    let mut h = Heap::new(SessionEpoch(1), limits).unwrap();
    h.alloc_array(&[Value::int(1)]).unwrap();
    assert_eq!(h.stats().payload_bytes, VALUE_CHUNK_BYTES);
    assert!(matches!(
        h.alloc_array(&[Value::NULL]),
        Err(HeapError::ResourceLimit { .. })
    ));
    assert_eq!(h.stats().live_objects, 1);
    h.begin_collection().unwrap();
    reach_phase(&mut h, GcPhase::Sweep);
    h.gc_step(GcBudget {
        units: 1,
        bytes: 0,
        release_edges: 0,
        ..GcBudget::default()
    });
    assert!(h.stats().pending_bytes > 0);
    assert!(matches!(
        h.alloc_array(&[Value::NULL]),
        Err(HeapError::ResourceLimit { .. })
    ));
    finish(&mut h, GcBudget::default());
    assert!(h.alloc_array(&[Value::NULL]).is_ok());
    let zero = GcBudget {
        roots: 0,
        edges: 0,
        weak: 0,
        objects: 0,
        release_edges: 0,
        bytes: 0,
        units: 0,
    };
    probe.begin_collection().unwrap();
    let p = probe.gc_step(zero);
    assert_eq!(p.units, 0);
}

#[test]
fn input_caps_and_failed_mutations_leave_content_unchanged() {
    let mut h = Heap::new(
        SessionEpoch(1),
        HeapLimits {
            max_objects: 8,
            max_roots: 4,
            max_mutation_values: 2,
            max_mutation_bytes: 4,
            ..HeapLimits::default()
        },
    )
    .unwrap();
    assert!(matches!(
        h.alloc_tuple(&[Value::NULL; 3]),
        Err(HeapError::OperationLimit { .. })
    ));
    assert!(matches!(
        h.alloc_string("12345"),
        Err(HeapError::OperationLimit { .. })
    ));
    let text = h.alloc_text_buffer("é").unwrap();
    let before = h.info(text).unwrap().content_version;
    assert!(matches!(
        h.text_append(text, "abcde"),
        Err(HeapError::OperationLimit { .. })
    ));
    assert_eq!(h.info(text).unwrap().content_version, before);
    let mut output = [0; 4];
    assert_eq!(h.read_bytes(text, 0, &mut output).unwrap(), 2);
    assert_eq!(&output[..2], "é".as_bytes());
    let array = h.alloc_array(&[Value::int(9)]).unwrap();
    assert_eq!(
        h.set_field(array, 1, Value::NULL).unwrap_err(),
        HeapError::OutOfBounds
    );
    assert_eq!(h.get_field(array, 0).unwrap().as_int(), Some(9));
}

#[test]
fn live_arrays_return_all_payload_allocations_when_the_last_element_is_removed() {
    let mut h = heap(256);
    let info = allocation_counter::measure(|| {
        for _ in 0..128 {
            let array = h.alloc_array(&[Value::int(1)]).unwrap();
            assert_eq!(h.array_pop(array).unwrap().unwrap().as_int(), Some(1));
            assert_eq!(h.info(array).unwrap().len, 0);
        }
        assert_eq!(h.stats().payload_bytes, 0);
    });
    // Reserved slots remain outside this measured region; empty live containers
    // must not retain an uncharged tree allocation after the last chunk is gone.
    assert_eq!(info.bytes_current, 0);
    collect(&mut h);
}
