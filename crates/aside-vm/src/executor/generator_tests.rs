//! Generator execution uses the ordinary task, GC, budget and snapshot paths.
use super::review_tests::{collect, gc, retry, tiny};
use super::*;

fn op(code: Opcode, a: u32, b: u32, c: u32) -> Instruction {
    Instruction::new(code, a, b, c)
}

fn package(body: Vec<Instruction>, regions: Vec<ExceptionRegion>) -> BytecodePackage {
    let mut p = fixtures::helper_native();
    p.natives.clear();
    p.capabilities.clear();
    p.signatures.truncate(1);
    p.signatures[0].parameters.clear();
    p.signatures[0].return_type = TypeId(0);
    let mut signature = p.signatures[0].clone();
    signature.parameters.push(Parameter {
        name: SymbolId(4),
        ty: TypeId(0),
        mutable: false,
        default: None,
    });
    p.signatures.push(signature);
    let mut main = p.functions[0].clone();
    main.signature = SignatureId(0);
    main.bindings.clear();
    main.source_map.clear();
    main.register_count = 4;
    main.code = vec![op(Opcode::Call, 0, 0, 0), op(Opcode::Return, 0, 0, 0)];
    main.descriptors = vec![Descriptor::Call(CallDesc {
        contract: None,
        target: CallTarget::Function(FunctionId(1)),
        arguments: Arguments::Window(RegWindow {
            start: Reg(0),
            len: 0,
        }),
        names: vec![],
    })];
    let mut generator = main.clone();
    generator.name = SymbolId(2);
    generator.kind = FunctionKind::Generator;
    generator.context = Context::NoWait;
    generator.code = body;
    generator.descriptors.clear();
    generator.regions = regions;
    let mut next = main.clone();
    next.name = SymbolId(7);
    next.signature = SignatureId(1);
    next.descriptors.clear();
    next.code = vec![op(Opcode::IterNext, 1, 0, 0), op(Opcode::Return, 1, 0, 0)];
    let mut close = next.clone();
    close.name = SymbolId(8);
    close.code = vec![
        op(Opcode::IterClose, 0, 0, 0),
        op(Opcode::LoadConst, 1, 0, 0),
        op(Opcode::Return, 1, 0, 0),
    ];
    p.functions = vec![main, generator, next, close];
    p
}

fn load(p: BytecodePackage) -> Vm {
    Vm::load(
        verify(p, Limits::default()).unwrap(),
        RuntimeSupport::default(),
        VmLimits::default(),
        SessionEpoch(97),
    )
    .unwrap()
}

fn run(vm: &mut Vm, task: TaskId) -> RunOutcome {
    let mut budget = tiny();
    for _ in 0..50000 {
        let out = vm.run_slice(task, budget, || 0).unwrap();
        gc(vm);
        match out {
            RunOutcome::BudgetYield(usage) => retry(&mut budget, usage),
            other => return other,
        }
    }
    panic!("generator did not terminate within the bounded driver");
}

fn returned(vm: &mut Vm, task: TaskId) -> Value {
    let RunOutcome::Returned(delivery) = run(vm, task) else {
        panic!("expected returned value")
    };
    let value = vm.delivery_value(delivery).unwrap();
    vm.discard_delivery(delivery).unwrap();
    value
}

fn call(vm: &mut Vm, function: u32, args: &[Value]) -> Value {
    let task = vm
        .start(FunctionId(function), args, RootOwner(9), Context::Ordinary)
        .unwrap();
    returned(vm, task)
}

fn pair(vm: &mut Vm, generator: Value) -> (bool, Value) {
    let value = call(vm, 2, &[generator]);
    (
        vm.heap.sequence_get(value, 0).unwrap().as_bool().unwrap(),
        vm.heap.sequence_get(value, 1).unwrap(),
    )
}

fn simple() -> BytecodePackage {
    package(
        vec![
            op(Opcode::LoadConst, 0, 0, 0),
            op(Opcode::Yield, 1, 0, 0),
            op(Opcode::LoadConst, 0, 1, 0),
            op(Opcode::Yield, 1, 0, 0),
            op(Opcode::LoadConst, 0, 2, 0),
            op(Opcode::Return, 0, 0, 0),
        ],
        vec![],
    )
}

