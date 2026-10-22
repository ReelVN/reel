//! Shared native methods retain ordinary binder, ownership and snapshot contracts.
use aside_compiler::{AnalysisOptions, analyze_module, semantic::*};
use aside_host::{CallContext, Error, NativeBinding, native, source::*};
use aside_vm::{
    GcBudget, GcPhase, HeapError, RootOwner, SessionEpoch, Value, bytecode as bc, executor::*,
};

#[native(symbol="sample::make",doc_result=named("sample::types::Base"))]
fn make(
    #[aside(default = 0)] kind: i64,
    #[aside(default = false)] shadow: bool,
    #[aside(default = false)] immutable: bool,
    #[aside(context)] context: &mut CallContext<'_, ()>,
) -> Result<Value, Error> {
    let heap = context.vm.heap_mut();
    let fields = heap.alloc_table()?;
    if shadow {
        let key = heap.alloc_string("read")?;
        heap.table_store(fields, 0, key, Value::int(17))?;
    }
    let host = if immutable {
        heap.alloc_immutable_host(fields, &[Value::int(40)])?
    } else {
        heap.alloc_host(fields, &[Value::int(40)], None)?
    };
    let ty = heap.host_type(match kind {
        1 => "sample::types::Inherited",
        2 => "sample::types::Child",
        _ => "sample::types::Base",
    })?;
    heap.set_host_type(host, ty)?;
    assert_eq!(
        heap.table_len(fields)?,
        u32::from(shadow),
        "constructors store only their own fields"
    );
    Ok(host)
}

#[native(symbol = "sample::read")]
fn read(
    receiver: Value,
    #[aside(default = 0)] extra: i64,
    #[aside(context)] context: &mut CallContext<'_, ()>,
) -> Result<i64, Error> {
    context.vm.heap().host_fields(receiver)?;
    Ok(context
        .vm
        .heap()
        .host_payload(receiver, 0)?
        .as_int()
        .unwrap()
        + extra)
}

#[native(symbol = "sample::child_read")]
fn child_read(
    receiver: Value,
    #[aside(default = 0)] extra: i64,
    #[aside(context)] context: &mut CallContext<'_, ()>,
) -> Result<i64, Error> {
    read(receiver, extra, context).map(|value| value + 2)
}

#[native(symbol="sample::identity",doc_result=named("sample::types::Base"))]
fn identity(receiver: Value) -> Value {
    receiver
}

#[native(symbol="sample::retire",effects=bc::Effects::ALL)]
fn retire(
    receiver: Value,
    #[aside(context)] context: &mut CallContext<'_, ()>,
) -> Result<(), Error> {
    context.vm.heap_mut().retire_host(receiver)?;
    Ok(())
}

struct Resolver(Api);
impl ModuleResolver for Resolver {
    fn resolve_type_member(&mut self, ty: &str, member: &str) -> Option<ExternalMember> {
        self.0.member(ty, member)
    }
    fn is_named_subtype(&self, actual: &str, expected: &str) -> Option<bool> {
        self.0.is_subtype(actual, expected)
    }
    fn resolve(&mut self, request: &ImportRequest) -> Result<ResolvedImport, ResolveError> {
        let mut modules = MemoryResolver::default();
        modules.insert(ModuleInterface {
            canonical_path: "sample::types".into(),
            version: "1".into(),
            exports: self.0.types.iter().map(NamedType::symbol).collect(),
        });
        modules.resolve(request)
    }
}

