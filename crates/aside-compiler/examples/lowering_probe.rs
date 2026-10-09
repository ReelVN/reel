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
    let max_depth = args.next().map(|s| s.parse::<usize>().unwrap());
    let mut source = String::new();
    match mode.as_str() {
        "spread" => {
            source.push_str("fn f(g,xs){");
            for _ in 0..n { source.push_str("g(1,...xs,中文:2);"); }
            source.push_str("null}");
        }
        "rest" => {
            for i in 0..n {
                writeln!(source,"fn f_{i}(mut x=0,y=||{{x+=1;x}},...mut rest:array){{let c=||rest;(y,c)}}").unwrap();
            }
        }
        "call-evidence" => {
            source.push_str("fn take(mut a:int=0,b=||a,...mut rest:array){let c=||rest;(b,c)}fn f(target,xs,flag){try{target?.run?(...xs,if flag{throw 1;}else{2},中文:take(...xs)).value}finally{take();}}");
        }
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
        "initializer" => {
            source.push_str("let mut i=0;while if true{let mut condition=i;let probe=||condition;probe();i<2}else{false}{");
            for i in 0..n {
                writeln!(source, "if true{{let mut x_{i}=i;let a_{i}=||{{x_{i}+=1;x_{i}}};let b_{i}=||x_{i};a_{i}();b_{i}();}} ").unwrap();
            }
            source.push_str("i+=1;}");
        }
        "sequences" => {
            source.push_str("fn id(x){x}fn f(){[");
            for i in 0..n {
                write!(source, "(id({i}),[id({i})]),").unwrap();
            }
            source.push_str("]}");
        }
        "tables" => {
            source.push_str("fn id(x){x}fn f(){({");
            for i in 0..n {
                write!(source, "[id({})]:id({i}),", i % 10).unwrap();
            }
            source.push_str("})}");
        }
        "locations" => {
            source.push_str("fn id(x){x}fn f(a){let mut name=0;");
            for i in 0..n {
                write!(source, "name=a[id({i})]=a[id({})]=id({i});a[id({i})]+=id({i});", i + 1).unwrap();
            }
            source.push_str("name}");
        }
        "members" => {
            source.push_str("fn id(x){x}fn f(mut a,b){let mut name=0;let rhs=||{a=b;1};");
            for _ in 0..n {
                source.push_str("name=id(a).中文=id(a.子)[id(0)]=rhs();id(a).中文+=rhs();a.method(id(name));");
            }
            source.push_str("name}");
        }
        "optional" => {
            source.push_str("fn id(x){x}fn f(a,b){let mut name=0;let rhs=||{name+=1;name};");
            for _ in 0..n {
                source.push_str("a?.中文[id(0)]?(value:b?[rhs()]).尾; (a?.子).分数=rhs();");
            }
            source.push_str("name}");
        }
        "nested" => {
            source.push_str("fn f(){");
            for _ in 0..n { source.push('['); }
            source.push('1');
            for _ in 0..n { source.push(']'); }
            source.push('}');
        }
        "terminal" => {
            source.push_str("fn f(a,flag){");
            for _ in 0..n {
                source.push_str("try{a[0]={first:1,[if flag{throw 2;}else{throw 3;}]:4};}catch e{}finally{0;} ");
            }
            source.push_str("0}");
        }
        "evidence" => source.push_str(
            "fn id(x){x}fn f(a,b,flag,x={pair:(1,[])}){let mut name=0;let t={named:id(0),[id(1)]:id(1),[id(1)]:id(2),[[]]:id(3)};name=a[id(0)]=b[id(1)]=id(9);a[id(0)]+=id(2);try{a[id(0)]=[id(1),if flag{return t;}else{id(2)}];}finally{id(3);}let c=||x[\"pair\"][1];(t,c,a[0])}",
        ),
        _ => panic!("mode must be plain, captures, temporaries, named, loops, initializer, sequences, tables, locations, members, optional, nested, terminal or evidence"),
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
    let mut options = Options::default();
    if let Some(max_depth) = max_depth {
        options.limits.max_depth = max_depth;
    }
    let ir = lowering::lower(&module, &Linkage::default(), options);
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
    let allocation = region.change();
    assert_eq!(
        lower_stats.capacity_bytes,
        allocation.bytes_allocated - allocation.bytes_deallocated,
        "IR capacity ledger differs from real retained allocation"
    );
    if let Some(directory) = std::env::var_os("ASIDE_LOWERING_EVIDENCE") {
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("source.aside"), &source).unwrap();
        std::fs::write(directory.join("ir.txt"), format!("{ir:#?}")).unwrap();
    }
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
    if let Some(directory) = std::env::var_os("ASIDE_LOWERING_EVIDENCE") {
        let directory = std::path::PathBuf::from(directory);
        let mut disassembly = String::new();
        bc::disassemble(&result.program, &mut disassembly).unwrap();
        std::fs::write(directory.join("disassembly.txt"), disassembly).unwrap();
        std::fs::write(directory.join("program.asb"), &bytes).unwrap();
        std::fs::write(
            directory.join("tables.txt"),
            format!("{:#?}", result.program.package()),
        )
        .unwrap();
    }
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
