use super::*;
use crate::snapshot::{SnapshotBudget, SnapshotRegistry};

fn package(constants: Vec<Const>, selected: usize) -> BytecodePackage {
    let mut package = fixtures::helper_native();
    package.natives.clear();
    package.capabilities.clear();
    package.signatures.truncate(1);
    package.signatures[0].parameters.clear();
    package.signatures[0].return_type = TypeId(0);
    package.functions.truncate(1);
    package.constants = constants;
    let function = &mut package.functions[0];
    function.signature = SignatureId(0);
    function.register_count = 1;
    function.descriptors.clear();
    function.source_map.clear();
    function.code = vec![
        Instruction::new(Opcode::LoadConst, 0, selected as u32, 0),
        Instruction::new(Opcode::Return, 0, 0, 0),
    ];
    package
}

fn support() -> RuntimeSupport {
    RuntimeSupport {
        host_types: Vec::new(),
        opcodes: vec![],
        capabilities: vec![],
        natives: vec![],
    }
}

fn collect(vm: &mut Vm) {
    while vm.heap.phase() != GcPhase::Idle {
        vm.gc_step(GcBudget::default()).unwrap();
    }
    vm.begin_collection().unwrap();
    while vm.heap.phase() != GcPhase::Idle {
        vm.gc_step(GcBudget::default()).unwrap();
    }
}

fn tick(vm: &mut Vm, task: TaskId) -> RunOutcome {
    let result = vm
        .run_slice(
            task,
            SliceBudget {
                instructions: 1,
                work: vm.last_usage.required_work.max(8),
                allocation_bytes: vm.last_usage.required_allocation.max(VALUE_CHUNK_BYTES),
                ..SliceBudget::default()
            },
            || 0,
        )
        .unwrap();
    if vm.heap.phase() == GcPhase::Idle {
        vm.begin_collection().unwrap();
    }
    vm.gc_step(GcBudget {
        units: 1,
        ..GcBudget::default()
    })
    .unwrap();
    result
}

fn finish(vm: &mut Vm, task: TaskId) -> DeliveryId {
    for _ in 0..20_000 {
        match tick(vm, task) {
            RunOutcome::Returned(delivery) => return delivery,
            RunOutcome::BudgetYield(_) => (),
            result => panic!("{result:?}"),
        }
    }
    panic!("constant construction did not finish")
}

