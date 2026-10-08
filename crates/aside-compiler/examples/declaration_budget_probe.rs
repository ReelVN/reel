//! Source/parser/HIR/analysis allocation admission probe; no execution.
use aside_compiler::{AnalysisOptions, analyze, parse, semantic::*};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::{alloc::System, fmt::Write, time::Instant};
#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;
fn main() {
    let count = std::env::args()
        .nth(1)
        .and_then(|n| n.parse::<usize>().ok())
        .unwrap_or(10_000);
    let mut source = String::from("fn consumer()->int{last()} ");
    for i in 0..count {
        write!(source, "fn unused_{i}(){{}} ").unwrap();
    }
    source.push_str("fn last()->int{1}");
    let parsed = parse(&source).unwrap();
    assert!(parsed.is_ok());
    println!(
        "declarations,max_work,analysis_us,symbols,symbol_bytes,work,diagnostics,exhausted,allocator_retained_bytes"
    );
    for max_work in [0, 1, 5, 100, count + 4, count + 10, 2 * count, 16_000_000] {
        let options = AnalysisOptions {
            limits: AnalysisLimits {
                max_work,
                ..Default::default()
            },
            ..Default::default()
        };
        let region = Region::new(GLOBAL);
        let start = Instant::now();
        let result = analyze(&parsed, &options, &mut NoModules);
        let elapsed = start.elapsed().as_micros();
        let allocations = region.change();
        let stats = &result.stats;
        println!(
            "{},{max_work},{elapsed},{},{},{},{},{},{}",
            count + 2,
            stats.symbols,
            stats.symbol_bytes,
            stats.work_units,
            result.diagnostics.len(),
            result.exhausted,
            allocations.bytes_allocated - allocations.bytes_deallocated
        );
    }
}
