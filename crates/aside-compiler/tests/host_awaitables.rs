//! Executable host protocol: source -> B01 codec -> shared VM native wait frames.
use aside_compiler::{AnalysisOptions, analyze_module, lowering, semantic::NoModules};
use aside_vm::{
    CauseKind, GcBudget, GcPhase, RootKind, RootOwner, SessionEpoch, Value, bytecode as bc,
    executor::*,
};

const SOURCE: &str = r#"
fn helper(request) { await request }
fn main(request) { let read:fn(any)->int=helper; read(request) + read(request) }
fn producer() { throw "producer failed"; }
fn consumer(request) { try { throw 7; } catch previous { await request } }
fn cooperative(request) { try { await request } catch cancelled { return 42; } }
fn repeat(request) { let mut total = 0; for i in 0..128 { total += await request; } total }
class SavedCounter { let mut value:int = 40; fn read(self) { self.value } }
fn saved_state(request) {
    let counter = SavedCounter();
    let aliases = [counter, counter];
    let cycle = []; cycle.append(cycle);
    let mut calls = 0;
    let bump:fn()->int = || { calls += 1; counter.read() + calls };
    bump();
    try { throw 7; } catch caught {
        let read:fn()->int = || await request;
        let result = read();
        aliases[0].value = 39;
        if aliases[1].value != 39 || cycle[0] != cycle || caught != 7 { return 0; }
        counter.value = 40;
        return bump() + result;
    } finally { counter.value += 1; }
}
"#;

fn fixture() -> Vm {
    let module = analyze_module(SOURCE, &AnalysisOptions::default(), &mut NoModules)
        .unwrap()
        .into_module()
        .unwrap();
    let compiled = lowering::compile(
        &module,
        &lowering::Linkage::default(),
        &[],
        lowering::Options::default(),
    )
    .unwrap();
    let mut p = compiled.program.package().clone();
    let any = bc::TypeId(
        p.types
            .iter()
            .position(|t| *t == bc::TypeDef::Builtin(bc::BuiltinType::Any))
            .unwrap() as u32,
    );
    let name = bc::SymbolId(p.symbols.len() as u32);
    p.symbols.push("host::await_request".into());
    let parameter = bc::SymbolId(p.symbols.iter().position(|s| s == "request").unwrap() as u32);
    // This is the fixture host's own declaration, used for both package linkage
    // and independent admission. No untrusted package signature is adopted.
    let sig = bc::Signature {
        parameters: vec![bc::Parameter {
            name: parameter,
            ty: any,
            mutable: false,
            default: None,
        }],
        rest: None,
        return_type: any,
        effects: bc::Effects::ALL,
        property_bag_tail: None,
    };
    let signature = bc::SignatureId(p.signatures.len() as u32);
    p.signatures.push(sig.clone());
    p.natives.push(bc::NativeDef {
        symbol: name,
        signature,
        abi_version: 1,
        capabilities: vec![],
    });
    let encoded = bc::encode(&p, bc::Limits::default()).unwrap();
    let program = bc::VerifiedProgram::decode(&encoded, bc::Limits::default()).unwrap();
    let mut vm = Vm::load(
        program,
        bc::RuntimeSupport {
            host_types: Vec::new(),
            natives: vec![bc::NativeSupport {
                standard_derived_commit: false,
                read_only_leaf: None,
                symbol: "host::await_request".into(),
                abi_version: 1,
                signature: sig,
                required_capabilities: vec![],
            }],
            ..Default::default()
        },
        VmLimits::default(),
        SessionEpoch(1),
    )
    .unwrap();
    let init = vm
        .start(compiled.entry, &[], RootOwner(99), bc::Context::Init)
        .unwrap();
    let RunOutcome::Returned(delivery) = next(&mut vm, init, true) else {
        panic!("initializer")
    };
    vm.discard_delivery(delivery).unwrap();
    vm
}

fn start(vm: &mut Vm, name: &str, args: &[Value], owner: u64, context: bc::Context) -> TaskId {
    let p = vm.program().package();
    let function = bc::FunctionId(
        p.functions
            .iter()
            .position(|f| p.symbols[f.name.index()] == name)
            .unwrap() as u32,
    );
    vm.start(function, args, RootOwner(owner), context).unwrap()
}

