//! Compare bounded borrowed execution with checked one-instruction execution.
use super::*;
use crate::snapshot::{SnapshotBudget, SnapshotRegistry, VmSnapshot};

fn op(opcode: Opcode, a: u32, b: u32, c: u32) -> Instruction {
    Instruction::new(opcode, a, b, c)
}

fn package(case: u8) -> BytecodePackage {
    let mut p = fixtures::helper_native();
    p.natives.clear();
    p.capabilities.clear();
    p.signatures.truncate(1);
    p.signatures[0].parameters.clear();
    p.signatures[0].return_type = TypeId(0);
    p.functions.truncate(1);
    p.constants = vec![Const::Int(0), Const::Int(1), Const::Int(37), Const::Null];
    let mut code = match case {
        1 => vec![
            op(Opcode::LoadConst, 0, 0, 0),
            op(Opcode::LoadConst, 1, 1, 0),
            op(Opcode::LoadConst, 2, 2, 0),
            op(Opcode::Add, 0, 0, 1),
            op(Opcode::Lt, 3, 0, 2),
            op(Opcode::BranchBool, 3, 3, 6),
            op(Opcode::LoadConst, 4, 3, 0),
            op(Opcode::BranchNull, 4, 9, 8),
            op(Opcode::Return, 1, 0, 0),
            op(Opcode::Jump, 10, 0, 0),
            op(Opcode::Return, 0, 0, 0),
        ],
        2 => vec![op(Opcode::Move, 0, 1, 0), op(Opcode::Return, 0, 0, 0)],
        3 => vec![
            op(Opcode::LoadConst, 0, 1, 0),
            op(Opcode::LoadConst, 1, 0, 0),
            op(Opcode::Div, 0, 0, 1),
            op(Opcode::Return, 0, 0, 0),
        ],
        4 => vec![
            op(Opcode::LoadConst, 0, 1, 0),
            op(Opcode::BranchBool, 0, 2, 2),
            op(Opcode::Return, 0, 0, 0),
        ],
        5 => {
            p.constants.push(Const::Utf8("rooted text".repeat(100)));
            vec![
                op(Opcode::LoadConst, 0, 4, 0),
                op(Opcode::Move, 1, 0, 0),
                op(Opcode::Clear, 0, 0, 0),
                op(Opcode::Return, 1, 0, 0),
            ]
        }
        _ => vec![
            op(Opcode::LoadConst, 0, 2, 0),
            op(Opcode::LoadConst, 1, 1, 0),
        ],
    };
    if case == 0 {
        for _ in 0..48 {
            code.extend([
                op(Opcode::Add, 2, 0, 1),
                op(Opcode::Neg, 3, 2, 0),
                op(Opcode::Mul, 4, 2, 1),
                op(Opcode::BitOr, 5, 3, 4),
                op(Opcode::Clear3, 2, 3, 4),
            ]);
        }
        code.push(op(Opcode::Return, 0, 0, 0));
    }
    let f = &mut p.functions[0];
    f.signature = SignatureId(0);
    f.register_count = 6;
    f.bindings.clear();
    f.descriptors.clear();
    f.regions.clear();
    f.source_map = (0..code.len())
        .map(|pc| SourceMapEntry {
            pc: Pc(pc as u32),
            span: SourceSpan {
                file: SourceId(0),
                start: 0,
                end: 1,
            },
        })
        .collect();
    f.code = code;
    p
}

fn load(case: u8, encoded: bool, reference: bool) -> Vm {
    let program = verify(package(case), Limits::default()).unwrap();
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
        SessionEpoch(56),
    )
    .unwrap();
    vm.work_probe.single_scalar_step = reference;
    vm
}

#[derive(Debug, PartialEq)]
struct Observation {
    usage: SliceUsage,
    frames: Vec<u8>,
    values: Vec<u8>,
    clock_calls: u32,
    caches: Vec<Cache>,
}

type Cache = (u32, u32, Option<u32>, Option<Instruction>);

