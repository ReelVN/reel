use super::*;

#[derive(Debug, PartialEq)]
struct Observation {
    usage: SliceUsage,
    frames: Vec<u8>,
    values: Vec<u8>,
    clock_calls: u32,
    #[cfg(feature = "execution-counts")]
    counts: ExecutionCounts,
}

fn observe(vm: &Vm, clock_calls: u32) -> Observation {
    let mut values = Vec::new();
    for frame in &vm.active.frames.data {
        let ranges = match frame {
            Frame::Script(frame) => vec![frame.regs, frame.env, frame.args, frame.bound],
            Frame::Native(frame) => {
                let mut ranges = vec![frame.args, frame.bound, frame.roots];
                ranges.extend(frame.bindings);
                ranges
            }
        };
        for range in ranges {
            values.push(
                (0..vm.slots.len(range).unwrap())
                    .map(|index| vm.slots.read(at(range, index)).ok())
                    .collect::<Vec<_>>(),
            );
        }
    }
    Observation {
        usage: vm.last_usage,
        frames: postcard::to_allocvec(&vm.active.frames.data).unwrap(),
        values: postcard::to_allocvec(&values).unwrap(),
        clock_calls,
        #[cfg(feature = "execution-counts")]
        counts: {
            let mut counts = vm.execution_counts();
            counts.heap_id = 0;
            counts
        },
    }
}

fn trace(general: bool, encoded: bool, case: u8, work: u64, deadline: bool) -> Vec<Observation> {
    let mut p = if case == 1 {
        fixtures::parameter_cell_default()
    } else {
        fixtures::helper_native()
    };
    if case == 2 {
        // All argument reads precede the callee's type checks; this null must
        // fault at the caller instead of reaching host echo or the callee body.
        p.functions[0].code[0].operands[1] = 0;
    }
    let support = if case == 1 {
        RuntimeSupport::default()
    } else {
        review_tests::echo_registry()
    };
    let program = verify(p, Limits::default()).unwrap();
    let program = if encoded {
        VerifiedProgram::decode(
            program.encode(Limits::default()).unwrap().as_bytes(),
            Limits::default(),
        )
        .unwrap()
    } else {
        program
    };
    let mut vm = Vm::load(program, support, VmLimits::default(), SessionEpoch(51)).unwrap();
    vm.work_probe.general_script_preparation = general;
    let task = vm
        .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
        .unwrap();
    let mut budget = SliceBudget {
        instructions: 1,
        work,
        allocation_bytes: 0,
        deadline_ns: deadline.then_some(1),
    };
    let mut observations = Vec::new();
    for _ in 0..5000 {
        let mut clock_calls = 0;
        let outcome = vm
            .run_slice(task, budget, || {
                let now = u64::from(clock_calls != 0);
                clock_calls += 1;
                now
            })
            .unwrap();
        observations.push(observe(&vm, clock_calls));
        match outcome {
            RunOutcome::BudgetYield(usage) => {
                budget.work = work.max(usage.required_work);
                budget.allocation_bytes = usage.required_allocation;
                review_tests::gc(&mut vm);
            }
            RunOutcome::HostCall(ticket) => {
                assert_eq!(case, 0);
                assert_eq!(vm.host_arguments(ticket).unwrap()[0].as_int(), Some(41));
                vm.respond(ticket, HostResponse::Ready(Value::int(42)))
                    .unwrap();
            }
            RunOutcome::Returned(delivery) => {
                assert_ne!(case, 2);
                let value = vm.delivery_value(delivery).unwrap();
                if case == 0 {
                    assert_eq!(value.as_int(), Some(42));
                } else {
                    let owner_result = vm.heap.sequence_get(value, 0).unwrap();
                    assert_eq!(
                        vm.heap.sequence_get(owner_result, 0).unwrap().as_int(),
                        Some(43)
                    );
                }
                return observations;
            }
            RunOutcome::Fault(delivery) => {
                assert_eq!(case, 2);
                let fault = vm.delivery_fault(delivery).unwrap().unwrap();
                assert_eq!(fault.kind, FaultKind::Type);
                assert_eq!(fault.location.code.function, fixtures::ENTRY);
                return observations;
            }
            other => panic!("unexpected outcome {other:?}"),
        }
    }
    panic!("preparation trace did not make bounded progress");
}

#[test]
fn hot_preparation_preserves_canonical_frames_roots_and_budget_boundaries() {
    eprintln!(
        "preparation-layout view={} phase={} canonical={}",
        size_of::<PreparationFrame>(),
        size_of::<PreparationPhase>(),
        size_of::<ScriptFrame>(),
    );
    assert!(size_of::<PreparationFrame>() < size_of::<ScriptFrame>());
    for encoded in [false, true] {
        for case in 0..3 {
            for work in [1, 7, 31, 64, 4096] {
                for deadline in [false, true] {
                    assert_eq!(
                        trace(false, encoded, case, work, deadline),
                        trace(true, encoded, case, work, deadline),
                        "encoded={encoded} case={case} work={work} deadline={deadline}"
                    );
                }
            }
        }
    }
}
