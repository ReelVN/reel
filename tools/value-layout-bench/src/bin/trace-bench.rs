use std::hint::black_box;
use std::mem::size_of;
use std::time::Instant;
use value_layout_bench::trace::*;
use value_layout_bench::{input_digest, validate_boundaries, Explicit, NanBox};

struct Sample {
    construct_ns: u128,
    execute_ns: u128,
    release_ns: u128,
    setup: Storage,
    end: Storage,
    closed: WideStats,
    snapshot: Snapshot,
}

fn sample<V: TraceValue>(program: &Program) -> Sample {
    let start = Instant::now();
    let mut state = black_box(State::<V>::new(black_box(program)));
    let construct_ns = start.elapsed().as_nanos();
    let setup = state.storage();
    let start = Instant::now();
    let execution = black_box(execute(black_box(program), black_box(&mut state))).unwrap();
    let execute_ns = start.elapsed().as_nanos();
    let end = state.storage();
    let snapshot = state.snapshot(execution);
    validate_snapshot(program, &snapshot);
    let start = Instant::now();
    let closed = black_box(state).close();
    let release_ns = start.elapsed().as_nanos();
    assert_eq!(closed.releases, closed.retains + closed.value_allocations);
    Sample {
        construct_ns,
        execute_ns,
        release_ns,
        setup,
        end,
        closed,
        snapshot,
    }
}

fn program_checksum(program: &Program) -> u64 {
    let mut sum = 0u64;
    for instruction in &program.code {
        let bits = instruction.op as u64
            | (instruction.a as u64) << 8
            | (instruction.b as u64) << 16
            | (instruction.c as u64) << 24
            | (instruction.immediate as u64) << 32;
        sum = sum.rotate_left(7).wrapping_add(bits);
    }
    for &input in &program.literals {
        sum = sum.rotate_left(7).wrapping_add(input_digest(input));
    }
    sum
}

fn snapshot_checksum(snapshot: &Snapshot) -> u64 {
    let mut sum = snapshot.host_digest;
    for &(kind, bits) in snapshot.registers.iter().chain(snapshot.fields.iter()) {
        sum = sum
            .rotate_left(7)
            .wrapping_add(bits ^ (kind as u64).rotate_left(53));
    }
    sum
}

fn emit<V: TraceValue>(program: &Program, index: usize, reference: &Snapshot, sample: Sample) {
    assert_eq!(&sample.snapshot, reference); // exact logical fields and all registers
    let fixed = sample.end.value_bytes + sample.end.host_table_bytes + sample.end.host_box_bytes;
    let setup_total = fixed + sample.setup.wide.table_bytes + sample.setup.wide.box_bytes;
    let end_total = fixed + sample.end.wide.table_bytes + sample.end.wide.box_bytes;
    let peak_total = fixed + sample.end.wide.peak_requested_bytes;
    let temporary_values = sample.end.wide.value_allocations - sample.setup.wide.value_allocations;
    let temporary_boxes = sample.end.wide.box_allocations - sample.setup.wide.box_allocations;
    let row = vec![
        program.name.clone(),
        V::NAME.into(),
        index.to_string(),
        program.cycles.to_string(),
        sample.snapshot.execution.instructions.to_string(),
        sample.snapshot.execution.type_checks.to_string(),
        sample.snapshot.execution.copies.to_string(),
        sample.snapshot.execution.branches_taken.to_string(),
        sample.snapshot.host_calls.to_string(),
        sample.construct_ns.to_string(),
        sample.execute_ns.to_string(),
        sample.release_ns.to_string(),
        format!("{:016x}", snapshot_checksum(&sample.snapshot)),
        format!("{:016x}", program_checksum(program)),
        sample.end.value_bytes.to_string(),
        sample.end.host_table_bytes.to_string(),
        sample.end.host_box_bytes.to_string(),
        sample.setup.wide.live.to_string(),
        sample.end.wide.live.to_string(),
        sample.end.wide.peak_live.to_string(),
        sample.end.wide.table_bytes.to_string(),
        sample.end.wide.box_bytes.to_string(),
        sample.end.wide.peak_requested_bytes.to_string(),
        setup_total.to_string(),
        end_total.to_string(),
        peak_total.to_string(),
        sample.closed.value_allocations.to_string(),
        temporary_values.to_string(),
        sample.closed.box_allocations.to_string(),
        temporary_boxes.to_string(),
        (temporary_boxes * 8).to_string(),
        sample.closed.box_deallocations.to_string(),
        sample.closed.retains.to_string(),
        sample.closed.releases.to_string(),
        sample.closed.table_allocations.to_string(),
        sample.closed.table_reallocations.to_string(),
        (sample.end.wide.table_reallocations - sample.setup.wide.table_reallocations).to_string(),
    ];
    println!("{}", row.join(","));
}