#[test]
fn lazy_null_items_final_result_once_and_repeated_close_share_one_heap() {
    let mut vm = load(simple());
    let generator = call(&mut vm, 0, &[]);
    let root = vm
        .heap
        .add_root(RootOwner(9), RootKind::Host, generator)
        .unwrap();
    assert_eq!(vm.heap.get_field(generator, 0).unwrap().as_int(), Some(0));
    collect(&mut vm);
    let (done, item) = pair(&mut vm, generator);
    assert!(!done);
    assert!(item.same_bits(Value::NULL));
    assert_eq!(pair(&mut vm, generator).1.as_int(), Some(41));
    let (done, final_value) = pair(&mut vm, generator);
    assert!(done);
    assert_eq!(final_value.as_int(), Some(1));
    for _ in 0..3 {
        let (done, value) = pair(&mut vm, generator);
        assert!(done && value.same_bits(Value::NULL));
        assert!(call(&mut vm, 3, &[generator]).same_bits(Value::NULL));
    }
    vm.heap.release_root(root).unwrap();
    collect(&mut vm);
    assert!(vm.heap.info(generator).is_err());
}

#[test]
fn escaped_fault_keeps_its_origin_and_closes_the_generator() {
    let mut p = simple();
    p.functions[1].code[2] = op(Opcode::Throw, 0, 0, 0);
    let mut vm = load(p);
    let generator = call(&mut vm, 0, &[]);
    let root = vm
        .heap
        .add_root(RootOwner(9), RootKind::Host, generator)
        .unwrap();
    pair(&mut vm, generator);
    let task = vm
        .start(FunctionId(2), &[generator], RootOwner(9), Context::Ordinary)
        .unwrap();
    let RunOutcome::Fault(delivery) = run(&mut vm, task) else {
        panic!("expected generator fault")
    };
    let info = vm.delivery_fault(delivery).unwrap().unwrap();
    assert_eq!(info.location.code.function, FunctionId(1));
    assert_eq!(
        info.location.pc,
        test_pc(&vm, info.location.code.function.0, 2)
    );
    vm.discard_delivery(delivery).unwrap();
    let (done, value) = pair(&mut vm, generator);
    assert!(done && value.same_bits(Value::NULL));
    vm.heap.release_root(root).unwrap();
}

#[test]
fn suspended_and_budget_paused_restore_preserve_cursor_and_gc_roots() {
    for pause_copy in [false, true] {
        let mut vm = load(simple());
        let generator = call(&mut vm, 0, &[]);
        let root = vm
            .heap
            .add_root(RootOwner(9), RootKind::Host, generator)
            .unwrap();
        assert!(!pair(&mut vm, generator).0);
        let task = if pause_copy {
            let task = vm
                .start(FunctionId(2), &[generator], RootOwner(9), Context::Ordinary)
                .unwrap();
            super::review_tests::reach(
                &mut vm,
                task,
                |vm| matches!(vm.active.frames.data.last(), Some(Frame::Script(s)) if matches!(s.phase, ScriptPhase::GeneratorRestore { part: 1, index: 1, .. })),
            );
            Some(task)
        } else {
            None
        };
        collect(&mut vm);
        let registry = crate::snapshot::SnapshotRegistry::default();
        let mut capture = vm.snapshot_begin(&registry).unwrap();
        while !capture
            .step(crate::snapshot::SnapshotBudget { units: 1 })
            .unwrap()
            .complete
        {}
        let image = capture.finish().unwrap();
        let image = crate::snapshot::VmSnapshot::decode(&image.encode().unwrap()).unwrap();
        let mut restore = Vm::restore_begin(
            &image,
            vm.program().clone(),
            RuntimeSupport::default(),
            VmLimits::default(),
            SessionEpoch(98),
            &registry,
        )
        .unwrap();
        while !restore
            .step(crate::snapshot::SnapshotBudget { units: 1 })
            .unwrap()
            .complete
        {}
        let (mut restored, map) = restore.finish().unwrap();
        let generator = map.value(generator).unwrap();
        if let Some(task) = task {
            let value = returned(&mut restored, map.task(task).unwrap());
            assert_eq!(
                restored.heap.sequence_get(value, 1).unwrap().as_int(),
                Some(41)
            );
        } else {
            assert_eq!(pair(&mut restored, generator).1.as_int(), Some(41));
        }
        assert_eq!(pair(&mut restored, generator).1.as_int(), Some(1));
        vm.heap.release_root(root).unwrap();
    }
}

