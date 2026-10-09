//! Reproducible V03 dispatch/call/root-lifecycle probe, using verified B01 packages.
use aside_vm::{bytecode::*, executor::*, *};
use std::time::Instant;
fn vm(p: BytecodePackage) -> Vm {
    let support = RuntimeSupport {
        opcodes: vec![],
        capabilities: vec!["host.echo".into()],
        natives: vec![NativeSupport {
            symbol: "host::echo".into(),
            abi_version: 1,
            signature: Signature {
                parameters: vec![Parameter {
                    name: SymbolId(4),
                    ty: TypeId(1),
                    mutable: false,
                    default: None,
                }],
                rest: None,
                return_type: TypeId(1),
                effects: Effects::ALL,
                property_bag_tail: None,
            },
            required_capabilities: vec!["host.echo".into()],
        }],
    };
    Vm::load(
        verify(p, Limits::default()).unwrap(),
        support,
        VmLimits::default(),
        SessionEpoch(1),
    )
    .unwrap()
}
fn loop_program(n: i64) -> BytecodePackage {
    let mut p = fixtures::exception_regions();
    p.constants = vec![Const::Int(0), Const::Int(1), Const::Int(n)];
    let f = &mut p.functions[0];
    f.regions.clear();
    f.source_map.clear();
    f.register_count = 4;
    let i = Instruction::new;
    f.code = vec![
        i(Opcode::LoadConst, 0, 0, 0),
        i(Opcode::LoadConst, 1, 1, 0),
        i(Opcode::LoadConst, 2, 2, 0),
        i(Opcode::Add, 0, 0, 1),
        i(Opcode::Lt, 3, 0, 2),
        i(Opcode::BranchBool, 3, 3, 6),
        i(Opcode::Return, 0, 0, 0),
    ];
    p
}
fn percentile(samples: &mut [u128], percent: usize) -> u128 {
    samples.sort_unstable();
    samples[(samples.len() - 1) * percent / 100]
}
fn main() {
    let mut numeric = vm(loop_program(1_000_000));
    numeric
        .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
        .unwrap();
    let mut timings = Vec::with_capacity(1000);
    let mut instructions = 0u64;
    let mut work = 0u64;
    let start = Instant::now();
    let allocated = allocation_counter::measure(|| {
        loop {
            let t = Instant::now();
            let out = numeric
                .run_slice(
                    numeric.task_id().unwrap(),
                    SliceBudget {
                        instructions: 4096,
                        ..SliceBudget::default()
                    },
                    || 0,
                )
                .unwrap();
            timings.push(t.elapsed().as_nanos());
            let stats = numeric.stats();
            instructions += stats.usage.instructions as u64;
            work += stats.usage.work;
            match out {
                RunOutcome::BudgetYield(_) => {}
                RunOutcome::Returned(id) => {
                    assert_eq!(
                        numeric.delivery_value(id).unwrap().as_int(),
                        Some(1_000_000)
                    );
                    break;
                }
                o => panic!("{o:?}"),
            }
        }
    });
    println!(
        "numeric iterations=1000000 instructions={instructions} work={work} elapsed_us={} slices={} slice_p50_ns={} slice_p95_ns={} slice_max_ns={} allocations={} allocated_bytes={} peak_shared_bytes={} program_bytes={} frame_pool_bytes={}",
        start.elapsed().as_micros(),
        timings.len(),
        percentile(&mut timings, 50),
        percentile(&mut timings, 95),
        timings.iter().max().unwrap(),
        allocated.count_total,
        allocated.bytes_total,
        numeric.stats().heap.peak_charged_bytes,
        numeric.stats().program_bytes,
        numeric.stats().slots.charged_bytes
    );
    let mut calls = vm(fixtures::helper_native());
    let mut timings = Vec::with_capacity(10000);
    let mut instructions = 0u64;
    let mut work = 0u64;
    let start = Instant::now();
    let allocated = allocation_counter::measure(|| {
        for round in 0..10000 {
            let t = Instant::now();
            calls
                .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
                .unwrap();
            loop {
                let out = calls
                    .run_slice(
                        calls.task_id().unwrap(),
                        SliceBudget {
                            instructions: 1,
                            ..SliceBudget::default()
                        },
                        || 0,
                    )
                    .unwrap();
                instructions += calls.stats().usage.instructions as u64;
                work += calls.stats().usage.work;
                match out {
                    RunOutcome::BudgetYield(_) => {}
                    RunOutcome::HostCall(ticket) => {
                        if round % 2 == 0 {
                            calls
                                .respond(ticket, HostResponse::Ready(Value::int(42)))
                                .unwrap();
                        } else {
                            calls
                                .respond(ticket, HostResponse::Pending(ticket.wait_id()))
                                .unwrap();
                        }
                    }
                    RunOutcome::Waiting(w) => calls
                        .complete(w, Completion::Ready(Value::int(42)))
                        .unwrap(),
                    RunOutcome::Returned(id) => {
                        assert_eq!(calls.delivery_value(id).unwrap().as_int(), Some(42));
                        calls.discard_delivery(id).unwrap();
                        break;
                    }
                    o => panic!("{o:?}"),
                }
            }
            timings.push(t.elapsed().as_nanos());
        }
    });
    println!(
        "calls ready=5000 pending=5000 native_entries={} instructions={instructions} work={work} elapsed_us={} call_p50_ns={} call_p95_ns={} call_max_ns={} allocations={} allocated_bytes={} active_ranges={} frames={} peak_shared_bytes={} native_null_publications={}",
        calls.stats().native_entries,
        start.elapsed().as_micros(),
        percentile(&mut timings, 50),
        percentile(&mut timings, 95),
        timings.iter().max().unwrap(),
        allocated.count_total,
        allocated.bytes_total,
        calls.stats().slots.active_ranges,
        calls.stats().frames,
        calls.stats().heap.peak_charged_bytes,
        calls.stats().native_null_publications
    );
    calls
        .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
        .unwrap();
    loop {
        if matches!(
            calls
                .run_slice(calls.task_id().unwrap(), SliceBudget::default(), || 0)
                .unwrap(),
            RunOutcome::HostCall(_)
        ) {
            break;
        }
    }
    let start = Instant::now();
    let mut units = 0u64;
    let mut descriptors = 0u64;
    let mut values = 0u64;
    let allocated_gc = allocation_counter::measure(|| {
        calls.begin_collection().unwrap();
        while calls.heap().phase() != GcPhase::Idle {
            let p = calls.gc_step(GcBudget::default()).unwrap();
            units += p.units as u64;
            descriptors += p.range_descriptors as u64;
            values += p.range_values as u64;
        }
    });
    println!(
        "gc native_live=true units={units} descriptors={descriptors} values={values} elapsed_ns={} allocations={} allocated_bytes={}",
        start.elapsed().as_nanos(),
        allocated_gc.count_total,
        allocated_gc.bytes_total
    );
    assert_eq!(allocated_gc.count_total, 0);
    calls.cancel(RootOwner(1)).unwrap();
    let start = Instant::now();
    let mut units = 0;
    let mut slices = 0;
    loop {
        slices += 1;
        match calls.run_slice(
            calls.task_id().unwrap(),
            SliceBudget {
                work: 16416,
                ..SliceBudget::default()
            },
            || 0,
        ) {
            Ok(RunOutcome::BudgetYield(u)) => units += u.work,
            Err(VmError::Idle) => {
                units += calls.stats().usage.work;
                break;
            }
            o => panic!("{o:?}"),
        }
    }
    println!(
        "cancel max_root_ranges=1024 conservative_work={units} slices={slices} elapsed_ns={} active_ranges={} retained_pool_bytes={}",
        start.elapsed().as_nanos(),
        calls.stats().slots.active_ranges,
        calls.stats().slots.charged_bytes
    );
    assert_eq!(allocated.count_total, 0);
}
