//! Direct frame publication retains the old enum assignment at every safe point.
use super::*;

#[derive(Debug, PartialEq)]
struct Observation {
    usage: SliceUsage,
    frames: Vec<u8>,
    caches: Vec<u8>,
    values: Vec<u8>,
    clocks: u32,
    #[cfg(feature = "execution-counts")]
    counts: ExecutionCounts,
}

fn observe(vm: &Vm, clocks: u32) -> Observation {
    let mut values = Vec::new();
    let mut caches = Vec::new();
    for frame in &vm.active.frames.data {
        let ranges = match frame {
            Frame::Script(frame) => {
                caches.push((
                    frame.instruction_end,
                    frame.instruction_pc,
                    frame.member_symbol,
                    frame.instruction.map(|op| (op.opcode, op.operands)),
                ));
                vec![frame.regs, frame.env, frame.args, frame.bound]
            }
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
        caches: postcard::to_allocvec(&caches).unwrap(),
        values: postcard::to_allocvec(&values).unwrap(),
        clocks,
        #[cfg(feature = "execution-counts")]
        counts: {
            let mut counts = vm.execution_counts();
            counts.heap_id = 0;
            counts
        },
    }
}

fn load(case: u8, encoded: bool, legacy: bool) -> Vm {
    let mut package = if case == 1 {
        fixtures::parameter_cell_default()
    } else {
        fixtures::helper_native()
    };
    if case == 2 {
        // The invalid callee faults before either the script or native body.
        package.functions[0].code[0].operands[1] = 0;
    }
    let support = if case == 1 {
        RuntimeSupport::default()
    } else {
        review_tests::echo_registry()
    };
    let program = verify(package, Limits::default()).unwrap();
    let program = if encoded {
        VerifiedProgram::decode(
            program.encode(Limits::default()).unwrap().as_bytes(),
            Limits::default(),
        )
        .unwrap()
    } else {
        program
    };
    let mut vm = Vm::load(program, support, VmLimits::default(), SessionEpoch(73)).unwrap();
    vm.work_probe.legacy_script_publication = legacy;
    vm
}

fn trace(case: u8, encoded: bool, legacy: bool, work: u64, deadline: bool) -> Vec<Observation> {
    let mut vm = load(case, encoded, legacy);
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
        let mut clocks = 0;
        let outcome = vm
            .run_slice(task, budget, || {
                let now = u64::from(clocks != 0);
                clocks += 1;
                now
            })
            .unwrap();
        observations.push(observe(&vm, clocks));
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
                    let first = vm.heap.sequence_get(value, 0).unwrap();
                    assert_eq!(vm.heap.sequence_get(first, 0).unwrap().as_int(), Some(43));
                }
                return observations;
            }
            RunOutcome::Fault(delivery) => {
                assert_eq!(case, 2);
                assert_eq!(
                    vm.delivery_fault(delivery).unwrap().unwrap().kind,
                    FaultKind::Type
                );
                return observations;
            }
            other => panic!("unexpected publication outcome {other:?}"),
        }
    }
    panic!("publication did not make bounded progress");
}

#[test]
fn payload_publication_preserves_each_slice_cache_roots_work_and_clock() {
    for case in 0..3 {
        for encoded in [false, true] {
            for work in [1, 12, 128] {
                for deadline in [false, true] {
                    assert_eq!(
                        trace(case, encoded, false, work, deadline),
                        trace(case, encoded, true, work, deadline),
                        "case={case} encoded={encoded} work={work} deadline={deadline}"
                    );
                }
            }
        }
    }
}

#[test]
fn publication_preserves_full_variant_replacement_when_destination_is_native() {
    let mut vm = load(0, false, false);
    let task = vm
        .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
        .unwrap();
    let Frame::Script(source) = vm.active.frames.data[0] else {
        unreachable!()
    };
    loop {
        match vm.run_slice(task, SliceBudget::default(), || 0).unwrap() {
            RunOutcome::BudgetYield(_) => {}
            RunOutcome::HostCall(_) => break,
            other => panic!("expected native call {other:?}"),
        }
    }
    let index = vm.active.frames.data.len() - 1;
    let original = vm.active.frames.data[index];
    assert!(matches!(original, Frame::Native(_)));
    vm.publish_script_frame(index, &source);
    assert_eq!(
        postcard::to_allocvec(&vm.active.frames.data[index]).unwrap(),
        postcard::to_allocvec(&Frame::Script(source)).unwrap()
    );
    vm.active.frames.data[index] = original;
}
