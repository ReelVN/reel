//! Reproducible host-only comparison of the removed monolithic path and bounded
//! dispatches. Input construction, VM scheduling, GC and rendering are excluded.
use super::*;
use std::time::Instant;
#[test]
#[ignore = "release host cost: sorted/mixed index construction and dispatch tail"]
fn bounded_list_index_cost() {
    for ordered in [true, false] {
        let count = 8192;
        let keys: Vec<_> = (0..count)
            .map(|n| Value::int(if ordered { n } else { (n * 997) % count }))
            .collect();
        let mut legacy_times = Vec::with_capacity(7);
        let mut bounded_times = Vec::with_capacity(7);
        let mut all_slices = Vec::with_capacity(7 * 2048);
        let mut final_work = 0;
        let mut calls = 0;
        let mut extra_heap = (0, 0);
        for _ in 0..7 {
            let mut legacy = heap();
            let input = entries(&mut legacy, &keys);
            let legacy_before = legacy.stats().charged_bytes;
            let begin = Instant::now();
            let _reservation = legacy
                .budget_usage()
                .reserve(count as u64 * size_of::<Value>() as u64)
                .unwrap();
            let mut values: Vec<_> = (0..count)
                .map(|i| legacy.sequence_get(input, i as u32).unwrap())
                .collect();
            values.sort_unstable_by(|&a, &b| {
                compare(
                    &legacy,
                    entry(&legacy, a, Entry::Key).unwrap(),
                    entry(&legacy, b, Entry::Key).unwrap(),
                    256,
                )
                .unwrap()
            });
            for pair in values.windows(2) {
                assert_ne!(
                    compare(
                        &legacy,
                        entry(&legacy, pair[0], Entry::Key).unwrap(),
                        entry(&legacy, pair[1], Entry::Key).unwrap(),
                        256
                    )
                    .unwrap(),
                    Ordering::Equal
                );
            }
            let result = legacy.alloc_array(&[]).unwrap();
            for value in values {
                legacy.array_push(result, value).unwrap();
            }
            let legacy_ns = begin.elapsed().as_nanos();
            let expected = super::values(&legacy, result);
            let mut current = heap();
            let input = entries(&mut current, &keys);
            let current_before = current.stats().charged_bytes;
            let mut state = Value::NULL;
            let mut slices = Vec::with_capacity(2048);
            let begin = Instant::now();
            let mut work = 0_u64;
            let actual = loop {
                let tick = Instant::now();
                let step =
                    advance(&mut current, input, state, NativeBudget::Exact(256), 256).unwrap();
                slices.push(tick.elapsed().as_nanos());
                match step {
                    Step::More {
                        state: next,
                        work: used,
                    } => {
                        assert!(used <= 256);
                        state = next;
                        work += u64::from(used);
                    }
                    Step::Ready { result, work: used } => {
                        work += u64::from(used);
                        break result;
                    }
                    Step::Budget { .. } => unreachable!(),
                }
            };
            let elapsed = begin.elapsed().as_nanos();
            assert_values(&super::values(&current, actual), &expected);
            legacy_times.push(legacy_ns);
            bounded_times.push(elapsed);
            calls = slices.len();
            final_work = work;
            extra_heap = (
                legacy.stats().peak_charged_bytes - legacy_before,
                current.stats().peak_charged_bytes - current_before,
            );
            all_slices.extend(slices);
        }
        eprintln!(
            "list_index_rounds ordered={ordered} legacy_ns={legacy_times:?} bounded_ns={bounded_times:?}"
        );
        legacy_times.sort_unstable();
        bounded_times.sort_unstable();
        all_slices.sort_unstable();
        eprintln!(
            "list_index_cost rows={count} ordered={ordered} repetitions=7 legacy_median_ns={} bounded_median_ns={} calls={calls} p99_dispatch_ns={} max_dispatch_ns={} work={final_work} legacy_extra_heap_bytes={} bounded_extra_heap_bytes={}",
            legacy_times[3],
            bounded_times[3],
            all_slices[(all_slices.len() - 1) * 99 / 100],
            all_slices.last().unwrap(),
            extra_heap.0,
            extra_heap.1
        );
    }
}
#[test]
#[ignore = "release operation attribution for the original unbatched sliced merge"]
fn unbatched_mixed_heap_operation_cost() {
    let mut heap = heap();
    let count = 8192;
    let keys: Vec<_> = (0..count).map(Value::int).collect();
    let input = entries(&mut heap, &keys);
    let refs: Vec<_> = (0..count)
        .map(|i| heap.sequence_get(input, i as u32).unwrap())
        .collect();
    let comparisons = 95001;
    let overwrites = 90112;
    let appends = 16384;
    let begin = Instant::now();
    for i in 0..comparisons * 2 {
        std::hint::black_box(
            entry(
                &heap,
                heap.sequence_get(input, ((i * 997) % count) as u32)
                    .unwrap(),
                Entry::Key,
            )
            .unwrap(),
        );
    }
    let key_reads = begin.elapsed().as_nanos();
    let begin = Instant::now();
    for i in 0..106496 {
        std::hint::black_box(
            heap.sequence_get(input, ((i * 997) % count) as u32)
                .unwrap(),
        );
    }
    let move_reads = begin.elapsed().as_nanos();
    let begin = Instant::now();
    for i in 0..overwrites {
        let index = ((i * 997) % count) as u32;
        heap.set_field(input, index, refs[index as usize]).unwrap();
    }
    let write_ns = begin.elapsed().as_nanos();
    let target = heap.alloc_array(&[]).unwrap();
    let begin = Instant::now();
    for i in 0..appends {
        heap.array_push(target, refs[(i % count) as usize]).unwrap();
    }
    let push_ns = begin.elapsed().as_nanos();
    eprintln!(
        "mixed_heap_ops comparisons={comparisons} key_reads_ns={key_reads} repeated_move_reads_ns={move_reads} overwrites={overwrites} overwrite_ns={write_ns} appends={appends} append_ns={push_ns}"
    );
}