#[test]
fn reentry_and_dynamic_helper_await_fail_at_the_actual_instruction() {
    for reentry in [false, true] {
        let mut p = simple();
        p.constants.push(Const::Int(0));
        if reentry {
            p.functions[1].signature = SignatureId(1);
            p.functions[0].signature = SignatureId(1);
            let Descriptor::Call(desc) = &mut p.functions[0].descriptors[0] else {
                unreachable!()
            };
            desc.arguments = Arguments::Window(RegWindow {
                start: Reg(0),
                len: 1,
            });
            p.functions[1].code = vec![
                op(Opcode::LoadConst, 2, 3, 0),
                op(Opcode::IndexGet, 1, 0, 2),
                op(Opcode::IterNext, 3, 1, 0),
                op(Opcode::Return, 3, 0, 0),
            ];
        } else {
            let mut helper = p.functions[0].clone();
            helper.name = SymbolId(9);
            helper.code = vec![
                op(Opcode::LoadConst, 0, 0, 0),
                op(Opcode::Await, 1, 0, 0),
                op(Opcode::Return, 1, 0, 0),
            ];
            helper.descriptors.clear();
            p.functions.push(helper);
            p.functions[1].descriptors = vec![Descriptor::Call(CallDesc {
                contract: None,
                target: CallTarget::Function(FunctionId(4)),
                arguments: Arguments::Window(RegWindow {
                    start: Reg(0),
                    len: 0,
                }),
                names: vec![],
            })];
            p.functions[1].code = vec![op(Opcode::Call, 0, 0, 0), op(Opcode::Return, 0, 0, 0)];
        }
        let mut vm = load(p);
        let self_box = vm.heap.alloc_array(&[Value::NULL]).unwrap();
        let generator = if reentry {
            call(&mut vm, 1, &[self_box])
        } else {
            call(&mut vm, 0, &[])
        };
        vm.heap.set_field(self_box, 0, generator).unwrap();
        let root = vm
            .heap
            .add_root(RootOwner(9), RootKind::Host, generator)
            .unwrap();
        let task = vm
            .start(FunctionId(2), &[generator], RootOwner(9), Context::Ordinary)
            .unwrap();
        let RunOutcome::Fault(delivery) = run(&mut vm, task) else {
            panic!("expected restricted execution fault")
        };
        let info = vm.delivery_fault(delivery).unwrap().unwrap();
        assert_eq!(
            info.kind,
            if reentry {
                FaultKind::InvalidState
            } else {
                FaultKind::Effect
            }
        );
        assert_eq!(
            info.location.code.function,
            FunctionId(if reentry { 1 } else { 4 })
        );
        assert_eq!(
            info.location.pc,
            test_pc(
                &vm,
                info.location.code.function.0,
                if reentry { 2 } else { 1 }
            )
        );
        vm.discard_delivery(delivery).unwrap();
        assert!(pair(&mut vm, generator).0);
        vm.heap.release_root(root).unwrap();
    }
}

#[test]
fn catch_rethrow_survives_yield_gc_and_snapshot_with_original_fault_identity() {
    let mut vm = load(package(
        vec![
            op(Opcode::LoadConst, 0, 1, 0),
            op(Opcode::Throw, 0, 0, 0),
            op(Opcode::Yield, 1, 0, 0),
            op(Opcode::Rethrow, 0, 0, 0),
            op(Opcode::Return, 0, 0, 0),
        ],
        vec![ExceptionRegion {
            protected: PcRange {
                start: Pc(0),
                end: Pc(2),
            },
            catch: Some(CatchRegion {
                range: PcRange {
                    start: Pc(2),
                    end: Pc(4),
                },
                exception: Reg(0),
            }),
            finally: None,
        }],
    ));
    let generator = call(&mut vm, 0, &[]);
    let root = vm
        .heap
        .add_root(RootOwner(9), RootKind::Host, generator)
        .unwrap();
    assert_eq!(pair(&mut vm, generator).1.as_int(), Some(41));
    assert!(vm.active.caught.data.is_empty());
    collect(&mut vm);
    let registry = crate::snapshot::SnapshotRegistry::default();
    let mut capture = vm.snapshot_begin(&registry).unwrap();
    while !capture
        .step(crate::snapshot::SnapshotBudget { units: 1 })
        .unwrap()
        .complete
    {}
    let image = capture.finish().unwrap();
    let mut restore = Vm::restore_begin(
        &image,
        vm.program().clone(),
        RuntimeSupport::default(),
        VmLimits::default(),
        SessionEpoch(98),
        &registry,
    )
    .unwrap();
    while !restore
        .step(crate::snapshot::SnapshotBudget { units: 1 })
        .unwrap()
        .complete
    {}
    let (mut restored, map) = restore.finish().unwrap();
    let generator = map.value(generator).unwrap();
    let task = restored
        .start(FunctionId(2), &[generator], RootOwner(9), Context::Ordinary)
        .unwrap();
    let RunOutcome::Fault(delivery) = run(&mut restored, task) else {
        panic!("expected rethrow")
    };
    let info = restored.delivery_fault(delivery).unwrap().unwrap();
    assert_eq!(info.location.code.function, FunctionId(1));
    assert_eq!(
        info.location.pc,
        test_pc(&vm, info.location.code.function.0, 1)
    );
    assert_eq!(
        restored.delivery_value(delivery).unwrap().as_int(),
        Some(41)
    );
    vm.heap.release_root(root).unwrap();
}

