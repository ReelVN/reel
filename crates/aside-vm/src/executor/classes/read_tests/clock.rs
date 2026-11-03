//! Mandatory clock queries retain one window; their observable exits stay exact.
use super::*;

fn bulk_vm(legacy: bool, encoded: bool) -> (Vm, TaskId) {
    let (mut vm, task, _) = loaded(false, encoded, 11, true);
    vm.legacy_body_clock = legacy;
    let instance = object(&vm);
    let class = vm
        .heap
        .get_field(instance, InstanceSlot::Class as u32)
        .unwrap();
    let members = vm.heap.get_field(class, ClassSlot::Members as u32).unwrap();
    for key in (10..24).step_by(2) {
        let member = vm
            .heap
            .alloc_tuple(&[
                Value::int(key),
                class,
                Value::int(0),
                Value::int(0),
                Value::bool(false),
            ])
            .unwrap();
        vm.heap.array_push(members, member).unwrap();
    }
    let target = vm.class_read_plan(instance, SymbolId(4), None).unwrap().0;
    assert_eq!(target.work(), 21);
    (vm, task)
}

#[test]
fn bulk_derived_fields_reuse_the_body_window_at_required_clock_queries() {
    for deadline in [false, true] {
        let run = |legacy| {
            let (mut vm, task) = bulk_vm(legacy, true);
            let mut clocks = 0;
            assert!(matches!(
                vm.run_slice(
                    task,
                    SliceBudget {
                        instructions: u32::MAX,
                        work: u64::MAX,
                        allocation_bytes: u64::MAX,
                        deadline_ns: deadline.then_some(1),
                    },
                    || {
                        clocks += 1;
                        0
                    }
                )
                .unwrap(),
                RunOutcome::Returned(_)
            ));
            assert_eq!(vm.work_probe.body_fields.get(), 2);
            assert_eq!(
                vm.work_probe.scalar_windows.get(),
                if deadline && legacy { 3 } else { 1 }
            );
            assert_eq!(clocks, if deadline { 3 } else { 0 });
            println!(
                "bulk-field legacy={legacy} deadline={deadline} work=42 fields=2 windows={} clock_queries={clocks}",
                vm.work_probe.scalar_windows.get()
            );
            (vm.last_usage, clocks)
        };
        assert_eq!(run(false), run(true));
    }
}

fn trace(
    legacy: bool,
    encoded: bool,
    work: u64,
    instructions: u32,
    expiry: u32,
) -> Vec<Observation> {
    let (mut vm, task) = bulk_vm(legacy, encoded);
    let mut budget = SliceBudget {
        instructions,
        work,
        allocation_bytes: 0,
        deadline_ns: Some(1),
    };
    let mut observations = Vec::new();
    for _ in 0..1000 {
        let mut clocks = 0;
        let outcome = vm
            .run_slice(task, budget, || {
                clocks += 1;
                u64::from(clocks >= expiry)
            })
            .unwrap();
        let values = vm.active.frames.data.last().map(|frame| {
            let Frame::Script(frame) = frame else {
                unreachable!()
            };
            [vm.value(frame.regs, 1).ok(), vm.value(frame.regs, 2).ok()]
        });
        observations.push(Observation {
            usage: vm.last_usage,
            frames: postcard::to_allocvec(&vm.active.frames.data).unwrap(),
            values: postcard::to_allocvec(&values).unwrap(),
            clocks,
        });
        match outcome {
            RunOutcome::BudgetYield(usage) => {
                budget.instructions = instructions.max(1);
                budget.work = work.max(usage.required_work);
                budget.allocation_bytes = usage.required_allocation;
                review_tests::gc(&mut vm);
            }
            RunOutcome::Returned(delivery) => {
                assert_eq!(vm.delivery_value(delivery).unwrap().as_int(), Some(42));
                return observations;
            }
            other => panic!("unexpected clocked Body outcome {other:?}"),
        }
    }
    panic!("clocked Body did not complete");
}

#[test]
fn clocked_body_preserves_every_usage_cursor_root_gc_and_expiry_boundary() {
    for encoded in [false, true] {
        for work in [0, 1, 41, 42, 43, 128] {
            for instructions in [0, 1, 2, 128] {
                for expiry in [2, 3, 1000] {
                    assert_eq!(
                        trace(false, encoded, work, instructions, expiry),
                        trace(true, encoded, work, instructions, expiry),
                        "encoded={encoded} work={work} instructions={instructions} expiry={expiry}"
                    );
                }
            }
        }
    }
}

fn expired_vm() -> (Vm, TaskId) {
    let (mut vm, task) = bulk_vm(false, true);
    let mut clocks = 0;
    assert!(matches!(
        vm.run_slice(
            task,
            SliceBudget {
                deadline_ns: Some(1),
                ..SliceBudget::default()
            },
            || {
                clocks += 1;
                u64::from(clocks >= 2)
            }
        )
        .unwrap(),
        RunOutcome::BudgetYield(_)
    ));
    assert_eq!(clocks, 2);
    let Some(Frame::Script(frame)) = vm.active.frames.data.last() else {
        unreachable!()
    };
    assert_eq!(frame.pc, Pc(2));
    assert_eq!(vm.value(frame.regs, 1).unwrap().as_int(), Some(42));
    (vm, task)
}

#[test]
fn an_expired_borrow_restores_and_observes_hard_and_cooperative_cancellation() {
    for cancel in [None, Some(false), Some(true)] {
        let (vm, task) = expired_vm();
        let registry = SnapshotRegistry::default();
        let mut capture = vm.snapshot_begin(&registry).unwrap();
        while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let image = VmSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
        let mut restore = Vm::restore_begin(
            &image,
            vm.program().clone(),
            RuntimeSupport::default(),
            VmLimits::default(),
            SessionEpoch(74),
            &registry,
        )
        .unwrap();
        while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let (mut vm, map) = restore.finish().unwrap();
        let task = map.task(task).unwrap();
        match cancel {
            Some(true) => vm.cancel_task(task).unwrap(),
            Some(false) => vm.cancel_cooperatively(task).unwrap(),
            None => {}
        }
        if cancel == Some(true) {
            loop {
                match vm.run_slice(task, SliceBudget::default(), || 0) {
                    Ok(RunOutcome::BudgetYield(_)) => {}
                    Err(VmError::Idle) => break,
                    other => panic!("unexpected hard-cancel result {other:?}"),
                }
            }
        } else {
            assert_eq!(
                finish(&mut vm, task),
                cancel.map_or(Ok(42), |_| Err(FaultKind::Cancelled))
            );
        }
    }
}

#[test]
fn a_panicking_clock_sees_the_same_committed_cursor_after_unwind() {
    let (mut vm, task) = bulk_vm(false, true);
    let mut clocks = 0;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        vm.run_slice(
            task,
            SliceBudget {
                deadline_ns: Some(1),
                ..SliceBudget::default()
            },
            || {
                clocks += 1;
                assert!(clocks < 2, "clock unwind after the first field");
                0
            },
        )
    }));
    assert!(result.is_err());
    let Some(Frame::Script(frame)) = vm.active.frames.data.last() else {
        unreachable!()
    };
    assert_eq!(frame.pc, Pc(2));
    assert_eq!(vm.value(frame.regs, 1).unwrap().as_int(), Some(42));
    assert_eq!(finish(&mut vm, task), Ok(42));
}