fn trace(
    case: u8,
    encoded: bool,
    reference: bool,
    work: u64,
    instructions: u32,
    deadline: bool,
) -> Vec<Observation> {
    let mut vm = load(case, encoded, reference);
    let task = vm
        .start(FunctionId(0), &[], RootOwner(1), Context::Ordinary)
        .unwrap();
    let mut budget = SliceBudget {
        instructions,
        work,
        allocation_bytes: 0,
        deadline_ns: deadline.then_some(1),
    };
    let mut trace = Vec::new();
    for _ in 0..10000 {
        let mut clock_calls = 0;
        let outcome = vm
            .run_slice(task, budget, || {
                let now = u64::from(clock_calls != 0);
                clock_calls += 1;
                now
            })
            .unwrap();
        let mut values = Vec::new();
        let mut caches = Vec::new();
        for frame in &vm.active.frames.data {
            let Frame::Script(frame) = frame else {
                unreachable!()
            };
            caches.push((
                frame.instruction_end.0,
                frame.instruction_pc.0,
                frame.member_symbol.map(|s| s.0),
                frame.instruction,
            ));
            for range in [frame.regs, frame.env, frame.args, frame.bound] {
                values.push(
                    (0..vm.slots.len(range).unwrap())
                        .map(|index| vm.slots.read(at(range, index)).ok())
                        .collect::<Vec<_>>(),
                );
            }
        }
        trace.push(Observation {
            usage: vm.last_usage,
            frames: postcard::to_allocvec(&vm.active.frames.data).unwrap(),
            values: postcard::to_allocvec(&values).unwrap(),
            clock_calls,
            caches,
        });
        match outcome {
            RunOutcome::BudgetYield(usage) => {
                budget.work = work.max(usage.required_work);
                budget.allocation_bytes = usage.required_allocation;
                review_tests::gc(&mut vm);
            }
            RunOutcome::Returned(delivery) => {
                let value = vm.delivery_value(delivery).unwrap();
                if case == 5 {
                    assert_eq!(vm.heap.info(value).unwrap().len, 1100);
                } else {
                    assert_eq!(value.as_int(), Some(37));
                }
                return trace;
            }
            RunOutcome::Fault(delivery) => {
                let kind = vm.delivery_fault(delivery).unwrap().unwrap().kind;
                assert_eq!(
                    kind,
                    match case {
                        2 => FaultKind::Uninitialized,
                        3 => FaultKind::DivisionByZero,
                        4 => FaultKind::Type,
                        _ => panic!("unexpected fault {kind:?}"),
                    }
                );
                return trace;
            }
            other => panic!("unexpected scalar outcome {other:?}"),
        }
    }
    panic!("scalar trace did not progress");
}

#[test]
fn borrowed_blocks_preserve_each_budget_clock_and_fault_boundary() {
    for encoded in [false, true] {
        for case in 0..6 {
            for work in [1, 3, 7, 31, 32, 64, 4096] {
                for instructions in [1, 7, 64] {
                    for deadline in [false, true] {
                        assert_eq!(
                            trace(case, encoded, false, work, instructions, deadline),
                            trace(case, encoded, true, work, instructions, deadline),
                            "case={case} encoded={encoded} work={work} instructions={instructions} deadline={deadline}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn published_block_cursor_restores_and_observes_cancellation() {
    for cancel in [false, true] {
        let mut vm = load(1, true, false);
        let task = vm
            .start(FunctionId(0), &[], RootOwner(1), Context::Ordinary)
            .unwrap();
        let budget = SliceBudget {
            instructions: 7,
            work: 512,
            allocation_bytes: 64 << 10,
            deadline_ns: None,
        };
        for _ in 0..100 {
            assert!(matches!(
                vm.run_slice(task, budget, || 0).unwrap(),
                RunOutcome::BudgetYield(_)
            ));
            if matches!(vm.active.frames.data.last(), Some(Frame::Script(f)) if matches!(f.phase, ScriptPhase::Body) && f.pc.0 != 0)
            {
                break;
            }
        }
        let registry = SnapshotRegistry::default();
        let mut capture = vm.snapshot_begin(&registry).unwrap();
        while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let image = VmSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
        let mut restore = Vm::restore_begin(
            &image,
            vm.program().clone(),
            RuntimeSupport::default(),
            VmLimits::default(),
            SessionEpoch(57),
            &registry,
        )
        .unwrap();
        while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let (mut vm, map) = restore.finish().unwrap();
        let task = map.task(task).unwrap();
        if cancel {
            vm.cancel_cooperatively(task).unwrap();
        }
        loop {
            match vm.run_slice(task, budget, || 0).unwrap() {
                RunOutcome::BudgetYield(_) => {}
                RunOutcome::Returned(delivery) => {
                    assert!(!cancel);
                    assert_eq!(vm.delivery_value(delivery).unwrap().as_int(), Some(37));
                    break;
                }
                RunOutcome::Fault(delivery) => {
                    assert!(cancel);
                    assert_eq!(
                        vm.delivery_fault(delivery).unwrap().unwrap().kind,
                        FaultKind::Cancelled
                    );
                    break;
                }
                other => panic!("unexpected restored scalar outcome {other:?}"),
            }
        }
    }
}
