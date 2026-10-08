use std::hint::black_box;
use std::mem::{align_of, size_of};
use std::time::Instant;
use value_layout_bench::*;

#[derive(Clone, Copy)]
enum Kernel {
    Integer,
    Float,
    Dispatch,
    Object,
    RandomInteger,
}

struct Case {
    name: String,
    inputs: Vec<Input>,
    order: Vec<u32>,
    kernel: Kernel,
    expected: u64,
    wide_count: usize,
}

impl Case {
    fn new(name: &str, n: usize, kernel: Kernel, wide_percent: Option<usize>, seed: u64) -> Self {
        let mut state = seed;
        let mut inputs = Vec::with_capacity(n);
        // Exactly floor(n * percentage / 100) wide ints, shuffled into the input.
        let wide_count = wide_percent.map_or(0, |p| n * p / 100);
        for i in 0..n {
            let r = random_word(&mut state);
            let small = ((r & ((1 << 47) - 1)) as i64) * if r & (1 << 63) == 0 { 1 } else { -1 };
            let integer = if i < wide_count {
                if i % 2 == 0 {
                    i64::MAX - (r & 0xffff) as i64
                } else {
                    i64::MIN + (r & 0xffff) as i64
                }
            } else {
                small
            };
            let value = match kernel {
                Kernel::Integer | Kernel::RandomInteger => Input::Int(integer),
                Kernel::Float => Input::Float(f64::from_bits(
                    (r & 0x800f_ffff_ffff_ffff) | (1022u64 << 52),
                )),
                Kernel::Object => Input::Object(r),
                Kernel::Dispatch => match r % 5 {
                    0 => Input::Null,
                    1 => Input::Bool(r & 0x100 != 0),
                    2 => Input::Int(small),
                    3 => Input::Float(f64::from_bits(
                        (r & 0x800f_ffff_ffff_ffff) | (1022u64 << 52),
                    )),
                    _ => Input::Object(r),
                },
            };
            inputs.push(value);
        }
        // Fisher-Yates prevents clustered wide ints and predictable mixed tags.
        for i in (1..n).rev() {
            let j = random_word(&mut state) as usize % (i + 1);
            inputs.swap(i, j);
        }
        let mut order = Vec::new();
        if matches!(kernel, Kernel::RandomInteger) {
            order = (0..n as u32).collect();
            for i in (1..n).rev() {
                let j = random_word(&mut state) as usize % (i + 1);
                order.swap(i, j);
            }
        }
        let expected = inputs
            .iter()
            .copied()
            .map(|input| match kernel {
                Kernel::Integer | Kernel::RandomInteger => match input {
                    Input::Int(v) => v as u64,
                    _ => unreachable!(),
                },
                Kernel::Float => match input {
                    Input::Float(v) => v.to_bits(),
                    _ => unreachable!(),
                },
                Kernel::Object => match input {
                    Input::Object(v) => v,
                    _ => unreachable!(),
                },
                Kernel::Dispatch => input_digest(input),
            })
            .fold(0u64, u64::wrapping_add);
        Self {
            name: name.into(),
            inputs,
            order,
            kernel,
            expected,
            wide_count,
        }
    }
}

struct Batch<V> {
    values: Vec<V>,
    heap: Heap,
}

#[inline(never)]
fn construct<V: Representation>(inputs: &[Input]) -> Batch<V> {
    let mut heap = Heap::default();
    let mut values = Vec::with_capacity(inputs.len());
    for &input in inputs {
        values.push(V::encode(input, &mut heap));
    }
    Batch { values, heap }
}

