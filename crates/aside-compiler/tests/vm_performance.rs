//! Reproducible interpreter workloads, with startup and steady execution separated.
//!
//! Run `cargo test --release --locked -p aside-compiler --test vm_performance
//! measure_vm_kernels -- --ignored --nocapture --test-threads=1`.
//! `ASIDE_BENCH_N` (default 2000), `ASIDE_BENCH_SAMPLES` (default 9), and
//! `ASIDE_BENCH_FILTER` select the work and samples. `ASIDE_BENCH_OUTPUT` writes
//! the same TSV to an artifact path. Profiling builds measure diagnostics too.
//!
//! Workload selection follows abstraction-oriented measurement in
//! https://github.com/smarr/are-we-fast-yet and startup/execution separation in
//! https://wasmi-labs.github.io/blog/posts/wasmi-v2.0/. Kernels are original,
//! engine-independent Aside sources; https://luau.org/performance/ identifies
//! relevant member, call, closure and collector optimizations for comparison.

#[path = "vm_performance/runner.rs"]
mod runner;

use runner::{Fixture, Mode};
use std::{fmt::Write as _, time::Instant};

const CASES: [&str; 19] = [
    "arithmetic",
    "calls",
    "fields",
    "named_tables",
    "polymorphic_tables",
    "named_host_fields",
    "methods",
    "retained_method",
    "captured_cells",
    "host_calls",
    "host_methods",
    "retained_host_method",
    "host_motion_methods",
    "locals",
    "allocation",
    "reactive",
    "reactive_fields",
    "text_compare",
    "string_keys",
];

const DEADLINE_CASES: [&str; 4] = [
    "fields",
    "reactive_fields",
    "reactive_wide_fields",
    "host_motion_methods",
];

fn oracle(name: &str, n: i64) -> i64 {
    match name {
        "arithmetic" => (0..n).map(|i| (i * 3 + 1) % 97).sum(),
        "calls" => (0..n).map(|i| i * 3 + 1).sum(),
        "fields" | "methods" | "retained_method" | "named_tables" | "polymorphic_tables"
        | "named_host_fields" => n * 10,
        "host_calls" | "host_methods" | "retained_host_method" | "host_motion_methods" => {
            n * (n - 1) / 2
        }
        "captured_cells" => n * (n - 1) / 2,
        "locals" => (0..n).map(|i| i * 16 + 136).sum(),
        "allocation" => (0..n).map(|i| i + 3).sum(),
        "reactive" | "reactive_fields" => (0..n).map(|i| i * 3 + 7).sum(),
        "text_compare" => n * 3,
        "reactive_wide_fields" => n * 36 + n * (n - 1) / 2,
        "string_keys" => n * 7,
        _ => unreachable!(),
    }
}

#[test]
fn deadline_kernels_preserve_oracles_in_encoded_and_unpacked_code() {
    let fixture = Fixture::compile();
    for encoded in [false, true] {
        for name in DEADLINE_CASES {
            let (mut vm, _) = fixture.load(encoded);
            let baseline = fixture.run(&mut vm, name, 7, Mode::Batch);
            let row = fixture.run(&mut vm, name, 7, Mode::Deadline);
            assert_eq!(row.value, oracle(name, 7), "{name} encoded={encoded}");
            assert_eq!(row.value, baseline.value);
            assert!(row.clock_queries > 0);
        }
    }
}

#[test]
fn kernels_preserve_oracles_across_codec_gc_and_budget_boundaries() {
    let fixture = Fixture::compile();
    for encoded in [false, true] {
        for mode in [Mode::Batch, Mode::Sliced, Mode::IncrementalGc] {
            let (mut vm, _) = fixture.load(encoded);
            for name in CASES {
                let result = fixture.run(&mut vm, name, 7, mode);
                assert_eq!(
                    result.value,
                    oracle(name, 7),
                    "{name} {mode:?} encoded={encoded}"
                );
                assert!(result.instructions > 0);
                #[cfg(feature = "execution-counts")]
                {
                    let counts = result.counts;
                    assert_eq!(counts.instructions, result.instructions);
                    assert_eq!(counts.work, result.work);
                    assert_eq!(counts.slices, result.slices);
                    assert_eq!(counts.native_entries, result.host_entries);
                    assert_eq!(counts.derived_starts, result.derived_starts);
                    assert_eq!(
                        counts.opcode_admissions().map(|(_, n)| n).sum::<u64>(),
                        result.instructions,
                        "{name} {mode:?} encoded={encoded}"
                    );
                    assert!(counts.bytecode_execute_steps <= counts.instructions);
                    assert!(counts.script_entries > 0);
                }
                if mode == Mode::IncrementalGc {
                    assert!(result.gc_units > 0);
                }
                if name == "host_calls" {
                    assert_eq!(result.host_entries, 7);
                }
                if matches!(name, "reactive" | "reactive_fields") {
                    assert!(result.derived_starts >= 6);
                }
            }
        }
    }
}