fn next(vm: &mut Vm, task: TaskId, tiny: bool) -> RunOutcome {
    let mut budget = if tiny {
        SliceBudget {
            instructions: 1,
            work: 1,
            allocation_bytes: 0,
            deadline_ns: None,
        }
    } else {
        SliceBudget::default()
    };
    loop {
        let outcome = vm.run_slice(task, budget, || 0).unwrap();
        if tiny {
            if vm.heap().phase() == GcPhase::Idle {
                vm.begin_collection().unwrap();
            }
            vm.gc_step(GcBudget {
                units: 1,
                ..Default::default()
            })
            .unwrap();
        }
        match outcome {
            RunOutcome::BudgetYield(usage) => {
                budget.work = usage.required_work.max(1);
                budget.allocation_bytes = usage.required_allocation;
            }
            _ => return outcome,
        }
    }
}

fn request(vm: &mut Vm) -> (Value, aside_vm::RootToken) {
    let fields = vm.heap_mut().alloc_table().unwrap();
    let object = vm.heap_mut().alloc_host(fields, &[], None).unwrap();
    vm.bind_awaitable(object, bc::NativeId(0)).unwrap();
    let root = vm
        .heap_mut()
        .add_root(RootOwner(99), RootKind::Host, object)
        .unwrap();
    (object, root)
}

fn ticket(vm: &mut Vm, task: TaskId, object: Value, tiny: bool) -> HostTicket {
    let RunOutcome::HostCall(ticket) = next(vm, task, tiny) else {
        panic!("await must dispatch its adapter")
    };
    assert_eq!(ticket.native, bc::NativeId(0));
    assert!(ticket.await_activation.is_some());
    assert!(vm.host_arguments(ticket).unwrap()[0].same_bits(object));
    let span = ticket.location.source.unwrap();
    assert_eq!(
        &SOURCE[span.start as usize..span.end as usize],
        "await request"
    );
    ticket
}

fn returned(vm: &mut Vm, task: TaskId, tiny: bool, expected: i64) {
    let RunOutcome::Returned(id) = next(vm, task, tiny) else {
        panic!("expected result")
    };
    assert_eq!(vm.delivery_value(id).unwrap().as_int(), Some(expected));
    vm.discard_delivery(id).unwrap();
}

#[test]
fn shared_waits_completed_results_cancellation_and_gc_use_the_same_native_protocol() {
    for tiny in [false, true] {
        let mut vm = fixture();
        let (object, root) = request(&mut vm);
        let first = start(&mut vm, "main", &[object], 1, bc::Context::Ordinary);
        let cancelled = start(&mut vm, "main", &[object], 2, bc::Context::Ordinary);
        let last = start(&mut vm, "main", &[object], 3, bc::Context::Ordinary);
        let mut waits = Vec::new();
        for task in [first, cancelled, last] {
            let t = ticket(&mut vm, task, object, tiny);
            vm.respond(t, HostResponse::Pending(t.wait_id())).unwrap();
            assert!(
                matches!(next(&mut vm, task, tiny), RunOutcome::Waiting(w) if w == t.wait_id())
            );
            assert!(vm.wait_is_pending(t.wait_id()));
            waits.push(t.wait_id());
        }
        vm.cancel(RootOwner(2)).unwrap();
        assert!(!vm.wait_is_pending(waits[1]));
        assert!(matches!(
            vm.complete(waits[1], Completion::Ready(Value::int(99))),
            Err(VmError::InvalidWait)
        ));
        // The shared producer remains available to both surviving consumers.
        for (task, wait) in [(first, waits[0]), (last, waits[2])] {
            vm.complete(wait, Completion::Ready(Value::int(20)))
                .unwrap();
            assert!(!vm.wait_is_pending(wait));
            assert!(matches!(
                vm.complete(wait, Completion::Ready(Value::int(99))),
                Err(VmError::InvalidWait)
            ));
            let second = ticket(&mut vm, task, object, tiny);
            vm.respond(second, HostResponse::Ready(Value::int(22)))
                .unwrap();
            // Ready publishes data; the consumer only executes on a later run_slice.
            assert!(matches!(
                vm.respond(second, HostResponse::Ready(Value::int(99))),
                Err(VmError::InvalidTicket)
            ));
            returned(&mut vm, task, tiny, 42);
        }
        loop {
            match vm.run_slice(
                cancelled,
                SliceBudget {
                    instructions: 1,
                    ..Default::default()
                },
                || 0,
            ) {
                Ok(RunOutcome::BudgetYield(_)) => {}
                Err(VmError::Idle) => break,
                other => panic!("cancel cleanup: {other:?}"),
            }
        }
        let cooperative = start(&mut vm, "cooperative", &[object], 6, bc::Context::Ordinary);
        let t = ticket(&mut vm, cooperative, object, tiny);
        vm.respond(t, HostResponse::Pending(t.wait_id())).unwrap();
        vm.cancel_cooperatively(cooperative).unwrap();
        returned(&mut vm, cooperative, tiny, 42);
        assert!(!vm.wait_is_pending(t.wait_id()));
        assert!(matches!(
            vm.complete(t.wait_id(), Completion::Ready(Value::int(0))),
            Err(VmError::InvalidWait)
        ));
        vm.heap_mut().release_root(root).unwrap();
        if vm.heap().phase() == GcPhase::Idle {
            vm.begin_collection().unwrap();
        }
        while !vm.gc_step(GcBudget::default()).unwrap().cycle_complete {}
        vm.begin_collection().unwrap();
        while !vm.gc_step(GcBudget::default()).unwrap().cycle_complete {}
        assert!(
            vm.heap().info(object).is_err(),
            "consumer roots must retire"
        );
    }
}