#[inline(never)]
fn read<V: Representation>(batch: &Batch<V>, case: &Case, passes: usize) -> u64 {
    let mut total = 0u64;
    for _ in 0..passes {
        // Opaque borrowed arrays and heap per pass force the optimized program to
        // perform loads again. No per-element black_box overhead in read loops.
        let values = black_box(batch.values.as_slice());
        let heap = black_box(&batch.heap);
        let mut sum = 0u64;
        match case.kernel {
            Kernel::Integer => {
                for &value in values {
                    sum = sum.wrapping_add(value.integer(heap) as u64);
                }
            }
            Kernel::Float => {
                for &value in values {
                    sum = sum.wrapping_add(value.float().to_bits());
                }
            }
            Kernel::Object => {
                for &value in values {
                    sum = sum.wrapping_add(
                        heap.objects
                            .get(value.object())
                            .expect("invalid object")
                            .payload,
                    );
                }
            }
            Kernel::Dispatch => {
                for &value in values {
                    sum = sum.wrapping_add(value_digest(value, heap));
                }
            }
            Kernel::RandomInteger => {
                for &index in black_box(case.order.as_slice()) {
                    sum = sum.wrapping_add(values[index as usize].integer(heap) as u64);
                }
            }
        }
        total = total.wrapping_add(black_box(sum));
    }
    black_box(total)
}

#[inline(never)]
fn release<V: Representation>(mut batch: Batch<V>) -> Heap {
    for &value in black_box(batch.values.as_slice()) {
        value.release(&mut batch.heap);
    }
    // Checks outside timing: return metadata/empty tables, then drop them timed.
    drop(batch.values);
    batch.heap
}

struct Sample {
    construct_ns: u128,
    read_ns: u128,
    release_ns: u128,
    checksum: u64,
    array_bytes: usize,
    object: ArenaStorage,
    wide: ArenaStorage,
}

fn sample<V: Representation>(case: &Case, passes: usize) -> Sample {
    let started = Instant::now();
    let batch = black_box(construct::<V>(black_box(&case.inputs)));
    let construct_ns = started.elapsed().as_nanos();
    let array_bytes = batch.values.capacity() * size_of::<V>();
    let object = batch.heap.objects.storage();
    let wide = batch.heap.wide.storage();
    // Full logical correspondence outside timing, independent of read kernel.
    for (&value, &input) in batch.values.iter().zip(&case.inputs) {
        assert_eq!(value_digest(value, &batch.heap), input_digest(input));
    }
    let started = Instant::now();
    let checksum = read(&batch, black_box(case), black_box(passes));
    let read_ns = started.elapsed().as_nanos();
    assert_eq!(checksum, case.expected.wrapping_mul(passes as u64));
    let started = Instant::now();
    let heap = release(black_box(batch));
    let object_after = heap.objects.storage();
    let wide_after = heap.wide.storage();
    drop(black_box(heap));
    let release_ns = started.elapsed().as_nanos();
    assert_eq!(object_after.live, 0);
    assert_eq!(wide_after.live, 0);
    Sample {
        construct_ns,
        read_ns,
        release_ns,
        checksum,
        array_bytes,
        object,
        wide,
    }
}

fn emit<V: Representation>(case: &Case, sample_index: usize, passes: usize, s: Sample) {
    let heap_bytes =
        s.object.table_bytes + s.object.box_bytes + s.wide.table_bytes + s.wide.box_bytes;
    // Allocation requests inferred from this code: one values Vec allocation,
    // one request per Box, plus table first allocation and growth reallocations.
    let allocs = 1
        + s.object.box_allocations
        + s.wide.box_allocations
        + s.object.table_allocations
        + s.wide.table_allocations;
    let reallocs = s.object.table_reallocations + s.wide.table_reallocations;
    println!(
        "{},{},{},{},{},{},{},{},{},{:016x},{},{},{},{},{},{},{},{},{},{},{}",
        case.name,
        V::NAME,
        sample_index,
        case.inputs.len(),
        passes,
        case.wide_count,
        s.construct_ns,
        s.read_ns,
        s.release_ns,
        s.checksum,
        s.array_bytes,
        s.object.live,
        s.object.table_bytes,
        s.object.box_bytes,
        s.wide.live,
        s.wide.table_bytes,
        s.wide.box_bytes,
        heap_bytes,
        s.array_bytes + heap_bytes,
        allocs,
        reallocs
    );
}