struct Fixture {
    program: bc::VerifiedProgram,
    init: bc::FunctionId,
    main: bc::FunctionId,
    bindings: Vec<NativeBinding>,
    support: bc::RuntimeSupport,
}
fn fixture(source: &str) -> Fixture {
    let bindings = vec![
        make::binding(),
        read::binding(),
        child_read::binding(),
        identity::binding(),
        retire::binding(),
    ];
    let contracts: Vec<_> = bindings
        .iter()
        .map(|binding| binding.contract.clone())
        .collect();
    let functions: Vec<_> = bindings
        .iter()
        .map(|binding| {
            let mut declaration = Declaration::native(&binding.contract, binding.documentation);
            declaration.symbol.signature = Some((binding.source_signature)(&binding.contract));
            declaration
        })
        .collect();
    let base = NamedType::new(
        "sample::types::Base",
        "A shared native receiver.",
        vec![
            Member::method(functions[1].clone().bound("read")),
            Member::method(functions[3].clone().bound("identity")),
        ],
    );
    let inherited =
        NamedType::new("sample::types::Inherited", "An inherited receiver.", vec![]).extends(&base);
    let child = NamedType::new(
        "sample::types::Child",
        "An overriding receiver.",
        vec![Member::method(functions[2].clone().bound("read"))],
    )
    .extends(&base);
    let api = Api {
        types: vec![base, inherited, child],
        functions,
        ..Default::default()
    };
    let registrations = api.registrations();
    let module = analyze_module(
        source,
        &AnalysisOptions {
            hosts: api
                .functions
                .iter()
                .map(|declaration| declaration.symbol.clone())
                .collect(),
            ..Default::default()
        },
        &mut Resolver(api.clone()),
    )
    .unwrap()
    .into_module()
    .unwrap();
    let compiled = aside_host::lower(
        &module,
        &contracts,
        &api.functions,
        &registrations,
        &[],
        Default::default(),
    )
    .unwrap();
    let p = compiled.program.package();
    let main = bc::FunctionId(
        p.functions
            .iter()
            .position(|function| p.symbols[function.name.index()] == "main")
            .unwrap() as u32,
    );
    let support =
        aside_host::runtime_support(&compiled.program, &contracts, &registrations, &[]).unwrap();
    Fixture {
        program: compiled.program,
        init: compiled.entry,
        main,
        bindings,
        support,
    }
}

fn drive(
    vm: &mut Vm,
    mut task: TaskId,
    fixture: &Fixture,
    tiny: bool,
    restore: bool,
) -> Result<Value, FaultKind> {
    use aside_vm::snapshot::{SnapshotBudget, SnapshotRegistry, VmSnapshot};
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
    let mut epochs = 1;
    for _ in 0..100_000 {
        match vm.run_slice(task, budget, || 0).unwrap() {
            RunOutcome::BudgetYield(usage) => {
                budget.work = usage.required_work.max(1);
                budget.allocation_bytes = usage.required_allocation;
            }
            RunOutcome::HostCall(ticket) => {
                let result = (fixture.bindings[ticket.native.index()].invoke)(&mut (), vm, ticket);
                if let Err(error) = result {
                    let kind = match error {
                        Error::Heap(HeapError::RetiredHost) => FaultKind::InvalidState,
                        other => panic!("unexpected fixture native error: {other}"),
                    };
                    vm.respond_language_error(ticket, kind, Value::NULL)
                        .unwrap();
                }
            }
            RunOutcome::Returned(delivery) => {
                let value = vm.delivery_value(delivery).unwrap();
                vm.discard_delivery(delivery).unwrap();
                return Ok(value);
            }
            RunOutcome::Fault(delivery) => {
                let kind = vm.delivery_fault(delivery).unwrap().unwrap().kind;
                vm.discard_delivery(delivery).unwrap();
                return Err(kind);
            }
            other => panic!("unexpected shared method outcome: {other:?}"),
        }
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
        if restore {
            let registry = SnapshotRegistry::default();
            let mut capture = vm.snapshot_begin(&registry).unwrap();
            while !capture.step(SnapshotBudget::default()).unwrap().complete {}
            let image = VmSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
            epochs += 1;
            let mut loading = Vm::restore_begin(
                &image,
                fixture.program.clone(),
                fixture.support.clone(),
                VmLimits::default(),
                SessionEpoch(epochs),
                &registry,
            )
            .unwrap();
            while !loading.step(SnapshotBudget::default()).unwrap().complete {}
            let (loaded, map) = loading.finish().unwrap();
            *vm = loaded;
            task = map.task(task).unwrap();
        }
    }
    panic!("shared method execution made no progress");
}