#[test]
fn resident_text_is_shared_on_demand_and_unrooted_literals_are_collected() {
    for external in [false, true] {
        let constants = (0..4096)
            .map(|i| Const::Utf8(format!("{i}:{}", "正文😀".repeat(32))))
            .collect();
        let mut package = package(constants, 4095);
        let pool = external.then(|| externalize_text(&mut package).unwrap());
        let body_bytes = pool.as_ref().map_or_else(
            || {
                package
                    .constants
                    .iter()
                    .map(|c| if let Const::Utf8(s) = c { s.len() } else { 0 })
                    .sum()
            },
            TextPool::len,
        );
        let encoded_bytes = encode(&package, Limits::default()).unwrap().len();
        let mut program = verify(package, Limits::default()).unwrap();
        if let Some(pool) = pool {
            program = program.bind_text_pool(pool).unwrap();
        }
        let mut vm = Vm::load(program, support(), VmLimits::default(), SessionEpoch(1)).unwrap();
        assert_eq!(vm.heap.stats().live_objects, 0);
        assert_eq!(vm.heap.stats().payload_bytes, 0);
        let roots = vm.slots.stats();
        let task = vm
            .start(FunctionId(0), &[], RootOwner(1), Context::Ordinary)
            .unwrap();
        let chunks = vm.heap.stats().chunk_allocations;
        let delivery = finish(&mut vm, task);
        let value = vm.delivery_value(delivery).unwrap();
        assert_eq!(vm.heap.info(value).unwrap().kind, ObjectKind::String);
        assert_eq!(
            vm.heap.stats().chunk_allocations,
            chunks,
            "literal copied into byte chunks"
        );
        assert_eq!(
            vm.heap.byte_segment(value, 0).unwrap(),
            format!("4095:{}", "正文😀".repeat(32)).as_bytes()
        );
        println!(
            "external={external} text_bytes={body_bytes} asb_bytes={encoded_bytes} heap_payload={} weak_cache_bytes={} slots={roots:?}",
            vm.heap.stats().payload_bytes,
            vm.constant_cache.data.capacity() * std::mem::size_of::<Value>()
        );
        let registry = SnapshotRegistry::default();
        let mut capture = vm.snapshot_begin(&registry).unwrap();
        while !capture.step(SnapshotBudget::default()).unwrap().complete {}
        let snapshot = capture.finish().unwrap();
        assert!(
            snapshot.recorded_bytes() < 32 * 1024,
            "snapshot serialized the text pool"
        );
        let mut restore = Vm::restore_begin(
            &snapshot,
            vm.program().clone(),
            support(),
            VmLimits::default(),
            SessionEpoch(2),
            &registry,
        )
        .unwrap();
        while !restore.step(SnapshotBudget::default()).unwrap().complete {}
        let (restored, map) = restore.finish().unwrap();
        let restored_value = restored
            .delivery_value(map.delivery(delivery).unwrap())
            .unwrap();
        assert_eq!(
            restored.heap.stats().payload_bytes,
            0,
            "restoration copied constant text"
        );
        assert_eq!(
            restored.heap.byte_segment(restored_value, 0).unwrap(),
            vm.heap.byte_segment(value, 0).unwrap()
        );
        vm.discard_delivery(delivery).unwrap();
        collect(&mut vm);
        assert_eq!(
            vm.heap.stats().live_objects,
            0,
            "weak literal cache retained heap objects"
        );
        assert!(vm.heap.info(value).is_err());
        let task = vm
            .start(FunctionId(0), &[], RootOwner(1), Context::Ordinary)
            .unwrap();
        let delivery = finish(&mut vm, task);
        let again = vm.delivery_value(delivery).unwrap();
        assert!(
            !again.same_bits(value),
            "stale full-generation cache handle reused"
        );
        assert_eq!(vm.heap.info(again).unwrap().len, 325);
    }
}