#[test]
fn producer_faults_get_independent_await_envelopes_and_preserve_active_catch() {
    for tiny in [false, true] {
        let mut vm = fixture();
        let (object, object_root) = request(&mut vm);
        let producer = start(&mut vm, "producer", &[], 4, bc::Context::Ordinary);
        let RunOutcome::Fault(delivery) = next(&mut vm, producer, tiny) else {
            panic!("producer fault")
        };
        let source = vm.take_fault_delivery(delivery, RootOwner(99)).unwrap();
        let original = source.info(vm.heap()).unwrap();
        let mut consumer_faults = Vec::new();
        for mode in ["ready", "pending", "continuation"] {
            let task = start(&mut vm, "consumer", &[object], 5, bc::Context::Ordinary);
            let mut t = ticket(&mut vm, task, object, tiny);
            vm.replace_native_faults(task, t.frame, &[source.id()])
                .unwrap();
            if mode == "continuation" {
                vm.respond(
                    t,
                    HostResponse::Continue(Continuation {
                        version: 1,
                        stage: 0,
                        values: &[],
                        faults: FaultRetention::Preserve,
                    }),
                )
                .unwrap();
                t = ticket(&mut vm, task, object, tiny);
                assert!(matches!(
                    t.input,
                    NativeInput::Continue {
                        version: 1,
                        stage: 0
                    }
                ));
            }
            if mode != "ready" {
                vm.respond(t, HostResponse::Pending(t.wait_id())).unwrap();
                vm.complete(t.wait_id(), Completion::ForwardFault(source.id()))
                    .unwrap();
            } else {
                vm.respond(t, HostResponse::ForwardFault(source.id()))
                    .unwrap();
            }
            if mode == "continuation" {
                t = ticket(&mut vm, task, object, tiny);
                assert_eq!(t.input, NativeInput::WaitingFault);
                let callback = vm.host_callback_fault(t).unwrap().unwrap();
                assert_eq!(
                    callback.id,
                    source.id(),
                    "continuation receives the untouched producer fault"
                );
                vm.respond(t, HostResponse::ForwardFault(callback.id))
                    .unwrap();
            }
            let RunOutcome::Fault(delivery) = next(&mut vm, task, tiny) else {
                panic!("consumer fault")
            };
            let info = vm.delivery_fault(delivery).unwrap().unwrap();
            assert_ne!(info.id, source.id());
            assert_eq!(info.location, t.location);
            assert_eq!(info.code, original.code);
            assert!(
                vm.heap()
                    .fault_value(info.id)
                    .unwrap()
                    .same_bits(vm.heap().fault_value(source.id()).unwrap())
            );
            let mut cursor = vm.heap().fault_causes(info.id).unwrap();
            let mut causes = Vec::new();
            while let Some(cause) = cursor.next(vm.heap()).unwrap() {
                causes.push(cause);
            }
            assert_eq!(causes.len(), 2);
            assert!(causes.iter().any(|c| matches!(c.kind, CauseKind::AwaitedFault { activation } if Some(activation)==t.await_activation) && c.fault==Some(source.id())));
            assert!(causes.iter().any(|c| c.kind == CauseKind::CaughtFault
                && vm.heap().fault_value(c.fault.unwrap()).unwrap().as_int() == Some(7)));
            assert_eq!(source.info(vm.heap()).unwrap(), original);
            consumer_faults.push(vm.take_fault_delivery(delivery, RootOwner(99)).unwrap());
        }
        assert_ne!(consumer_faults[0].id(), consumer_faults[1].id());
        vm.heap_mut().release_fault_root(source).unwrap();
        for fault in consumer_faults {
            vm.heap_mut().release_fault_root(fault).unwrap();
        }
        vm.heap_mut().release_root(object_root).unwrap();
    }
}

