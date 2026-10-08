//! Fixed source/allocator probe. Run the built release binary, not cargo, for RSS.
use aside_compiler::{lex, parse};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, Stats, StatsAlloc};
use std::{alloc::System, hint::black_box, time::Instant};
#[global_allocator]
static ALLOCATOR: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;
const UNIT: &str = "// 固定源码：注释与中文\nfn panel(title, children = []) { column(gap: 12) { text(title); for child in children { child; } } }\nlet view = panel(f\"音量：{settings.music}\") { slider(value: bind(settings.music), min: 0, max: 1); };\nlet data = {...base, [key]: (1, 2), items: [...items, 3]};\n";
fn live(stats: Stats) -> isize {
    stats.bytes_allocated as isize + stats.bytes_reallocated - stats.bytes_deallocated as isize
}
fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "parse".into());
    assert!(matches!(mode.as_str(), "parse" | "lex" | "named"));
    let source = if mode == "named" {
        use std::fmt::Write;
        let mut source = String::from("f(");
        for i in 0..100_000 {
            write!(source, "k{i}:0,").unwrap();
        }
        source.push_str(");");
        source
    } else {
        UNIT.repeat(1024)
    };
    let tokens = lex(&source).unwrap().tokens.len();
    assert!(parse(&source).unwrap().is_ok()); // warm up, outside sample regions
    let samples = if mode == "named" { 11 } else { 31 };
    println!(
        "mode={mode},source_bytes={},units={},tokens={tokens},samples={samples}",
        source.len(),
        if mode == "named" { 100_000 } else { 1024 }
    );
    println!(
        "sample,parse_ns,allocations,reallocations,allocated_bytes,reallocated_net_bytes,retained_bytes,live_after_drop"
    );
    for sample in 0..samples {
        let region = Region::new(ALLOCATOR);
        let started = Instant::now();
        let (elapsed, stats, after_drop) = if mode != "lex" {
            let output = black_box(parse(black_box(&source)).unwrap());
            let elapsed = started.elapsed().as_nanos();
            assert!(output.is_ok());
            let stats = region.change();
            drop(output);
            (elapsed, stats, live(region.change()))
        } else {
            let output = black_box(lex(black_box(&source)).unwrap());
            let elapsed = started.elapsed().as_nanos();
            assert!(output.diagnostics.is_empty());
            let stats = region.change();
            drop(output);
            (elapsed, stats, live(region.change()))
        };
        assert_eq!(
            after_drop, 0,
            "frontend retained memory after dropping its output"
        );
        println!(
            "{sample},{elapsed},{},{},{},{},{},{after_drop}",
            stats.allocations,
            stats.reallocations,
            stats.bytes_allocated,
            stats.bytes_reallocated,
            live(stats)
        );
    }
}