#[test]
fn hard_cancellation_and_unreachable_gc_release_frames_without_running_finally() {
    let mut p = package(
        vec![
            op(Opcode::LoadConst, 1, 2, 0),
            op(Opcode::LoadConst, 2, 3, 0),
            op(Opcode::Yield, 3, 1, 0),
            op(Opcode::Jump, 3, 0, 0),
            op(Opcode::IndexSet, 0, 2, 1),
            op(Opcode::EndFinally, 0, 0, 0),
            op(Opcode::Return, 1, 0, 0),
        ],
        vec![ExceptionRegion {
            protected: PcRange {
                start: Pc(0),
                end: Pc(4),
            },
            catch: None,
            finally: Some(PcRange {
                start: Pc(4),
                end: Pc(6),
            }),
        }],
    );
    p.constants.push(Const::Int(0));
    p.functions[1].signature = SignatureId(1);
    p.functions[0].signature = SignatureId(1);
    let Descriptor::Call(desc) = &mut p.functions[0].descriptors[0] else {
        unreachable!()
    };
    desc.arguments = Arguments::Window(RegWindow {
        start: Reg(0),
        len: 1,
    });
    let mut vm = load(p);
    let log = vm.heap.alloc_array(&[Value::int(0)]).unwrap();
    let log_root = vm.heap.add_root(RootOwner(9), RootKind::Host, log).unwrap();
    let generator = call(&mut vm, 1, &[log]);
    let root = vm
        .heap
        .add_root(RootOwner(9), RootKind::Host, generator)
        .unwrap();
    assert_eq!(pair(&mut vm, generator).1.as_int(), Some(1));
    let task = vm
        .start(FunctionId(2), &[generator], RootOwner(9), Context::Ordinary)
        .unwrap();
    super::review_tests::reach(
        &mut vm,
        task,
        |vm| matches!(vm.active.frames.data.last(), Some(Frame::Script(s)) if s.generator && s.pc == test_pc(vm, s.code.function.0, 3) && matches!(s.phase, ScriptPhase::Body)),
    );
    vm.cancel_task(task).unwrap();
    for _ in 0..2000 {
        match vm.run_slice(task, SliceBudget::default(), || 0) {
            Ok(RunOutcome::BudgetYield(_)) => (),
            Err(VmError::Idle | VmError::UnknownTask) => break,
            other => panic!("unexpected cancellation outcome: {other:?}"),
        }
    }
    assert_eq!(vm.heap.sequence_get(log, 0).unwrap().as_int(), Some(0));
    assert!(pair(&mut vm, generator).0);
    vm.heap.release_root(root).unwrap();
    let abandoned = call(&mut vm, 1, &[log]);
    assert_eq!(pair(&mut vm, abandoned).1.as_int(), Some(1));
    collect(&mut vm);
    assert!(vm.heap.info(abandoned).is_err());
    assert_eq!(vm.heap.sequence_get(log, 0).unwrap().as_int(), Some(0));
    vm.heap.release_root(log_root).unwrap();
}