#[test]
fn nested_tuple_dag_resumes_after_snapshot_with_gc_and_releases_on_cancel() {
    let mut constants = vec![Const::Utf8("小さな物語😀".into())];
    for id in 0..80 {
        constants.push(Const::Tuple(vec![ConstId(id), ConstId(id)]));
    }
    let mut package = package(constants, 80);
    let pool = externalize_text(&mut package).unwrap();
    let program = verify(package, Limits::default())
        .unwrap()
        .bind_text_pool(pool)
        .unwrap();
    let mut vm = Vm::load(
        program.clone(),
        support(),
        VmLimits::default(),
        SessionEpoch(1),
    )
    .unwrap();
    let task = vm
        .start(FunctionId(0), &[], RootOwner(1), Context::Ordinary)
        .unwrap();
    for _ in 0..500 {
        if matches!(vm.active.frames.data.last(), Some(Frame::Script(frame))
            if matches!(frame.phase, ScriptPhase::Constant(ConstantWork { id: ConstId(20), index: 1, .. })))
        {
            break;
        }
        assert!(matches!(tick(&mut vm, task), RunOutcome::BudgetYield(_)));
    }
    assert!(
        matches!(vm.active.frames.data.last(), Some(Frame::Script(frame))
        if matches!(frame.phase, ScriptPhase::Constant(ConstantWork { id: ConstId(20), index: 1, .. })))
    );
    let registry = SnapshotRegistry::default();
    let mut capture = vm.snapshot_begin(&registry).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let image = capture.finish().unwrap();
    let mut restore = Vm::restore_begin(
        &image,
        program,
        support(),
        VmLimits::default(),
        SessionEpoch(2),
        &registry,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let (mut restored, map) = restore.finish().unwrap();
    let task = map.task(task).unwrap();
    let delivery = finish(&mut restored, task);
    let mut value = restored.delivery_value(delivery).unwrap();
    for _ in 0..80 {
        assert_eq!(restored.heap.info(value).unwrap().kind, ObjectKind::Tuple);
        assert_eq!(restored.heap.info(value).unwrap().len, 2);
        value = restored.heap.get_field(value, 0).unwrap();
    }
    assert_eq!(
        restored.heap.byte_segment(value, 0).unwrap(),
        "小さな物語😀".as_bytes()
    );
    assert!(
        restored.heap.stats().live_objects < 200,
        "tuple DAG expanded as an exponential tree"
    );
    restored.discard_delivery(delivery).unwrap();
    collect(&mut restored);
    assert_eq!(restored.heap.stats().live_objects, 0);

    vm.cancel_task(vm.task_id().unwrap()).unwrap();
    let task = vm.task_id().unwrap();
    loop {
        match vm.run_slice(task, SliceBudget::default(), || 0) {
            Err(VmError::Idle) => break,
            Ok(RunOutcome::BudgetYield(_)) => (),
            result => panic!("{result:?}"),
        }
    }
    collect(&mut vm);
    assert_eq!(vm.heap.stats().live_objects, 0);
}

#[test]
fn external_text_binding_checks_identity_range_and_code_point_boundaries() {
    let mut package = package(vec![Const::Utf8("正文😀".into())], 0);
    let pool = externalize_text(&mut package).unwrap();
    assert_eq!(pool.as_bytes(), "正文😀".as_bytes());
    let encoded = encode(&package, Limits::default()).unwrap();
    assert_eq!(
        decode(&encoded, Limits::default())
            .unwrap()
            .to_unpacked()
            .unwrap(),
        package
    );
    let program = VerifiedProgram::decode(&encoded, Limits::default()).unwrap();
    assert!(
        matches!(Vm::load(program.clone(), support(), VmLimits::default(), SessionEpoch(1)), Err(VmError::Format(error)) if error.detail.contains("external text pool"))
    );
    let wrong = TextPool::from_bytes("違う😀".as_bytes().to_vec()).unwrap();
    assert!(
        program
            .clone()
            .bind_text_pool(wrong)
            .unwrap_err()
            .detail
            .contains("identity")
    );
    assert!(program.bind_text_pool(pool.clone()).is_ok());
    package.constants[0] = Const::Text { offset: 1, len: 2 };
    assert_eq!(
        verify(package.clone(), Limits::default())
            .unwrap()
            .bind_text_pool(pool)
            .unwrap_err()
            .kind,
        ErrorKind::Encoding
    );
    package.constants[0] = Const::Text {
        offset: u64::MAX,
        len: 2,
    };
    assert_eq!(
        verify(package, Limits::default()).unwrap_err().kind,
        ErrorKind::Index
    );
    assert!(TextPool::from_bytes(vec![0xff]).is_err());
}

#[test]
fn external_text_pool_preserves_backing_and_accounts_retained_capacity() {
    let mut retained = None;
    let mut expected = 0;
    let allocation = allocation_counter::measure(|| {
        let mut bytes = Vec::with_capacity(64 * 1024);
        bytes.extend_from_slice("正文😀".as_bytes());
        let pointer = bytes.as_ptr();
        expected =
            bytes.capacity() + std::mem::size_of::<Vec<u8>>() + 2 * std::mem::size_of::<usize>();
        let pool = TextPool::from_bytes(bytes).unwrap();
        assert_eq!(pool.as_bytes().as_ptr(), pointer);
        assert_eq!(pool.capacity_bytes(), expected);
        retained = Some(pool);
    });
    assert_eq!(allocation.count_total, 2);
    assert_eq!(allocation.bytes_total, expected as u64);
    assert_eq!(allocation.bytes_current, expected as i64);
    assert_eq!(allocation.bytes_max, expected as u64);

    let pool = retained.unwrap();
    let allocation = allocation_counter::measure(|| {
        let shared = pool.clone();
        assert_eq!(shared.as_bytes().as_ptr(), pool.as_bytes().as_ptr());
    });
    assert_eq!(allocation.count_total, 0);

    let mut package = package(
        vec![Const::Text {
            offset: 0,
            len: pool.len() as u32,
        }],
        0,
    );
    package.text_pool = Some(pool.descriptor());
    let program = verify(package.clone(), Limits::default()).unwrap();
    let base_capacity = program.stats().owned_capacity_bytes;
    let execution_capacity = program.execution_capacity_bytes();
    let bound = program.bind_text_pool(pool.clone()).unwrap();
    assert_eq!(bound.stats().owned_capacity_bytes, base_capacity + expected);
    assert_eq!(
        bound.execution_capacity_bytes(),
        execution_capacity + expected
    );
    assert_eq!(
        bound
            .bind_text_pool(pool.clone())
            .unwrap()
            .execution_capacity_bytes(),
        execution_capacity + expected
    );

    let limits = Limits {
        max_allocation_bytes: execution_capacity + expected - 1,
        ..Limits::default()
    };
    assert_eq!(
        verify(package.clone(), limits)
            .unwrap()
            .bind_text_pool(pool.clone())
            .unwrap_err()
            .kind,
        ErrorKind::Limit
    );
    let limits = Limits {
        max_allocation_bytes: execution_capacity + expected,
        ..limits
    };
    assert!(
        verify(package.clone(), limits)
            .unwrap()
            .bind_text_pool(pool)
            .is_ok()
    );
}

fn default_package(
    constants: Vec<Const>,
    selected: usize,
    native: bool,
) -> (BytecodePackage, RuntimeSupport) {
    let mut package = package(constants, selected);
    let signature = &mut package.signatures[0];
    signature.parameters = vec![Parameter {
        name: SymbolId(4),
        ty: TypeId(0),
        mutable: false,
        default: Some(ParameterDefault::Constant(ConstId(selected as u32))),
    }];
    let function = &mut package.functions[0];
    function.code = vec![Instruction::new(Opcode::Return, 0, 0, 0)];
    function.bindings = vec![BindingDef {
        name: SymbolId(4),
        ty: TypeId(0),
        mutable: false,
        storage: BindingStorage::Value(Reg(0)),
        initialization: BindingInitialization::Parameter(0),
    }];
    let mut registered = support();
    if native {
        function.kind = FunctionKind::Native(NativeId(0));
        function.name = SymbolId(3);
        function.register_count = 0;
        function.bindings.clear();
        function.code.clear();
        package.natives = vec![NativeDef {
            symbol: SymbolId(3),
            signature: SignatureId(0),
            abi_version: 1,
            capabilities: vec![],
        }];
        registered.natives = vec![NativeSupport {
            symbol: "host::echo".into(),
            abi_version: 1,
            signature: package.signatures[0].clone(),
            required_capabilities: vec![],
        }];
    }
    (package, registered)
}

fn finish_default(vm: &mut Vm, task: TaskId) -> DeliveryId {
    for _ in 0..20000 {
        match tick(vm, task) {
            RunOutcome::Returned(delivery) => return delivery,
            RunOutcome::BudgetYield(_) => {}
            RunOutcome::HostCall(ticket) => {
                let value = vm.host_arguments(ticket).unwrap()[0];
                vm.respond(ticket, HostResponse::Ready(value)).unwrap();
            }
            other => panic!("default construction: {other:?}"),
        }
    }
    panic!("default constant did not settle")
}

fn assert_default_tuple_dag(vm: &Vm, value: Value, shared: bool) {
    let mut pending = vec![(value, 60)];
    let mut seen = std::collections::HashSet::new();
    while let Some((value, depth)) = pending.pop() {
        if !seen.insert((value.object_parts().unwrap(), depth)) {
            continue;
        }
        if depth == 0 {
            assert_eq!(vm.heap.info(value).unwrap().kind, ObjectKind::String);
            assert_eq!(
                vm.heap.byte_segment(value, 0).unwrap(),
                "共用immutable😀".as_bytes()
            );
        } else {
            let info = vm.heap.info(value).unwrap();
            assert_eq!(info.kind, ObjectKind::Tuple);
            assert_eq!(info.len, 2);
            let a = vm.heap.get_field(value, 0).unwrap();
            let b = vm.heap.get_field(value, 1).unwrap();
            if shared {
                assert!(
                    a.same_bits(b),
                    "the live constant cache shares DAG children"
                );
            }
            pending.extend([(a, depth - 1), (b, depth - 1)]);
        }
    }
    assert!(
        seen.len() < 128,
        "tuple DAG expanded as an exponential tree"
    );
    assert!(vm.heap.stats().live_objects < 128);
}

#[test]
fn default_tuple_materialization_shares_the_cursor_through_source_asb_restore_gc_and_cancel() {
    for native in [false, true] {
        for encoded in [false, true] {
            let mut constants = vec![Const::Utf8("共用immutable😀".into())];
            for id in 0..60 {
                constants.push(Const::Tuple(vec![ConstId(id), ConstId(id)]));
            }
            let (mut package, support) = default_package(constants, 60, native);
            let pool = externalize_text(&mut package).unwrap();
            let program = if encoded {
                VerifiedProgram::decode(
                    &encode(&package, Limits::default()).unwrap(),
                    Limits::default(),
                )
                .unwrap()
            } else {
                verify(package, Limits::default()).unwrap()
            }
            .bind_text_pool(pool)
            .unwrap();
            let mut live = Vm::load(
                program.clone(),
                support.clone(),
                VmLimits::default(),
                SessionEpoch(1),
            )
            .unwrap();
            let task = live
                .start(FunctionId(0), &[], RootOwner(1), Context::Ordinary)
                .unwrap();
            let delivery = finish_default(&mut live, task);
            assert_default_tuple_dag(&live, live.delivery_value(delivery).unwrap(), true);
            live.discard_delivery(delivery).unwrap();
            collect(&mut live);
            assert_eq!(live.heap.stats().live_objects, u32::from(native));
            let mut vm = Vm::load(
                program.clone(),
                support.clone(),
                VmLimits::default(),
                SessionEpoch(1),
            )
            .unwrap();
            let task = vm
                .start(FunctionId(0), &[], RootOwner(1), Context::Ordinary)
                .unwrap();
            let ranges = vm.slots.stats().active_ranges;
            let paused = |vm: &Vm| match vm.active.frames.data.last() {
                Some(Frame::Script(frame)) => matches!(
                    frame.phase,
                    ScriptPhase::DefaultConstant {
                        work: ConstantWork {
                            id: ConstId(20),
                            index: 1,
                            ..
                        },
                        ..
                    }
                ),
                Some(Frame::Native(frame)) => matches!(
                    frame.phase,
                    NativePhase::DefaultConstant {
                        work: ConstantWork {
                            id: ConstId(20),
                            index: 1,
                            ..
                        },
                        ..
                    }
                ),
                _ => false,
            };
            for _ in 0..2000 {
                if paused(&vm) {
                    break;
                }
                assert!(matches!(tick(&mut vm, task), RunOutcome::BudgetYield(_)));
            }
            assert!(paused(&vm));
            assert_eq!(
                vm.slots.stats().active_ranges,
                ranges,
                "aggregate defaults reuse the frame's roots"
            );
            let registry = SnapshotRegistry::default();
            let mut capture = vm.snapshot_begin(&registry).unwrap();
            while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
            let image = capture.finish().unwrap();
            let mut restore = Vm::restore_begin(
                &image,
                program,
                support,
                VmLimits::default(),
                SessionEpoch(2),
                &registry,
            )
            .unwrap();
            while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
            let (mut restored, map) = restore.finish().unwrap();
            let delivery = finish_default(&mut restored, map.task(task).unwrap());
            // Restoration rebuilds the weak cache. Tuple identity is not observable;
            // validate every stored child and bound the DAG instead of requiring
            // restored and subsequently materialized equal tuples to share storage.
            assert_default_tuple_dag(&restored, restored.delivery_value(delivery).unwrap(), false);
            restored.discard_delivery(delivery).unwrap();
            collect(&mut restored);
            assert_eq!(restored.heap.stats().live_objects, u32::from(native));
            vm.cancel_task(task).unwrap();
            loop {
                match vm.run_slice(task, SliceBudget::default(), || 0) {
                    Err(VmError::Idle) => break,
                    Ok(RunOutcome::BudgetYield(_)) => {}
                    other => panic!("{other:?}"),
                }
            }
            collect(&mut vm);
            assert_eq!(vm.heap.stats().live_objects, u32::from(native));
        }
    }
}

#[test]
fn constant_default_type_failures_report_the_original_script_or_native_call() {
    for native in [false, true] {
        for aggregate in [false, true] {
            for encoded in [false, true] {
                let constants = if aggregate {
                    vec![Const::Int(7), Const::Tuple(vec![ConstId(0)])]
                } else {
                    vec![Const::Utf8("wrong type".into())]
                };
                let selected = constants.len() - 1;
                let (mut package, mut support) = default_package(constants, selected, native);
                package.signatures[0].parameters[0].ty = TypeId(1);
                if native {
                    support.natives[0].signature = package.signatures[0].clone();
                } else {
                    package.functions[0].bindings[0].ty = TypeId(1);
                }
                let signature = SignatureId(package.signatures.len() as u32);
                package.signatures.push(Signature {
                    parameters: vec![],
                    rest: None,
                    return_type: TypeId(0),
                    effects: Effects::ALL,
                    property_bag_tail: None,
                });
                let source = SourceSpan {
                    file: SourceId(0),
                    start: 12,
                    end: 22,
                };
                let mut caller = package.functions[0].clone();
                caller.name = SymbolId(1);
                caller.kind = FunctionKind::Plain;
                caller.signature = signature;
                caller.register_count = 1;
                caller.bindings.clear();
                caller.source_map = vec![SourceMapEntry {
                    pc: Pc(0),
                    span: source,
                }];
                caller.descriptors = vec![Descriptor::Call(CallDesc {
                    contract: None,
                    target: CallTarget::Function(FunctionId(0)),
                    arguments: Arguments::Window(RegWindow {
                        start: Reg(0),
                        len: 0,
                    }),
                    names: vec![],
                })];
                caller.code = vec![
                    Instruction::new(Opcode::Call, 0, 0, 0),
                    Instruction::new(Opcode::Return, 0, 0, 0),
                ];
                package.functions.push(caller);
                let program = if encoded {
                    VerifiedProgram::decode(
                        &encode(&package, Limits::default()).unwrap(),
                        Limits::default(),
                    )
                    .unwrap()
                } else {
                    verify(package, Limits::default()).unwrap()
                };
                let mut vm =
                    Vm::load(program, support, VmLimits::default(), SessionEpoch(1)).unwrap();
                let task = vm
                    .start(FunctionId(1), &[], RootOwner(1), Context::Ordinary)
                    .unwrap();
                let delivery = loop {
                    match tick(&mut vm, task) {
                        RunOutcome::BudgetYield(_) => {}
                        RunOutcome::Fault(delivery) => break delivery,
                        other => panic!("wrong default type was accepted: {other:?}"),
                    }
                };
                let fault = vm.delivery_fault(delivery).unwrap().unwrap();
                assert_eq!(fault.kind, FaultKind::Type);
                assert_eq!(fault.location.code.function, FunctionId(1));
                assert_eq!(fault.location.pc, Pc(0));
                assert_eq!(fault.location.source, Some(source));
                assert_eq!(vm.native_entries, 0);
                vm.discard_delivery(delivery).unwrap();
                collect(&mut vm);
                assert_eq!(vm.heap.stats().live_objects, u32::from(native));
            }
        }
    }
}

#[test]
fn default_cold_string_prepays_object_storage_and_reuses_the_live_pool_value() {
    let (package, support) = default_package(vec![Const::Utf8("long😀".repeat(4096))], 0, false);
    let program = verify(package, Limits::default()).unwrap();
    let mut vm = Vm::load(program, support, VmLimits::default(), SessionEpoch(1)).unwrap();
    let task = vm
        .start(FunctionId(0), &[], RootOwner(1), Context::Ordinary)
        .unwrap();
    let before = vm.heap.stats();
    let required = loop {
        match vm
            .run_slice(
                task,
                SliceBudget {
                    instructions: 1,
                    work: 10000,
                    allocation_bytes: 0,
                    ..Default::default()
                },
                || 0,
            )
            .unwrap()
        {
            RunOutcome::BudgetYield(usage) if usage.required_allocation > 0 => {
                break usage.required_allocation;
            }
            RunOutcome::BudgetYield(_) => {}
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(vm.heap.stats().live_objects, before.live_objects);
    assert_eq!(vm.heap.stats().charged_bytes, before.charged_bytes);
    assert_eq!(required, vm.constant_allocation_bytes(ConstId(0)));
    let delivery = finish_default(&mut vm, task);
    let value = vm.delivery_value(delivery).unwrap();
    assert_eq!(
        vm.heap.info(value).unwrap().len,
        "long😀".repeat(4096).len() as u32
    );
    assert_eq!(
        vm.heap.stats().payload_bytes,
        0,
        "text borrows the code pool"
    );
    assert_eq!(vm.constant_allocation_bytes(ConstId(0)), 0);
    let task = vm
        .start(FunctionId(0), &[], RootOwner(2), Context::Ordinary)
        .unwrap();
    let second = finish_default(&mut vm, task);
    assert!(vm.delivery_value(second).unwrap().same_bits(value));
}

#[test]
fn restore_rejects_forged_constant_default_phase_and_each_parent_triple() {
    for native in [false, true] {
        for forged in ["phase", "parent_id", "parent_index", "parent_builder"] {
            let constants = vec![
                Const::Int(7),
                Const::Tuple(vec![ConstId(0); 2]),
                Const::Tuple(vec![ConstId(1); 2]),
                Const::Tuple(vec![ConstId(2); 2]),
            ];
            let (package, support) = default_package(constants, 3, native);
            let program = verify(package, Default::default()).unwrap();
            let mut vm = Vm::load(
                program.clone(),
                support.clone(),
                Default::default(),
                SessionEpoch(1),
            )
            .unwrap();
            let task = vm
                .start(FunctionId(0), &[], RootOwner(1), Context::Ordinary)
                .unwrap();
            let mut roots = None;
            for _ in 0..200 {
                roots = match vm.active.frames.data.last() {
                    Some(Frame::Script(frame))
                        if matches!(
                            frame.phase,
                            ScriptPhase::DefaultConstant {
                                work: ConstantWork {
                                    id: ConstId(1),
                                    index: 0,
                                    ..
                                },
                                ..
                            }
                        ) =>
                    {
                        Some((frame.env, [1, 2]))
                    }
                    Some(Frame::Native(frame))
                        if matches!(
                            frame.phase,
                            NativePhase::DefaultConstant {
                                work: ConstantWork {
                                    id: ConstId(1),
                                    index: 0,
                                    ..
                                },
                                ..
                            }
                        ) =>
                    {
                        Some((frame.roots, [0, 1]))
                    }
                    _ => None,
                };
                if roots.is_some() {
                    break;
                }
                assert!(matches!(tick(&mut vm, task), RunOutcome::BudgetYield(_)));
            }
            let (roots, slots) = roots.expect("paused in nested default");
            let parents = vm.value(roots, slots[1]).unwrap();
            match forged {
                "phase" => match vm.active.frames.data.last_mut().unwrap() {
                    Frame::Script(frame) => {
                        if let ScriptPhase::DefaultConstant { parameter, .. } = &mut frame.phase {
                            *parameter = 100;
                        }
                    }
                    Frame::Native(frame) => {
                        if let NativePhase::DefaultConstant { parameter, .. } = &mut frame.phase {
                            *parameter = 100;
                        }
                    }
                },
                "parent_id" => vm.heap.set_field(parents, 1, Value::int(1)).unwrap(),
                "parent_index" => vm.heap.set_field(parents, 2, Value::int(0)).unwrap(),
                "parent_builder" => {
                    let builder = vm.heap.get_field(parents, 0).unwrap();
                    vm.heap.array_push(builder, Value::int(99)).unwrap();
                }
                _ => unreachable!(),
            }
            let registry = SnapshotRegistry::default();
            let mut capture = vm.snapshot_begin(&registry).unwrap();
            while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
            let image = capture.finish().unwrap();
            let mut restore = Vm::restore_begin(
                &image,
                program,
                support,
                Default::default(),
                SessionEpoch(2),
                &registry,
            )
            .unwrap();
            let error = loop {
                match restore.step(SnapshotBudget { units: 1 }) {
                    Ok(progress) => assert!(!progress.complete, "forged {forged} accepted"),
                    Err(error) => break error,
                }
            };
            assert!(
                matches!(error, crate::snapshot::SnapshotError::Invalid(_)),
                "{forged}: {error:?}"
            );
        }
    }
}