#[test]
fn shared_methods_cover_dynamic_typed_inherited_and_frozen_receivers() {
    let fixture = fixture(
        r#"
        use sample::types::Base;
        fn erased(value){value}
        fn main(){
            let base:Base=make();
            if base.read(extra:2)!=42 || base.identity()!=base {throw "typed member";}
            let inherited=make(1);
            if inherited["r"+"ead"](extra:2)!=42 {throw "inherited dynamic method";}
            let child=erased(make(2));
            let kept=child["read"];
            if kept()!=42 || child.identity().read()!=42 {throw "bound override";}
            let frozen=make(2,immutable:true);
            if frozen.read()!=42 {throw "frozen receiver";}
            let shadow=erased(make(shadow:true));
            if shadow.read!=17 {throw "own field priority";}
            shadow.read=19;
            if shadow["read"]!=19 {throw "own field writes";}
            42
        }
    "#,
    );
    for encoded in [false, true] {
        for tiny in [false, true] {
            let program = if encoded {
                bc::VerifiedProgram::decode(
                    &bc::encode(fixture.program.package(), bc::Limits::default()).unwrap(),
                    bc::Limits::default(),
                )
                .unwrap()
            } else {
                fixture.program.clone()
            };
            let mut vm = Vm::load(
                program,
                fixture.support.clone(),
                VmLimits::default(),
                SessionEpoch(1),
            )
            .unwrap();
            let init = vm
                .start(fixture.init, &[], RootOwner(1), bc::Context::Init)
                .unwrap();
            drive(&mut vm, init, &fixture, tiny, false).unwrap();
            let main = vm
                .start(fixture.main, &[], RootOwner(1), bc::Context::Ordinary)
                .unwrap();
            assert_eq!(
                drive(&mut vm, main, &fixture, tiny, false)
                    .unwrap()
                    .as_int(),
                Some(42)
            );
        }
    }
}

#[test]
fn missing_readonly_and_retired_methods_report_language_faults() {
    for (operation, expected) in [
        ("object.missing", FaultKind::MissingKey),
        ("object[\"missing\"]", FaultKind::MissingKey),
        ("object[0]", FaultKind::MissingKey),
        ("object.read(receiver:object)", FaultKind::DuplicateArgument),
        ("object.read=1;", FaultKind::ReadOnly),
        ("object[\"read\"]=1;", FaultKind::ReadOnly),
        ("object.missing=1;", FaultKind::MissingKey),
        (
            "let kept=object.read;retire(object);kept()",
            FaultKind::InvalidState,
        ),
        ("retire(object);object.read()", FaultKind::InvalidState),
        ("retire(object);object.missing", FaultKind::InvalidState),
        ("retire(object);object.read=1;", FaultKind::InvalidState),
    ] {
        let fixture = fixture(&format!(
            "fn erased(value){{value}} fn main(){{let object=erased(make());{operation}}}"
        ));
        let mut vm = Vm::load(
            fixture.program.clone(),
            fixture.support.clone(),
            VmLimits::default(),
            SessionEpoch(1),
        )
        .unwrap();
        let init = vm
            .start(fixture.init, &[], RootOwner(1), bc::Context::Init)
            .unwrap();
        drive(&mut vm, init, &fixture, true, false).unwrap();
        let main = vm
            .start(fixture.main, &[], RootOwner(1), bc::Context::Ordinary)
            .unwrap();
        assert_eq!(
            drive(&mut vm, main, &fixture, true, false).unwrap_err(),
            expected,
            "{operation}"
        );
    }
}

#[test]
fn retired_receivers_keep_shared_method_identity_without_exposing_instance_fields() {
    let fixture = fixture(
        r#"
        fn main() {
            let object=make();
            let kept=object.identity;
            retire(object);
            let cleanup=object.identity;
            let dynamic=object["identity"];
            if kept()!=object || cleanup()!=object || dynamic()!=object {throw "retired method receiver";}
            42
        }
    "#,
    );
    for encoded in [false, true] {
        let program = if encoded {
            bc::VerifiedProgram::decode(
                &bc::encode(fixture.program.package(), bc::Limits::default()).unwrap(),
                bc::Limits::default(),
            )
            .unwrap()
        } else {
            fixture.program.clone()
        };
        let mut vm = Vm::load(
            program,
            fixture.support.clone(),
            VmLimits::default(),
            SessionEpoch(1),
        )
        .unwrap();
        let init = vm
            .start(fixture.init, &[], RootOwner(1), bc::Context::Init)
            .unwrap();
        drive(&mut vm, init, &fixture, true, true).unwrap();
        let main = vm
            .start(fixture.main, &[], RootOwner(1), bc::Context::Ordinary)
            .unwrap();
        assert_eq!(
            drive(&mut vm, main, &fixture, true, true).unwrap().as_int(),
            Some(42)
        );
    }
}

