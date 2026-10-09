//! Fixed API costs, allocator residency and GC work; no dispatcher claims.
use aside_vm::*;
use std::time::Instant;

fn limits() -> HeapLimits {
    HeapLimits {
        max_objects: 32,
        max_roots: 4,
        max_root_ranges: 16,
        max_bytes: 4 << 20,
        ..HeapLimits::default()
    }
}
fn position(range: RootRangeToken, index: u32) -> RangePosition {
    RangePosition { range, index }
}
fn numeric(size: u32, marking: bool, clear: bool) {
    let mut samples = [0u64; 512];
    let mut charged = 0;
    let mut batch_total = 0;
    let mut loop_allocations = 0;
    let mut checksum = 0i64;
    let total = allocation_counter::measure(|| {
        let mut h = Heap::new(SessionEpoch(1), limits()).unwrap();
        let mut f = FrameSlots::new(
            &mut h,
            FrameSlotsLimits {
                initial_capacity: size,
                max_capacity: size * 2,
                max_window_len: size,
            },
        )
        .unwrap();
        let range = f
            .open(&mut h, RootOwner(1), RootKind::TaskFrame, size)
            .unwrap();
        if marking {
            h.begin_collection().unwrap();
        }
        let start = Instant::now();
        let a = allocation_counter::measure(|| {
            for (batch, sample) in samples.iter_mut().enumerate() {
                let clock = Instant::now();
                for offset in 0..256 {
                    let n = (batch * 256 + offset) as u32;
                    let at = position(range, std::hint::black_box(n % size));
                    f.write(&mut h, at, Value::int(n as i64)).unwrap();
                    checksum = checksum
                        .wrapping_add(std::hint::black_box(f.read(at).unwrap().as_int().unwrap()));
                    if clear {
                        f.clear(&mut h, at).unwrap();
                        assert_eq!(f.read(at).unwrap_err(), HeapError::UninitializedSlot);
                    }
                }
                *sample = clock.elapsed().as_nanos() as u64;
            }
        });
        batch_total = start.elapsed().as_nanos() as u64;
        loop_allocations = a.count_total;
        assert_eq!(a.count_total, 0);
        let operations = 512i64 * 256;
        assert_eq!(checksum, operations * (operations - 1) / 2);
        assert_eq!(h.stats().live_objects, 0);
        assert_eq!(h.stats().roots, 0);
        assert_eq!(h.stats().root_ranges, 1);
        f.close(&mut h).unwrap();
        drop(f);
        charged = h.stats().peak_charged_bytes;
    });
    assert_eq!(total.bytes_current, 0);
    assert!(total.bytes_max <= charged);
    samples.sort_unstable();
    println!(
        "numeric,size={size},phase={},clear={clear},iterations=131072,loop_allocations={loop_allocations},loop_total_ns={batch_total},ns_per_iteration={:.3},batch_p50_ns={},batch_p95_ns={},batch_p99_ns={},batch_max_ns={},actual_peak={},charged_peak={},setup_allocations={},checksum={checksum}",
        if marking { "Mark" } else { "Idle" },
        batch_total as f64 / 131072.0,
        samples[256],
        samples[486],
        samples[506],
        samples[511],
        total.bytes_max,
        charged,
        total.count_total
    );
}

fn lifecycle() {
    let mut peak = 0;
    let mut heap_new_ns = 0;
    let mut pool_new_ns = 0;
    let mut open_ns = 0;
    let mut relocation_ns = 0;
    let mut close_ns = 0;
    let mut gc_ns = 0;
    let mut descriptors = 0;
    let mut values = 0;
    let mut holes = 0;
    let mut gc_allocations = 0;
    let mut overlap = 0;
    let total = allocation_counter::measure(|| {
        let start = Instant::now();
        let mut h = Heap::new(SessionEpoch(1), limits()).unwrap();
        heap_new_ns = start.elapsed().as_nanos() as u64;
        let start = Instant::now();
        let mut f = FrameSlots::new(
            &mut h,
            FrameSlotsLimits {
                initial_capacity: 8192,
                max_capacity: 16384,
                max_window_len: 4096,
            },
        )
        .unwrap();
        pool_new_ns = start.elapsed().as_nanos() as u64;
        let start = Instant::now();
        let r = f
            .open(&mut h, RootOwner(1), RootKind::TaskFrame, 4096)
            .unwrap();
        open_ns = start.elapsed().as_nanos() as u64;
        let child = h.alloc_cell(Value::int(17)).unwrap();
        for i in 0..4096 {
            f.write(&mut h, position(r, i), child).unwrap();
        }
        h.begin_collection().unwrap();
        let start = Instant::now();
        let cost = f.relocate(&mut h, 16384).unwrap();
        relocation_ns = start.elapsed().as_nanos() as u64;
        overlap = cost.old_buffer_bytes + cost.new_buffer_bytes;
        let a = allocation_counter::measure(|| {
            while h.phase() != GcPhase::Idle {
                let start = Instant::now();
                let p = h
                    .gc_step_with_ranges(
                        GcBudget {
                            units: 1,
                            ..GcBudget::default()
                        },
                        f.roots(),
                    )
                    .unwrap();
                gc_ns = gc_ns.max(start.elapsed().as_nanos() as u64);
                assert!(p.units <= 1 && p.roots <= 1);
                descriptors += p.range_descriptors;
                values += p.range_values;
                holes += p.range_holes;
            }
        });
        gc_allocations += a.count_total;
        assert_eq!(gc_allocations, 0);
        assert_eq!(values, 4096);
        assert_eq!(h.get_field(child, 0).unwrap().as_int(), Some(17));
        let before_release = h.stats().charged_bytes;
        f.release(&mut h, r).unwrap();
        assert_eq!(h.stats().charged_bytes, before_release);
        h.begin_collection().unwrap();
        while h.phase() != GcPhase::Idle {
            h.gc_step_with_ranges(GcBudget::default(), f.roots())
                .unwrap();
        }
        assert_eq!(h.stats().live_objects, 0);
        let start = Instant::now();
        f.close(&mut h).unwrap();
        close_ns = start.elapsed().as_nanos() as u64;
        drop(f);
        assert_eq!(h.stats().external_bytes, 0);
        peak = h.stats().peak_charged_bytes;
    });
    assert_eq!(total.bytes_current, 0);
    assert!(total.bytes_max <= peak);
    println!(
        "lifecycle,window_values=4096,heap_new_ns={heap_new_ns},pool_new_ns={pool_new_ns},open_null_bitmap_ns={open_ns},relocate_ns={relocation_ns},close_ns={close_ns},gc_one_unit_max_ns={gc_ns},gc_allocations={gc_allocations},descriptors={descriptors},holes={holes},values={values},overlap_buffer_bytes={overlap},actual_peak={},charged_peak={peak},allocations={}",
        total.bytes_max, total.count_total
    );
}

fn main() {
    println!(
        "value_size={},value_align={},pointer_bits={}",
        std::mem::size_of::<Value>(),
        std::mem::align_of::<Value>(),
        usize::BITS
    );
    for size in [1, 16, 256, 4096] {
        for marking in [false, true] {
            for clear in [false, true] {
                numeric(size, marking, clear);
            }
        }
    }
    lifecycle();
}