fn main() {
    let mut samples = 11usize;
    let mut n = 262_144usize;
    let mut cache_n = 4_194_304usize;
    let mut visits = 16_777_216usize;
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut validate_only = false;
    let args: Vec<_> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let flag = &args[i];
        if flag == "--validate-only" {
            validate_only = true;
            i += 1;
            continue;
        }
        if flag == "--help" {
            println!("value-layout-bench [--samples 11] [--n 262144] [--cache-n 4194304] [--visits 16777216] [--seed 11400714819323198485] [--validate-only]");
            return;
        }
        let value = args
            .get(i + 1)
            .expect("missing flag value")
            .parse::<u64>()
            .expect("invalid integer");
        match flag.as_str() {
            "--samples" => samples = usize::try_from(value).unwrap(),
            "--n" => n = usize::try_from(value).unwrap(),
            "--cache-n" => cache_n = usize::try_from(value).unwrap(),
            "--visits" => visits = usize::try_from(value).unwrap(),
            "--seed" => seed = value,
            _ => panic!("unknown flag {flag}"),
        }
        i += 2;
    }
    assert!(samples >= 3 && n > 0 && cache_n > 0 && visits > 0 && seed != 0);
    assert!(n <= FIELD_MAX as usize && cache_n <= FIELD_MAX as usize);
    validate_boundaries();
    eprintln!("boundary validation passed; Explicit={} align={}, NanBox={} align={}, Handle={}, Slot={}, Object={}; seed={seed}; samples={samples}; n={n}; cache_n={cache_n}; visits={visits}",
        size_of::<Explicit>(), align_of::<Explicit>(), size_of::<NanBox>(), align_of::<NanBox>(),
        size_of::<Handle>(), slot_size(), size_of::<Object>());
    if validate_only {
        return;
    }
    println!("case,repr,sample,n,passes,wide_inputs,construct_ns,read_ns,release_ns,checksum,array_bytes,object_count,object_table_bytes,object_box_bytes,wide_count,wide_table_bytes,wide_box_bytes,heap_bytes,total_bytes,allocation_requests,reallocation_requests");
    let mut specs = vec![
        ("int_4k".to_string(), 4096, Kernel::Integer, None),
        ("int_scan".to_string(), n, Kernel::Integer, None),
        ("float_scan".to_string(), n, Kernel::Float, None),
        ("mixed_dispatch".to_string(), n, Kernel::Dispatch, None),
        ("object_read".to_string(), n, Kernel::Object, None),
        ("cache_seq".to_string(), cache_n, Kernel::Integer, None),
        (
            "cache_random".to_string(),
            cache_n,
            Kernel::RandomInteger,
            None,
        ),
    ];
    for percent in [0, 1, 10, 50, 100] {
        specs.push((
            format!("wide_{percent:03}pct"),
            n,
            Kernel::Integer,
            Some(percent),
        ));
    }
    for (name, count, kernel, wide_percent) in specs {
        let case = Case::new(&name, count, kernel, wide_percent, seed);
        let passes = visits.div_ceil(count);
        let harness_bytes =
            case.inputs.capacity() * size_of::<Input>() + case.order.capacity() * size_of::<u32>();
        eprintln!("{}: n={count}, passes={passes}, wide={}, excluded_shared_harness_bytes={harness_bytes}", name, case.wide_count);
        // One excluded warmup per representation and case. Alternate timed order
        // to distribute drift; no two representations run concurrently.
        black_box(sample::<Explicit>(&case, passes));
        black_box(sample::<NanBox>(&case, passes));
        for index in 0..samples {
            if index % 2 == 0 {
                emit::<Explicit>(&case, index, passes, sample::<Explicit>(&case, passes));
                emit::<NanBox>(&case, index, passes, sample::<NanBox>(&case, passes));
            } else {
                emit::<NanBox>(&case, index, passes, sample::<NanBox>(&case, passes));
                emit::<Explicit>(&case, index, passes, sample::<Explicit>(&case, passes));
            }
        }
    }
}