#[test]
fn extracted_methods_restore_with_the_original_receiver_across_every_slice() {
    let fixture = fixture(
        r#"
        fn main(){
            let object=make(2,immutable:true);
            let method=object["read"];
            let identity=object.identity;
            if identity()!=object || method()!=42 {throw "restored receiver";}
            method(extra:0)
        }
    "#,
    );
    let mut vm = Vm::load(
        fixture.program.clone(),
        fixture.support.clone(),
        VmLimits::default(),
        SessionEpoch(1),
    )
    .unwrap();
    let init = vm
        .start(fixture.init, &[], RootOwner(1), bc::Context::Init)
        .unwrap();
    drive(&mut vm, init, &fixture, true, false).unwrap();
    let main = vm
        .start(fixture.main, &[], RootOwner(1), bc::Context::Ordinary)
        .unwrap();
    assert_eq!(
        drive(&mut vm, main, &fixture, true, true).unwrap().as_int(),
        Some(42)
    );
}

#[test]
fn shared_method_lookup_accounts_for_work_when_receiver_validation_must_retry() {
    let fixture = fixture("fn main(){make().read}");
    let mut vm = Vm::load(
        fixture.program.clone(),
        fixture.support.clone(),
        VmLimits::default(),
        SessionEpoch(1),
    )
    .unwrap();
    let init = vm
        .start(fixture.init, &[], RootOwner(1), bc::Context::Init)
        .unwrap();
    drive(&mut vm, init, &fixture, false, false).unwrap();
    let task = vm
        .start(fixture.main, &[], RootOwner(1), bc::Context::Ordinary)
        .unwrap();
    let mut work = 1;
    let mut charged_retries = 0;
    for _ in 0..10_000 {
        let budget = SliceBudget {
            instructions: 1,
            work,
            allocation_bytes: 1_000_000,
            deadline_ns: None,
        };
        match vm.run_slice(task, budget, || 0).unwrap() {
            RunOutcome::BudgetYield(usage) => {
                if usage.instructions == 0 && usage.work > 0 && usage.required_work > usage.work {
                    let RunOutcome::BudgetYield(retry) = vm.run_slice(task, budget, || 0).unwrap()
                    else {
                        panic!("a repeated insufficient budget advanced the call");
                    };
                    assert_eq!(retry.instructions, 0);
                    assert_eq!(retry.required_work, usage.required_work);
                    if retry.work > 0 {
                        assert!(retry.required_work > retry.work);
                        charged_retries += 1;
                    }
                }
                work = usage.required_work.max(1);
            }
            RunOutcome::HostCall(ticket) => {
                (fixture.bindings[ticket.native.index()].invoke)(&mut (), &mut vm, ticket).unwrap();
            }
            RunOutcome::Returned(delivery) => {
                assert_eq!(
                    vm.heap()
                        .info(vm.delivery_value(delivery).unwrap())
                        .unwrap()
                        .kind,
                    aside_vm::ObjectKind::BoundNative
                );
                assert_eq!(
                    charged_retries, 1,
                    "the lookup before receiver validation is real work"
                );
                return;
            }
            other => panic!("method lookup did not complete: {other:?}"),
        }
    }
    panic!("method lookup did not make progress");
}

fn collect(vm: &mut Vm) {
    if vm.heap().phase() == GcPhase::Idle {
        vm.begin_collection().unwrap();
    }
    while vm.heap().phase() != GcPhase::Idle {
        vm.gc_step(GcBudget::default()).unwrap();
    }
}

#[test]
fn shared_method_tables_are_charged_once_and_add_no_per_object_graph() {
    let fixture = fixture("fn main(){42}");
    let growth = [true, false].map(|methods| {
        let mut support = fixture.support.clone();
        if !methods {
            for ty in &mut support.host_types {
                ty.methods.clear();
            }
        }
        let mut vm = Vm::load(
            fixture.program.clone(),
            support,
            VmLimits::default(),
            SessionEpoch(1),
        )
        .unwrap();
        let ty = vm.heap().host_type("sample::types::Base").unwrap();
        let initial = vm.heap().stats();
        for _ in 0..1000 {
            let heap = vm.heap_mut();
            let fields = heap.alloc_table().unwrap();
            let object = heap.alloc_host(fields, &[], None).unwrap();
            heap.set_host_type(object, ty).unwrap();
            heap.add_root(RootOwner(1), aside_vm::RootKind::Host, object)
                .unwrap();
        }
        let final_stats = vm.heap().stats();
        assert_eq!(
            final_stats.live_objects - initial.live_objects,
            2000,
            "one fields table and one host per object"
        );
        (
            initial.charged_bytes,
            final_stats.charged_bytes - initial.charged_bytes,
        )
    });
    assert!(
        growth[0].0 > growth[1].0,
        "the shared method table is included in the heap ledger"
    );
    assert_eq!(
        growth[0].1, growth[1].1,
        "object storage growth does not depend on registered methods"
    );
    println!(
        "shared method bytes={}, 1000-host growth={}",
        growth[0].0 - growth[1].0,
        growth[0].1
    );
}

