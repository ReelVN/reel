//! Fixed storage/GC load; allocator instrumentation stays outside the production crate.
use aside_vm::*;
use std::time::Instant;

#[derive(Default)]
struct Report {
    operations: u64,
    max_mutation_ns: u64,
    steps: usize,
    max_step_ns: u64,
    max_mark_ns: u64,
    max_weak_ns: u64,
    max_sweep_ns: u64,
    gc_allocations: u64,
    peak_charged: u64,
    peak_pending: u64,
    reserved: u64,
    chunks: u64,
    final_live: u32,
    freed_bytes: u64,
    released_edges: u64,
    max_drop_heap_ns: u64,
}
fn timed<T>(report: &mut Report, op: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let result = op();
    report.max_mutation_ns = report
        .max_mutation_ns
        .max(start.elapsed().as_nanos() as u64);
    report.operations += 1;
    result
}
fn cycle(h: &mut Heap, report: &mut Report, samples: &mut [u64; 16384]) {
    h.begin_collection().unwrap();
    while h.phase() != GcPhase::Idle {
        let phase = h.phase();
        let mut ns = 0;
        let mut progress = GcProgress::default();
        // Keep measured phase workloads separate; transitions themselves are O(1).
        let mut budget = GcBudget::default();
        match phase {
            GcPhase::Mark => {
                budget.weak = 0;
                budget.objects = 0;
            }
            GcPhase::Weak => {
                budget.objects = 0;
            }
            _ => {}
        }
        let allocations = allocation_counter::measure(|| {
            let start = Instant::now();
            progress = h.gc_step(budget);
            ns = start.elapsed().as_nanos() as u64;
        });
        report.gc_allocations += allocations.count_total;
        assert_eq!(allocations.count_total, 0);
        assert!(progress.units <= 256 && progress.bytes <= 16_384);
        samples[report.steps] = ns;
        report.steps += 1;
        report.max_step_ns = report.max_step_ns.max(ns);
        match phase {
            GcPhase::Mark => report.max_mark_ns = report.max_mark_ns.max(ns),
            GcPhase::Weak => report.max_weak_ns = report.max_weak_ns.max(ns),
            GcPhase::Sweep => report.max_sweep_ns = report.max_sweep_ns.max(ns),
            GcPhase::Idle => unreachable!(),
        }
        report.freed_bytes += progress.bytes;
        report.released_edges += progress.release_edges as u64;
        report.peak_pending = h.stats().peak_pending_bytes;
    }
}
fn run(name: &str, objects: u32, build: impl FnOnce(&mut Heap, &mut Report) -> Value) {
    let mut report = Report::default();
    let mut samples = [0u64; 16384];
    let total = allocation_counter::measure(|| {
        let mut h = timed(&mut report, || {
            Heap::new(
                SessionEpoch(1),
                HeapLimits {
                    max_objects: objects,
                    max_roots: 64,
                    max_bytes: 32 * 1024 * 1024,
                    ..HeapLimits::default()
                },
            )
            .unwrap()
        });
        let value = build(&mut h, &mut report);
        let root = h.add_root(RootOwner(1), RootKind::Host, value).unwrap();
        cycle(&mut h, &mut report, &mut samples);
        assert!(h.info(value).is_ok());
        h.release_root(root).unwrap();
        cycle(&mut h, &mut report, &mut samples);
        let stats = h.stats();
        assert_eq!(stats.live_objects, 0);
        assert_eq!(stats.pending_bytes, 0);
        assert_eq!(stats.payload_bytes, 0);
        report.final_live = stats.live_objects;
        report.peak_charged = stats.peak_charged_bytes;
        report.reserved = stats.reserved_bytes;
        report.chunks = stats.chunk_allocations;
        let start = Instant::now();
        drop(h);
        report.max_drop_heap_ns = start.elapsed().as_nanos() as u64;
    });
    assert_eq!(
        total.bytes_current, 0,
        "no allocator bytes left after dropping heap"
    );
    assert!(
        total.bytes_max <= report.peak_charged,
        "conservative accounting must cover allocator peak"
    );
    samples[..report.steps].sort_unstable();
    let quantile = |percent: usize| samples[(report.steps - 1) * percent / 100];
    println!(
        "{name},operations={},max_mutation_ns={},gc_steps={},gc_p50_ns={},gc_p95_ns={},gc_p99_ns={},gc_max_ns={},mark_max_ns={},weak_max_ns={},sweep_max_ns={},gc_allocations={},allocation_count={},allocated_bytes={},actual_peak_bytes={},peak_charged_bytes={},reserved_bytes={},peak_pending_bytes={},chunk_allocations={},released_edges={},freed_charged_bytes={},final_live={},heap_drop_ns={}",
        report.operations,
        report.max_mutation_ns,
        report.steps,
        quantile(50),
        quantile(95),
        quantile(99),
        report.max_step_ns,
        report.max_mark_ns,
        report.max_weak_ns,
        report.max_sweep_ns,
        report.gc_allocations,
        total.count_total,
        total.bytes_total,
        total.bytes_max,
        report.peak_charged,
        report.reserved,
        report.peak_pending,
        report.chunks,
        report.released_edges,
        report.freed_bytes,
        report.final_live,
        report.max_drop_heap_ns
    );
}
fn main() {
    println!(
        "value_size={},value_align={},pointer_bits={},gc_units=256,gc_bytes=16384",
        std::mem::size_of::<Value>(),
        std::mem::align_of::<Value>(),
        usize::BITS
    );
    run("shared_cells_10000", 64, |h, r| {
        let cell = timed(r, || h.alloc_cell(Value::int(0)).unwrap());
        let a = timed(r, || h.alloc_record(ObjectKind::Closure, &[cell]).unwrap());
        let b = timed(r, || h.alloc_record(ObjectKind::Closure, &[cell]).unwrap());
        for n in 0..10_000 {
            timed(r, || {
                h.set_field(h.get_field(a, 0).unwrap(), 0, Value::int(n))
                    .unwrap()
            });
            assert_eq!(
                h.get_field(h.get_field(b, 0).unwrap(), 0).unwrap().as_int(),
                Some(n)
            );
        }
        timed(r, || h.alloc_tuple(&[a, b]).unwrap())
    });
    run("deep_chain_10000", 12_000, |h, r| {
        let mut chain = Value::NULL;
        for _ in 0..10_000 {
            chain = timed(r, || h.alloc_cell(chain).unwrap());
        }
        chain
    });
    run("cycles_2000", 5000, |h, r| {
        let outer = timed(r, || h.alloc_array(&[]).unwrap());
        for _ in 0..2000 {
            let a = timed(r, || h.alloc_array(&[]).unwrap());
            let b = timed(r, || h.alloc_array(&[a]).unwrap());
            timed(r, || h.array_push(a, b).unwrap());
            timed(r, || h.array_push(outer, a).unwrap());
        }
        outer
    });
    run("array_100000", 8, |h, r| {
        let a = timed(r, || h.alloc_array(&[]).unwrap());
        for n in 0..100_000 {
            timed(r, || h.array_push(a, Value::int(n)).unwrap());
        }
        a
    });
    run("utf8_buffer_1mib", 8, |h, r| {
        let b = timed(r, || h.alloc_text_buffer("").unwrap());
        // 16 bytes: combining scalar + ZWJ emoji + NUL; repeated to exactly 1MiB.
        let text = "e\u{301}👩‍👧\0A";
        assert_eq!(text.len(), 16);
        for _ in 0..65_536 {
            timed(r, || h.text_append(b, text).unwrap());
        }
        b
    });
    run("byte_buffer_1mib", 8, |h, r| {
        let b = timed(r, || h.alloc_byte_buffer(&[]).unwrap());
        let block = [0x80; 4096];
        for _ in 0..256 {
            timed(r, || h.bytes_append(b, &block).unwrap());
        }
        b
    });
}