fn main() {
    let mut samples = 11;
    let mut cycles = 65_536;
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut validate_only = false;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--validate-only" {
            validate_only = true;
            index += 1;
            continue;
        }
        if args[index] == "--help" {
            println!("trace-bench [--samples 11] [--cycles 65536] [--seed 11400714819323198485] [--validate-only]");
            return;
        }
        let value: u64 = args
            .get(index + 1)
            .expect("missing value")
            .parse()
            .expect("invalid integer");
        match args[index].as_str() {
            "--samples" => samples = usize::try_from(value).unwrap(),
            "--cycles" => cycles = usize::try_from(value).unwrap(),
            "--seed" => seed = value,
            flag => panic!("unknown flag {flag}"),
        }
        index += 2;
    }
    assert!(samples >= 3 && (1..=1_048_576).contains(&cycles) && seed != 0);
    validate_boundaries();
    let mut programs = vec![
        story(cycles, seed),
        ui(cycles, seed),
        animation(cycles, seed),
    ];
    for percent in [0, 1, 10, 50, 100] {
        programs.push(wide_pressure(cycles, seed, percent));
    }
    if !validate_only {
        println!("case,repr,sample,cycles,instructions,type_checks,copies,branches_taken,host_calls,construct_ns,execute_ns,release_ns,checksum,program_checksum,value_bytes,host_table_bytes,host_box_bytes,wide_setup_live,wide_end_live,wide_peak_live,wide_table_bytes,wide_end_box_bytes,wide_peak_requested_bytes,setup_total_bytes,end_total_bytes,peak_requested_total_bytes,wide_value_allocations,temporary_wide_allocations,box_allocations,temporary_box_allocations,temporary_box_requested_bytes,box_deallocations,wide_retains,wide_releases,wide_table_allocations,wide_table_reallocations,temporary_table_reallocations");
    }
    for program in programs {
        let reference = sample::<Explicit>(&program).snapshot;
        assert_eq!(sample::<NanBox>(&program).snapshot, reference);
        assert_eq!(sample::<NanSlots>(&program).snapshot, reference);
        let harness = program.code.capacity() * size_of::<Instruction>()
            + program.literals.capacity() * size_of::<value_layout_bench::Input>();
        eprintln!("{}: cycles={cycles}, instructions={}, checks={}, copies={}, branches_taken={}, host_calls={}, code_len={}, literal_len={}, wide_pool_count={}, excluded_shared_program_bytes={harness}, program_checksum={:016x}",
            program.name, reference.execution.instructions, reference.execution.type_checks,
            reference.execution.copies, reference.execution.branches_taken, reference.host_calls,
            program.code.len(), program.literals.len(), program.wide_pool_count, program_checksum(&program));
        if validate_only {
            continue;
        }
        // Three-way rotating order balances each representation's position.
        for sample_index in 0..samples {
            for turn in 0..3 {
                match (sample_index + turn) % 3 {
                    0 => emit::<Explicit>(
                        &program,
                        sample_index,
                        &reference,
                        sample::<Explicit>(&program),
                    ),
                    1 => emit::<NanBox>(
                        &program,
                        sample_index,
                        &reference,
                        sample::<NanBox>(&program),
                    ),
                    _ => emit::<NanSlots>(
                        &program,
                        sample_index,
                        &reference,
                        sample::<NanSlots>(&program),
                    ),
                }
            }
        }
    }
}
