//! Reproducible compiler/bytecode measurements, with no bytecode dispatch.
use aside_compiler::{
    AnalysisOptions, analyze,
    lowering::{self, Linkage, Options},
    parse,
    semantic::NoModules,
};
use aside_vm::bytecode as bc;
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::{alloc::System, fmt::Write, time::Instant};
#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

fn main() {
    let mut args = std::env::args().skip(1);
    let n: usize = args
        .next()
        .unwrap_or_else(|| "1000".into())
        .parse()
        .unwrap();
    let mode = args.next().unwrap_or_else(|| "plain".into());
    let mut source = String::new();
    match mode.as_str() {
        "plain" => {
            for i in 0..n {
                writeln!(source, "fn f_{i}() {{ null }}").unwrap();
            }
        }
        "captures" => {
            for i in 0..n {
                writeln!(
                    source,
                    "fn f_{i}() {{ let mut x=0; let c=||{{x+=1;x}}; c() }}"
                )
                .unwrap();
            }
        }
        "temporaries" => {
            source.push_str("fn f(){");
            for _ in 0..n {
                source.push_str("1+2;");
            }
            source.push_str("3}");
        }
        "named" => {
            source.push_str("fn target(");
            for i in 0..n {
                write!(source, "p{i},").unwrap();
            }
            source.push_str("){null} fn caller(){target(");
            for i in (0..n).rev() {
                write!(source, "p{i}:{i},").unwrap();
            }
            source.push_str(")}");
        }
        "loops" => {
            source.push_str("fn choose(x)->bool{true} fn f(flag){");
            for _ in 0..n {
                source.push_str("while choose(if flag{break;}else{continue;}){} ");
            }
            source.push_str("7}");
        }
        _ => panic!("mode must be plain, captures, temporaries, named or loops"),
    }
    let time = Instant::now();
    let parsed = parse(&source).unwrap();
    let parse_us = time.elapsed().as_micros();
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics());
    let time = Instant::now();
    let analysis = analyze(&parsed, &AnalysisOptions::default(), &mut NoModules);
    let analysis_us = time.elapsed().as_micros();
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
    let module = analysis.into_module().unwrap();
    let region = Region::new(GLOBAL);
    let time = Instant::now();
    let ir = lowering::lower(&module, &Linkage::default(), Options::default());
    let lower_us = time.elapsed().as_micros();
    let ir = match ir {
        Ok(ir) => ir,
        Err(error) => {
            println!(
                "rejected,{mode},{n},lower,{:?},{:?},{},{}",
                error.kind, error.span, error.stats.work_units, error.detail
            );
            return;
        }
    };
    let lower_stats = ir.stats();
    let time = Instant::now();
    let result = match ir.emit() {
        Ok(result) => result,
        Err(error) => {
            println!(
                "rejected,{mode},{n},emit,{:?},{:?},{},{}",
                error.kind, error.span, error.stats.work_units, error.detail
            );
            return;
        }
    };
    let emit_verify_us = time.elapsed().as_micros();
    let allocation = region.change();
    let retained = allocation.bytes_allocated - allocation.bytes_deallocated;
    // There is no seed package in this workload; the ledger must exactly match
    // newly retained Vec/String allocations. Verifier scratch has been dropped.
    assert_eq!(
        result.stats.capacity_bytes, retained,
        "lowering capacity ledger differs from real retained allocation"
    );
    let time = Instant::now();
    let bytes = bc::encode(result.program.package(), bc::Limits::default()).unwrap();
    let encode_us = time.elapsed().as_micros();
    let time = Instant::now();
    let decoded = bc::decode(&bytes, bc::Limits::default()).unwrap();
    let decode_us = time.elapsed().as_micros();
    assert_eq!(result.program.package(), &decoded);
    let pstats = result.program.stats();
    println!(
        "mode,n,source_bytes,parse_us,analysis_us,lower_us,emit_verify_us,encode_us,decode_us,lower_work,total_compiler_work,compiler_retained_bytes,compiler_peak_upper_bytes,allocator_retained_bytes,allocations,reallocations,blocks,instructions,registers,max_function_registers,encoded_bytes,bytecode_records,verification_work,verification_scratch_peak_bytes,ir_capacity_bytes"
    );
    println!(
        "{mode},{n},{},{parse_us},{analysis_us},{lower_us},{emit_verify_us},{encode_us},{decode_us},{},{},{},{},{retained},{},{},{},{},{},{},{},{},{},{},{}",
        source.len(),
        lower_stats.work_units,
        result.stats.work_units,
        result.stats.capacity_bytes,
        result.stats.capacity_peak_bytes,
        allocation.allocations,
        allocation.reallocations,
        result.stats.blocks,
        result.stats.instructions,
        result.stats.registers,
        result.stats.max_function_registers,
        bytes.len(),
        pstats.records,
        pstats.validation_work,
        pstats.validation_scratch_peak,
        lower_stats.capacity_bytes
    );
}