fn setting(name: &str, default: usize, max: usize) -> usize {
    let value = std::env::var(name).map_or(default, |s| s.parse().expect(name));
    assert!((1..=max).contains(&value), "{name} must be in 1..={max}");
    value
}

#[cfg(feature = "execution-counts")]
#[test]
#[ignore = "count-only structural diagnostics; not performance measurement"]
fn inspect_vm_kernel_counts() {
    let fixture = Fixture::compile();
    let n = setting("ASIDE_BENCH_N", 16, 1_000_000) as i64;
    let mut output = String::new();
    writeln!(output, "# vm-execution-counts-v1 n={n}; clocks and GC scans excluded from counts; counts include run/task/host delivery root operations; bytecode admissions include lazy-read retries").unwrap();
    writeln!(
        output,
        "# opcode-order={:?}",
        aside_vm::bytecode::INSTRUCTION_SPECS
            .iter()
            .map(|s| s.opcode)
            .collect::<Vec<_>>()
    )
    .unwrap();
    for name in CASES {
        for mode in [Mode::Batch, Mode::Sliced, Mode::IncrementalGc] {
            let (mut vm, _) = fixture.load(true);
            fixture.collect(&mut vm);
            let row = fixture.run(&mut vm, name, n, mode);
            assert_eq!(row.value, oracle(name, n));
            assert_eq!(
                row.counts.opcode_admissions().map(|(_, n)| n).sum::<u64>(),
                row.instructions
            );
            writeln!(
                output,
                "case={name} mode={mode:?} n={n} oracle={} counts={:?}",
                row.value, row.counts
            )
            .unwrap();
        }
    }
    if let Ok(path) = std::env::var("ASIDE_BENCH_OUTPUT") {
        std::fs::write(path, &output).unwrap();
    }
    print!("{output}");
}

#[test]
#[ignore = "release interpreter performance measurement; uses wall time, not target FPS"]
fn measure_vm_kernels() {
    measure(&CASES, &[Mode::Batch, Mode::Sliced, Mode::IncrementalGc]);
}

#[test]
#[ignore = "release VM measurements with the player's monotonic 2 ms deadline"]
fn measure_vm_deadline_kernels() {
    measure(&DEADLINE_CASES, &[Mode::Batch, Mode::Deadline]);
}

#[cfg(feature = "profiling")]
#[test]
#[ignore = "sampled operation attribution, not an exclusive CPU or speedup measurement"]
fn inspect_vm_lifecycle() {
    assert!(
        !std::hint::black_box(cfg!(debug_assertions)),
        "use --release"
    );
    let fixture = Fixture::compile();
    let n = setting("ASIDE_BENCH_N", 10000, 1_000_000) as i64;
    let mut output = String::new();
    for name in [
        "calls",
        "fields",
        "captured_cells",
        "host_motion_methods",
        "reactive_wide_fields",
    ] {
        for mode in [Mode::Batch, Mode::Deadline] {
            let (mut vm, _) = fixture.load(true);
            for _ in 0..3 {
                fixture.collect(&mut vm);
                assert_eq!(fixture.run(&mut vm, name, n, mode).value, oracle(name, n));
            }
            fixture.collect(&mut vm);
            let before = vm.driver_costs();
            let row = fixture.run(&mut vm, name, n, mode);
            let driver = vm.driver_costs().checked_delta(&before).unwrap();
            let part = driver.lifecycle;
            assert_eq!(row.value, oracle(name, n));
            assert_eq!(part.errors, 0);
            assert_eq!(part.run_ns, driver.run_ns);
            assert_eq!(
                part.parts.iter().map(|p| p.wall_ns).sum::<u64>()
                    + part.unattributed_ns
                    + part.observer_ns,
                part.run_ns
            );
            let parts: std::collections::BTreeMap<_, _> = aside_vm::executor::LIFECYCLE_NAMES
                .into_iter()
                .zip(part.parts)
                .map(|(name, p)| {
                    (
                        name,
                        serde_json::json!({"calls":p.calls,"wall_ns":p.wall_ns}),
                    )
                })
                .collect();
            writeln!(output, "{}", serde_json::json!({"case":name,"mode":format!("{mode:?}"),"n":n,
                "wall_ns":row.wall_ns,"instructions":row.instructions,"work":row.work,"slices":row.slices,
                "clock_queries":row.clock_queries,"host_entries":row.host_entries,"derived_starts":row.derived_starts,
                "samples":driver.samples,"driver_run_ns":driver.run_ns,"driver_loop_ns":driver.loop_ns,
                "sampled_instructions":driver.sampled_instructions,"sampled_work":driver.sampled_work,
                "unattributed_ns":part.unattributed_ns,"observer_ns":part.observer_ns,
                "observer_clock_queries":part.observer_clock_queries,"errors":part.errors,"parts":parts})).unwrap();
        }
    }
    if let Ok(path) = std::env::var("ASIDE_BENCH_OUTPUT") {
        std::fs::write(path, &output).unwrap();
    }
    print!("{output}");
}

