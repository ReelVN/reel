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
    assert_eq!(decode(&encoded, Limits::default()).unwrap(), package);
    let program = VerifiedProgram::decode(&encoded, Limits::default()).unwrap();
    assert!(
        matches!(Vm::load(program.clone(), support(), VmLimits::default(), SessionEpoch(1)), Err(VmError::Format(error)) if error.detail.contains("external text pool"))
    );
    let wrong = TextPool::from_bytes(std::sync::Arc::from("違う😀".as_bytes())).unwrap();
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
    assert!(TextPool::from_bytes(std::sync::Arc::from([0xff])).is_err());
}
