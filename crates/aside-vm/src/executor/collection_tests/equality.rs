//! Byte equality batching preserves the single-byte walk at observable yields.
use super::*;
use crate::snapshot::{SnapshotBudget, SnapshotRegistry, VmSnapshot};

fn comparison(left: Const, right: Const, negated: bool) -> BytecodePackage {
    package(
        vec![left, right],
        vec![
            op(Opcode::LoadConst, 0, 0, 0),
            op(Opcode::LoadConst, 1, 1, 0),
            op(if negated { Opcode::Ne } else { Opcode::Eq }, 2, 0, 1),
            op(Opcode::Return, 2, 0, 0),
        ],
        vec![],
        3,
    )
}

#[derive(Debug, PartialEq)]
struct Observation {
    usage: SliceUsage,
    frames: Vec<u8>,
    cursors: Vec<u32>,
    roots: Vec<u8>,
    clock_calls: u32,
}

fn trace(
    p: &BytecodePackage,
    single: bool,
    encoded: bool,
    work: u64,
    deadline: bool,
    expected: bool,
) -> Vec<Observation> {
    let program = verify(p.clone(), Limits::default()).unwrap();
    let program = if encoded {
        VerifiedProgram::decode(
            program.encode(Limits::default()).unwrap().as_bytes(),
            Limits::default(),
        )
        .unwrap()
    } else {
        program
    };
    let mut vm = Vm::load(
        program,
        RuntimeSupport::default(),
        VmLimits::default(),
        SessionEpoch(81),
    )
    .unwrap();
    vm.work_probe.single_equality_step = single;
    let task = vm
        .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
        .unwrap();
    let mut budget = SliceBudget {
        instructions: 1,
        work,
        allocation_bytes: 0,
        deadline_ns: deadline.then_some(1),
    };
    let mut observations = Vec::new();
    for _ in 0..10000 {
        let mut clock_calls = 0;
        let outcome = vm
            .run_slice(task, budget, || {
                let now = u64::from(clock_calls != 0);
                clock_calls += 1;
                now
            })
            .unwrap();
        let roots = vm.active.equality_roots.map(|range| {
            let len = (vm.active.equality_cursors.data.len().max(1) * 2) as u32;
            (0..len)
                .map(|index| vm.slots.read(at(range, index)).ok())
                .collect::<Vec<_>>()
        });
        observations.push(Observation {
            usage: vm.last_usage,
            frames: postcard::to_allocvec(&vm.active.frames.data).unwrap(),
            cursors: vm.active.equality_cursors.data.clone(),
            roots: postcard::to_allocvec(&roots).unwrap(),
            clock_calls,
        });
        match outcome {
            RunOutcome::BudgetYield(usage) => {
                budget.work = work.max(usage.required_work);
                budget.allocation_bytes = usage.required_allocation;
                gc(&mut vm);
            }
            RunOutcome::Returned(delivery) => {
                assert_eq!(
                    vm.delivery_value(delivery).unwrap().as_bool(),
                    Some(expected)
                );
                assert!(vm.active.equality_cursors.data.is_empty());
                return observations;
            }
            other => panic!("unexpected comparison outcome {other:?}"),
        }
    }
    panic!("comparison did not make bounded progress");
}