fn measure(cases: &[&str], modes: &[Mode]) {
    assert!(
        !std::hint::black_box(cfg!(debug_assertions)),
        "use --release for VM measurements"
    );
    let n = setting("ASIDE_BENCH_N", 2000, 1_000_000);
    let samples = setting("ASIDE_BENCH_SAMPLES", 9, 1000);
    let filter = std::env::var("ASIDE_BENCH_FILTER").unwrap_or_default();
    assert!(
        cases
            .iter()
            .any(|name| filter.is_empty() || name.contains(&filter)),
        "ASIDE_BENCH_FILTER matched no workload"
    );
    let start = Instant::now();
    let fixture = Fixture::compile();
    let compile_ns = start.elapsed().as_nanos();
    let mut output = String::new();
    writeln!(
        output,
        "# vm-benchmark-v1 arch={} os={} debug={} value_bytes={} warmup=3 samples={samples} n={n}",
        std::env::consts::ARCH,
        std::env::consts::OS,
        cfg!(debug_assertions),
        size_of::<aside_vm::Value>()
    )
    .unwrap();
    writeln!(output, "# timing=wall_ns includes task entry, run_slice, host dispatch, delivery release; gc_ns is nested; compile_ns={compile_ns}; decode/load/init measured separately; explicit collection between samples excluded; peak_charged is VM-lifetime cumulative").unwrap();
    writeln!(output, "kind\tcase\tmode\tsample\tn\twall_ns\tgc_ns\tinstructions\twork\tslices\thost_entries\tderived_starts\tcollections\tgc_units\tcharged_before\tcharged_after\tpeak_charged\toracle\tclock_queries").unwrap();
    for name in cases
        .iter()
        .copied()
        .filter(|name| filter.is_empty() || name.contains(&filter))
    {
        for &mode in modes {
            if mode == Mode::IncrementalGc && name != "allocation" {
                continue;
            }
            let (mut vm, startup) = fixture.load(true);
            writeln!(
                output,
                "# startup case={name} mode={mode:?} decode_ns={} load_ns={} init_ns={}",
                startup[0], startup[1], startup[2]
            )
            .unwrap();
            for _ in 0..3 {
                fixture.collect(&mut vm);
                assert_eq!(
                    fixture.run(&mut vm, name, n as i64, mode).value,
                    oracle(name, n as i64)
                );
            }
            for sample in 0..samples {
                fixture.collect(&mut vm);
                let row = fixture.run(&mut vm, name, n as i64, mode);
                assert_eq!(row.value, oracle(name, n as i64), "{name} {mode:?}");
                writeln!(output, "sample\t{name}\t{mode:?}\t{sample}\t{n}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}", row.wall_ns, row.gc_ns, row.instructions, row.work, row.slices, row.host_entries, row.derived_starts, row.collections, row.gc_units, row.charged_before, row.charged_after, row.peak_charged, row.value, row.clock_queries).unwrap();
            }
        }
    }
    if let Ok(path) = std::env::var("ASIDE_BENCH_OUTPUT") {
        std::fs::write(path, &output).unwrap();
    }
    print!("{output}");
}
