//! Reproducible source->parser->HIR->analysis measurements; no execution probe.
use aside_compiler::{AnalysisOptions, analyze, hir::*, parse, semantic::NoModules};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::{alloc::System, fmt::Write, time::Instant};
#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;
fn main() {
    let mut args = std::env::args().skip(1);
    let count = args
        .next()
        .and_then(|n| n.parse::<usize>().ok())
        .unwrap_or(10_000);
    let mode = args.next().unwrap_or_else(|| "captures".into());
    let mut source = String::new();
    match mode.as_str() {
        "captures" => {
            for i in 0..count {
                writeln!(
                    source,
                    "fn worker_{i}(a:int=1)->int {{let mut n=a; let step=||{{n+=1;n}}; step()}}"
                )
                .unwrap();
            }
        }
        "named" => {
            source.push_str("fn target(");
            for i in 0..count {
                write!(source, "p{i},").unwrap();
            }
            source.push_str("){null} target(");
            for i in (0..count).rev() {
                write!(source, "p{i}:1,").unwrap();
            }
            source.push_str(");");
        }
        _ => panic!("mode must be captures or named"),
    }
    let start = Instant::now();
    let parsed = parse(&source).unwrap();
    let parse_us = start.elapsed().as_micros();
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics());
    let region = Region::new(GLOBAL);
    let start = Instant::now();
    let result = analyze(&parsed, &AnalysisOptions::default(), &mut NoModules);
    let analysis_us = start.elapsed().as_micros();
    let allocations = region.change();
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result.module().unwrap();
    let s = module.stats();
    println!(
        "functions,source_bytes,parse_us,analysis_us,nodes,symbols,captures,work,accounted_bytes,allocator_retained_bytes,allocations,node_size,symbol_size,capture_size,symbol_capacity_bytes,capture_capacity_bytes,mode"
    );
    println!(
        "{count},{},{parse_us},{analysis_us},{},{},{},{},{},{},{},{},{},{},{},{},{mode}",
        source.len(),
        s.nodes,
        s.symbols,
        s.captures,
        s.work_units,
        s.retained_bytes,
        allocations.bytes_allocated - allocations.bytes_deallocated,
        allocations.allocations,
        std::mem::size_of::<HirNode>(),
        std::mem::size_of::<Symbol>(),
        std::mem::size_of::<Capture>(),
        s.symbol_bytes,
        s.capture_bytes
    );
}