#[test]
fn generator_and_array_iteration_report_actual_cost_and_reuse_storage() {
    const COUNT: i64 = 10_000;
    for generator_case in [false, true] {
        let mut p = simple();
        p.constants.push(Const::Int(0));
        p.constants.push(Const::Int(COUNT));
        p.functions[1].code = vec![
            op(Opcode::LoadConst, 0, 3, 0),
            op(Opcode::LoadConst, 1, 4, 0),
            op(Opcode::LoadConst, 2, 2, 0),
            op(Opcode::Yield, 3, 0, 0),
            op(Opcode::Add, 0, 0, 2),
            op(Opcode::Lt, 3, 0, 1),
            op(Opcode::BranchBool, 3, 3, 7),
            op(Opcode::Return, 0, 0, 0),
        ];
        p.functions[2].code = vec![
            op(Opcode::LoadConst, 1, 3, 0),
            op(Opcode::IterAdvance, 2, 0, 3),
            op(Opcode::BranchBool, 2, 5, 3),
            op(Opcode::Add, 1, 1, 3),
            op(Opcode::Jump, 1, 0, 0),
            op(Opcode::Return, 1, 0, 0),
        ];
        let mut vm = load(p);
        collect(&mut vm);
        let baseline_bytes = vm.heap.stats().charged_bytes;
        let source = if generator_case {
            call(&mut vm, 0, &[])
        } else {
            let array = vm.heap.alloc_array(&[]).unwrap();
            for index in 0..COUNT {
                vm.heap.array_push(array, Value::int(index)).unwrap();
            }
            vm.iterator_new(array).unwrap()
        };
        let root = vm
            .heap
            .add_root(RootOwner(9), RootKind::Host, source)
            .unwrap();
        collect(&mut vm);
        let initial_bytes = vm.heap.stats().charged_bytes;
        let task = vm
            .start(FunctionId(2), &[source], RootOwner(9), Context::Ordinary)
            .unwrap();
        // Prepare the first code page before measuring steady-state iteration.
        assert!(matches!(
            vm.run_slice(
                task,
                SliceBudget {
                    instructions: 1,
                    ..SliceBudget::default()
                },
                || 0,
            )
            .unwrap(),
            RunOutcome::BudgetYield(_)
        ));
        let mut delivery = None;
        let started = std::time::Instant::now();
        let measured = allocation_counter::measure(|| {
            loop {
                match vm
                    .run_slice(
                        task,
                        SliceBudget {
                            allocation_bytes: 0,
                            ..SliceBudget::default()
                        },
                        || 0,
                    )
                    .unwrap()
                {
                    RunOutcome::BudgetYield(usage) => assert_eq!(usage.required_allocation, 0),
                    RunOutcome::Returned(id) => {
                        delivery = Some(id);
                        break;
                    }
                    other => panic!("iteration failed: {other:?}"),
                }
            }
        });
        let elapsed = started.elapsed();
        assert_eq!(
            vm.delivery_value(delivery.unwrap()).unwrap().as_int(),
            Some(COUNT * (COUNT - 1) / 2)
        );
        assert_eq!(
            measured.count_total, 0,
            "prepared iteration must reuse heap and frame storage"
        );
        println!(
            "kind={} items={COUNT} prepared_heap_bytes={initial_bytes} source_bytes={} loop_allocations={} elapsed={elapsed:?}",
            if generator_case { "generator" } else { "array" },
            initial_bytes - baseline_bytes,
            measured.count_total
        );
        vm.heap.release_root(root).unwrap();
    }
}

#[test]
fn restore_rejects_forged_suspended_bitmap_and_restore_cursor() {
    for bitmap in [false, true] {
        let mut vm = load(simple());
        let generator = call(&mut vm, 0, &[]);
        let _root = vm
            .heap
            .add_root(RootOwner(9), RootKind::Host, generator)
            .unwrap();
        pair(&mut vm, generator);
        if bitmap {
            let registers = vm.heap.get_field(generator, 8).unwrap();
            vm.heap.set_field(registers, 0, Value::int(7)).unwrap();
        } else {
            let task = vm
                .start(FunctionId(2), &[generator], RootOwner(9), Context::Ordinary)
                .unwrap();
            super::review_tests::reach(
                &mut vm,
                task,
                |vm| matches!(vm.active.frames.data.last(), Some(Frame::Script(s)) if matches!(s.phase, ScriptPhase::GeneratorRestore { .. })),
            );
            let Some(Frame::Script(frame)) = vm.active.frames.data.last_mut() else {
                unreachable!()
            };
            frame.phase = ScriptPhase::GeneratorRestore {
                part: 99,
                index: 0,
                closing: false,
            };
        }
        let registry = crate::snapshot::SnapshotRegistry::default();
        let mut capture = vm.snapshot_begin(&registry).unwrap();
        while !capture
            .step(crate::snapshot::SnapshotBudget { units: 1 })
            .unwrap()
            .complete
        {}
        let image = capture.finish().unwrap();
        let mut restore = Vm::restore_begin(
            &image,
            vm.program().clone(),
            RuntimeSupport::default(),
            VmLimits::default(),
            SessionEpoch(98),
            &registry,
        )
        .unwrap();
        let mut rejected = false;
        for _ in 0..50000 {
            match restore.step(crate::snapshot::SnapshotBudget { units: 1 }) {
                Err(crate::snapshot::SnapshotError::Invalid(_)) => {
                    rejected = true;
                    break;
                }
                Err(error) => panic!("unexpected restore rejection: {error:?}"),
                Ok(progress) if progress.complete => break,
                Ok(_) => (),
            }
        }
        assert!(
            rejected,
            "forged generator state must fail candidate admission"
        );
    }
}