#[test]
fn await_enforces_dynamic_context_and_reuses_storage_for_repeated_settled_results() {
    let mut vm = fixture();
    let (object, root) = request(&mut vm);
    for context in [bc::Context::NoWait, bc::Context::Derived, bc::Context::Init] {
        let task = start(&mut vm, "main", &[object], 1, context);
        let RunOutcome::Fault(id) = next(&mut vm, task, true) else {
            panic!("restricted await")
        };
        assert_eq!(
            vm.delivery_fault(id).unwrap().unwrap().kind,
            FaultKind::Effect
        );
        vm.discard_delivery(id).unwrap();
    }
    for invalid in [Value::int(1), Value::NULL] {
        let task = start(&mut vm, "main", &[invalid], 1, bc::Context::Ordinary);
        let RunOutcome::Fault(id) = next(&mut vm, task, true) else {
            panic!("non Awaitable")
        };
        assert_eq!(
            vm.delivery_fault(id).unwrap().unwrap().kind,
            FaultKind::NotAwaitable
        );
        vm.discard_delivery(id).unwrap();
    }
    let task = start(&mut vm, "repeat", &[object], 1, bc::Context::Ordinary);
    let mut activations = std::collections::BTreeSet::new();
    let mut allocations = None;
    for i in 0..128 {
        let t = ticket(&mut vm, task, object, false);
        assert!(activations.insert(t.await_activation.unwrap()));
        if i == 0 {
            allocations = Some(vm.heap().stats().chunk_allocations);
        }
        vm.respond(t, HostResponse::Ready(Value::int(1))).unwrap();
    }
    returned(&mut vm, task, false, 128);
    assert_eq!(
        vm.heap().stats().chunk_allocations,
        allocations.unwrap(),
        "settled waits reuse native frame windows, no per-wait heap payload"
    );
    eprintln!("128 settled awaits: no additional heap payload chunks after first dispatch");
    vm.heap_mut().release_root(root).unwrap();
}
#[test]
fn snapshot_restores_shared_graph_cells_classes_faults_waits_and_rejects_stale_completion() {
    use aside_vm::snapshot::{SnapshotBudget, SnapshotError, SnapshotRegistry, VmSnapshot};
    fn sendable<T: Send>() {}
    sendable::<VmSnapshot>();
    let mut vm = fixture();
    let (object, _) = request(&mut vm);
    let first = start(&mut vm, "saved_state", &[object], 1, bc::Context::Ordinary);
    let a = ticket(&mut vm, first, object, false);
    vm.respond(a, HostResponse::Pending(a.wait_id())).unwrap();
    let second = start(&mut vm, "saved_state", &[object], 2, bc::Context::Ordinary);
    let b = ticket(&mut vm, second, object, true);
    vm.respond(b, HostResponse::Pending(b.wait_id())).unwrap();

    let dead = vm.heap_mut().alloc_array(&[]).unwrap();
    let weak = vm.heap_mut().alloc_weakref(dead).unwrap();
    let root = vm
        .heap_mut()
        .add_root(RootOwner(99), RootKind::Host, weak)
        .unwrap();
    let bytes = vm.heap_mut().alloc_byte_buffer(&[]).unwrap();
    for _ in 0..32 {
        vm.heap_mut().bytes_append(bytes, &[73; 1024]).unwrap();
    }
    let byte_root = vm
        .heap_mut()
        .add_root(RootOwner(99), RootKind::Host, bytes)
        .unwrap();
    fn readonly(
        _: &aside_vm::PropertyRead<'_>,
        _: Value,
        _: u32,
        _: Value,
    ) -> Result<(), aside_vm::HeapError> {
        Err(aside_vm::HeapError::ReadOnly)
    }
    static READONLY: aside_vm::HostPropertyValidator =
        aside_vm::HostPropertyValidator::new("test.readonly", 1, readonly);
    static READONLY_V2: aside_vm::HostPropertyValidator =
        aside_vm::HostPropertyValidator::new("test.readonly", 2, readonly);
    let fields = vm.heap_mut().alloc_table().unwrap();
    let host = vm
        .heap_mut()
        .alloc_host(fields, &[], Some(&READONLY))
        .unwrap();
    vm.heap_mut()
        .add_root(RootOwner(99), RootKind::Host, host)
        .unwrap();
    let mut registry = SnapshotRegistry::default();
    let mut unsupported = vm.snapshot_begin(&registry).unwrap();
    loop {
        match unsupported.step(SnapshotBudget::default()) {
            Ok(progress) => assert!(
                !progress.complete,
                "unregistered native schema must not silently disappear"
            ),
            Err(SnapshotError::MissingAdapter { .. }) => break,
            other => panic!("unexpected snapshot adapter outcome: {other:?}"),
        }
    }
    drop(unsupported);
    registry.register_validator(&READONLY).unwrap();
    let mut capture = vm.snapshot_begin(&registry).unwrap();
    let mut slices = 0;
    loop {
        let progress = capture.step(SnapshotBudget { units: 1 }).unwrap();
        assert!(progress.units <= 1);
        assert!(progress.bytes <= 2048, "one record must remain bounded");
        slices += 1;
        if progress.complete {
            break;
        }
    }
    assert!(slices > 32, "large buffers must split across captures");
    let image = capture.finish().unwrap();
    let encoded = image.encode().unwrap();
    eprintln!(
        "snapshot bytes={}, records={}, capture_slices={slices}",
        encoded.len(),
        image.record_count()
    );
    let image = VmSnapshot::decode(&encoded).unwrap();
    let p = vm.program().package().clone();
    let support = bc::RuntimeSupport {
        host_types: Vec::new(),
        natives: vec![bc::NativeSupport {
            standard_derived_commit: false,
            read_only_leaf: None,
            symbol: "host::await_request".into(),
            abi_version: 1,
            signature: p.signatures[p.natives[0].signature.index()].clone(),
            required_capabilities: vec![],
        }],
        ..Default::default()
    };
    let program = bc::verify(p, bc::Limits::default()).unwrap();
    let mut wrong = program.package().clone();
    let changed = wrong
        .constants
        .iter_mut()
        .find(|c| **c == bc::Const::Int(40))
        .unwrap();
    *changed = bc::Const::Int(41);
    assert!(matches!(
        Vm::restore_begin(
            &image,
            bc::verify(wrong, bc::Limits::default()).unwrap(),
            support.clone(),
            VmLimits::default(),
            SessionEpoch(2),
            &registry
        ),
        Err(SnapshotError::CodeMismatch)
    ));
    let mut wrong_registry = SnapshotRegistry::default();
    wrong_registry.register_validator(&READONLY_V2).unwrap();
    let mut candidate = Vm::restore_begin(
        &image,
        bc::verify(program.package().clone(), bc::Limits::default()).unwrap(),
        support.clone(),
        VmLimits::default(),
        SessionEpoch(2),
        &wrong_registry,
    )
    .unwrap();
    loop {
        match candidate.step(SnapshotBudget::default()) {
            Ok(progress) => assert!(!progress.complete),
            Err(SnapshotError::AdapterVersion { version: 1, .. }) => break,
            other => panic!("unexpected restore adapter outcome: {other:?}"),
        }
    }
    drop(candidate);
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
    let (mut loaded, map) = restore.finish().unwrap();
    assert_ne!(loaded.heap().id(), vm.heap().id());
    let weak = loaded.heap().root_value(map.root(root).unwrap()).unwrap();
    assert!(
        loaded.heap_mut().upgrade_weakref(weak).unwrap().is_none(),
        "weak edges cannot retain otherwise dead data"
    );
    let bytes = loaded
        .heap()
        .root_value(map.root(byte_root).unwrap())
        .unwrap();
    let mut tail = [0; 1024];
    loaded
        .heap()
        .read_bytes(bytes, 31 * 1024, &mut tail)
        .unwrap();
    assert_eq!(tail, [73; 1024]);
    assert!(
        loaded
            .complete(a.wait_id(), Completion::Ready(Value::int(99)))
            .is_err()
    );
    for (task, wait) in [(first, a.wait_id()), (second, b.wait_id())] {
        loaded
            .complete(map.wait(wait).unwrap(), Completion::Ready(Value::int(0)))
            .unwrap();
        returned(&mut loaded, map.task(task).unwrap(), true, 42);
    }
    // Restoring did not consume the source's waits or mutate its shared cells.
    vm.complete(a.wait_id(), Completion::Ready(Value::int(0)))
        .unwrap();
    returned(&mut vm, first, true, 42);
}