#[test]
fn byte_batches_preserve_cursors_roots_work_and_clock_yields() {
    let text = "中文🙂".repeat(12);
    let mut cases = vec![
        (Const::Utf8(text.clone()), Const::Utf8(text.clone()), true),
        (Const::Bytes(vec![]), Const::Bytes(vec![]), true),
        (
            Const::Utf8(text.clone()),
            Const::Utf8("short".into()),
            false,
        ),
    ];
    for mismatch in [0, 1, 30, 31, 32, 63, text.len() - 1] {
        let mut different = text.clone().into_bytes();
        different[mismatch] ^= 1;
        cases.push((
            Const::Bytes(text.as_bytes().into()),
            Const::Bytes(different),
            false,
        ));
    }
    for (left, right, equal) in cases {
        for negated in [false, true] {
            let p = comparison(left.clone(), right.clone(), negated);
            for encoded in [false, true] {
                for work in [1, 7, 31, 32, 64, 4096] {
                    for deadline in [false, true] {
                        assert_eq!(
                            trace(&p, false, encoded, work, deadline, equal != negated),
                            trace(&p, true, encoded, work, deadline, equal != negated),
                            "encoded={encoded} work={work} deadline={deadline} negated={negated}"
                        );
                    }
                }
            }
        }
    }
    // The same tuple still traverses its children; nested NaN remains unequal.
    let mut p = comparison(Const::Utf8(text.clone()), Const::Utf8(text), false);
    p.constants.extend([
        Const::FloatBits(f64::NAN.to_bits()),
        Const::Tuple(vec![ConstId(0), ConstId(2)]),
        Const::Tuple(vec![ConstId(1), ConstId(2)]),
    ]);
    p.functions[0].code[0].operands[1] = 3;
    p.functions[0].code[1].operands[1] = 4;
    for deadline in [false, true] {
        assert_eq!(
            trace(&p, false, true, 31, deadline, false),
            trace(&p, true, true, 31, deadline, false)
        );
    }
}

fn pause(vm: &mut Vm, task: TaskId) -> ScriptFrame {
    for _ in 0..10000 {
        if let Some(Frame::Script(frame)) = vm.active.frames.data.last().copied()
            && matches!(frame.phase, ScriptPhase::Equality { .. })
            && vm
                .active
                .equality_cursors
                .data
                .first()
                .copied()
                .unwrap_or(0)
                > 0
        {
            return frame;
        }
        assert!(matches!(tick(vm, task), RunOutcome::BudgetYield(_)));
    }
    panic!("long comparison must yield with a retained cursor");
}

#[test]
fn byte_batch_respects_segment_work_and_existing_clock_batch_bounds() {
    for mutation in [1, 7, 4096] {
        for work in [1, 7, 64] {
            for clock in [None, Some(0), Some(3), Some(31)] {
                let text = "x".repeat(1024);
                let mut limits = VmLimits::default();
                limits.heap.max_mutation_bytes = mutation;
                let mut vm = load_with_limits(
                    comparison(Const::Utf8(text.clone()), Const::Utf8(text), false),
                    limits,
                );
                let task = vm
                    .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
                    .unwrap();
                let mut frame = pause(&mut vm, task);
                let before = vm.active.equality_cursors.data[0];
                let mut meter = Meter::new(SliceBudget {
                    work,
                    deadline_ns: clock.map(|_| u64::MAX),
                    ..SliceBudget::default()
                });
                meter.clock_steps = clock.unwrap_or(0);
                assert!(meter.take(0, 1, 0));
                vm.equality_step(&mut frame, Reg(2), false, &mut meter)
                    .unwrap();
                let count = u64::from(mutation)
                    .min(work)
                    .min(32)
                    .min(clock.map_or(32, |n| u64::from(n) + 1));
                assert_eq!(vm.active.equality_cursors.data[0] - before, count as u32);
                assert_eq!(meter.usage.work, count);
                assert_eq!(
                    meter.clock_steps,
                    clock.map_or(0, |n| n - (count - 1) as u8)
                );
            }
        }
    }
}

#[test]
fn partial_byte_comparison_restores_its_existing_snapshot_cursor() {
    for bytes in [false, true] {
        let text = "中文🙂".repeat(256);
        let value = if bytes {
            Const::Bytes(text.into_bytes())
        } else {
            Const::Utf8(text)
        };
        let mut vm = load(comparison(value.clone(), value, false));
        let task = vm
            .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
            .unwrap();
        pause(&mut vm, task);
        let cursor = vm.active.equality_cursors.data.clone();
        let registry = SnapshotRegistry::default();
        let mut capture = vm.snapshot_begin(&registry).unwrap();
        while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let image = VmSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
        let mut restore = Vm::restore_begin(
            &image,
            vm.program().clone(),
            RuntimeSupport::default(),
            VmLimits::default(),
            SessionEpoch(82),
            &registry,
        )
        .unwrap();
        while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let (mut restored, map) = restore.finish().unwrap();
        assert_eq!(restored.active.equality_cursors.data, cursor);
        assert_eq!(
            returned(&mut restored, map.task(task).unwrap()).as_bool(),
            Some(true)
        );
    }
}