#[test]
fn direct_method_calls_leave_no_retained_bindings_or_payload_blocks() {
    let fixture = fixture("fn main(){let object=make(2);for i in 0..10000{object.read();}42}");
    let mut vm = Vm::load(
        fixture.program.clone(),
        fixture.support.clone(),
        VmLimits::default(),
        SessionEpoch(1),
    )
    .unwrap();
    let init = vm
        .start(fixture.init, &[], RootOwner(1), bc::Context::Init)
        .unwrap();
    drive(&mut vm, init, &fixture, false, false).unwrap();
    let mut retained = None;
    for round in 0..2 {
        let before = vm.heap().stats().chunk_allocations;
        let task = vm
            .start(fixture.main, &[], RootOwner(1), bc::Context::Ordinary)
            .unwrap();
        assert_eq!(
            drive(&mut vm, task, &fixture, false, false)
                .unwrap()
                .as_int(),
            Some(42)
        );
        assert_eq!(
            vm.heap().stats().chunk_allocations,
            before,
            "temporary BoundNative values use inline payloads"
        );
        collect(&mut vm);
        let live = vm.heap().stats().live_objects;
        if let Some(previous) = retained {
            assert_eq!(
                live, previous,
                "the second round retains no method values or receivers"
            );
        }
        retained = Some(live);
        println!("10000 direct host method calls: round={round}, chunks=0, live_after_gc={live}");
    }
}

#[test]
fn method_admission_checks_native_identity_receiver_and_snapshot_fingerprint() {
    use aside_vm::snapshot::{SnapshotBudget, SnapshotRegistry};
    let fixture = fixture("fn main(){42}");
    for mutation in 0..4 {
        let mut support = fixture.support.clone();
        let method = &mut support.host_types[0].methods[0];
        match mutation {
            0 => method.abi_version += 1,
            1 => method.native = "sample::make".into(), // Its first parameter is an int with a default.
            2 => method.name = "not_in_the_package".into(),
            _ => {
                let method = method.clone();
                support.host_types[0].methods.push(method);
            }
        }
        assert!(
            Vm::load(
                fixture.program.clone(),
                support,
                VmLimits::default(),
                SessionEpoch(1)
            )
            .is_err(),
            "mutation={mutation}"
        );
    }
    let vm = Vm::load(
        fixture.program.clone(),
        fixture.support.clone(),
        VmLimits::default(),
        SessionEpoch(1),
    )
    .unwrap();
    let registry = SnapshotRegistry::default();
    let mut capture = vm.snapshot_begin(&registry).unwrap();
    while !capture.step(SnapshotBudget::default()).unwrap().complete {}
    let snapshot = capture.finish().unwrap();
    let mut support = fixture.support.clone();
    support.host_types[0].methods[0].native = "sample::child_read".into();
    let mut loading = Vm::restore_begin(
        &snapshot,
        fixture.program.clone(),
        support,
        VmLimits::default(),
        SessionEpoch(2),
        &registry,
    )
    .unwrap();
    loop {
        match loading.step(SnapshotBudget::default()) {
            Err(_) => break,
            Ok(progress) => assert!(
                !progress.complete,
                "a changed method table must reject the saved registration fingerprint"
            ),
        }
    }
    // Broad host registries may accompany a package that links no methods.
    let minimal = bc::verify(bc::BytecodePackage::default(), bc::Limits::default()).unwrap();
    Vm::load(
        minimal,
        bc::RuntimeSupport {
            host_types: fixture.support.host_types,
            ..Default::default()
        },
        VmLimits::default(),
        SessionEpoch(1),
    )
    .unwrap();
}
