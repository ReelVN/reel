//! Source declarations -> compact sidecar -> actual waiting frames -> current-code return.
use aside_compiler::{AnalysisOptions, hir, lowering, semantic::*};
use aside_vm::{RootKind, RootOwner, SessionEpoch, Value, bytecode as bc, executor::*, resume::*};

fn signature(result: TypeHint, effect: CallEffect) -> ExternalSignature {
    ExternalSignature {
        parameters: vec![ExternalParameter {
            name: "value".into(),
            ty: if result == TypeHint::Null {
                TypeHint::String
            } else {
                TypeHint::Any
            },
            mutable: false,
            default: false,
            rest: false,
        }],
        result,
        result_cases: vec![],
        result_parameter: None,
        effect,
        init_safe: Some(true),
        derived_safe: Some(false),
    }
}
fn flow_signature() -> ExternalSignature {
    let mut result = signature(TypeHint::Int, CallEffect::MayWait);
    result.parameters.insert(
        0,
        ExternalParameter {
            name: "entry".into(),
            ty: TypeHint::Closure,
            mutable: false,
            default: false,
            rest: false,
        },
    );
    result
}
fn resolver() -> MemoryResolver {
    let mut resolver = MemoryResolver::default();
    let exports = [
        ("flow", Intrinsic::ResumeFlow),
        ("point", Intrinsic::ResumePoint),
    ]
    .map(|(name, intrinsic)| {
        let mut symbol =
            ExternalSymbol::function(name, signature(TypeHint::Null, CallEffect::NoWait));
        symbol.intrinsic = Some(intrinsic);
        symbol
    });
    resolver.insert(ModuleInterface {
        canonical_path: "std::resume".into(),
        version: "1".into(),
        exports: exports.into(),
    });
    resolver.insert(ModuleInterface {
        canonical_path: "host".into(),
        version: "1".into(),
        exports: vec![
            ExternalSymbol::function("wait", signature(TypeHint::Int, CallEffect::MayWait)),
            ExternalSymbol::function("run_flow", flow_signature()),
        ],
    });
    resolver
}
fn seed(version: u64, flow_effects: bc::Effects) -> bc::VerifiedProgram {
    let mut package = bc::BytecodePackage::default();
    package.content_version = version;
    package.symbols = ["host", "host::wait", "value", "host::run_flow", "entry"]
        .map(str::to_owned)
        .into();
    package.types = vec![
        bc::TypeDef::Builtin(bc::BuiltinType::Any),
        bc::TypeDef::Builtin(bc::BuiltinType::Int),
        bc::TypeDef::Builtin(bc::BuiltinType::Closure),
    ];
    package.signatures = vec![bc::Signature {
        parameters: vec![bc::Parameter {
            name: bc::SymbolId(2),
            ty: bc::TypeId(0),
            mutable: false,
            default: None,
        }],
        rest: None,
        return_type: bc::TypeId(1),
        effects: bc::Effects::ALL,
        property_bag_tail: None,
    }];
    package.signatures.push(bc::Signature {
        parameters: vec![
            bc::Parameter {
                name: bc::SymbolId(4),
                ty: bc::TypeId(2),
                mutable: false,
                default: None,
            },
            bc::Parameter {
                name: bc::SymbolId(2),
                ty: bc::TypeId(0),
                mutable: false,
                default: None,
            },
        ],
        rest: None,
        return_type: bc::TypeId(1),
        effects: flow_effects,
        property_bag_tail: None,
    });
    package.natives = vec![bc::NativeDef {
        symbol: bc::SymbolId(1),
        signature: bc::SignatureId(0),
        abi_version: 1,
        capabilities: vec![],
    }];
    package.natives.push(bc::NativeDef {
        symbol: bc::SymbolId(3),
        signature: bc::SignatureId(1),
        abi_version: 1,
        capabilities: vec![],
    });
    package.modules = vec![bc::ModuleDef {
        path: bc::SymbolId(0),
        imports: vec![],
        exports: vec![],
        globals: vec![],
        initializer: None,
    }];
    package.functions = vec![bc::FunctionDef {
        module: bc::ModuleId(0),
        name: bc::SymbolId(1),
        kind: bc::FunctionKind::Native(bc::NativeId(0)),
        signature: bc::SignatureId(0),
        context: bc::Context::Ordinary,
        captures: vec![],
        register_count: 0,
        bindings: vec![],
        descriptors: vec![],
        code: Vec::<bc::Instruction>::new(),
        regions: vec![],
        source_map: vec![],
    }];
    let mut run = package.functions[0].clone();
    run.name = bc::SymbolId(3);
    run.signature = bc::SignatureId(1);
    run.kind = bc::FunctionKind::Native(bc::NativeId(1));
    package.functions.push(run);
    bc::verify(package, bc::Limits::default()).unwrap()
}
fn compile(
    source: &str,
    version: u64,
) -> Result<lowering::ProjectCompilation, lowering::ProjectError> {
    compile_with_effects(source, version, bc::Effects::ALL)
}
fn compile_with_effects(
    source: &str,
    version: u64,
    effects: bc::Effects,
) -> Result<lowering::ProjectCompilation, lowering::ProjectError> {
    let source = source.replace("wait(", "host.wait(");
    let inputs = [SourceModule {
        canonical_path: "game".into(),
        source_path: Some("story.aside".into()),
        version: version.to_string(),
        source: source.into(),
    }];
    let mut analysis =
        analyze_project(&inputs, &AnalysisOptions::default(), &mut resolver()).unwrap();
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
    let plan = std::mem::take(&mut analysis.plan);
    let modules = analysis.into_modules().unwrap();
    let references = ["wait", "run_flow"].map(|export| SymbolReference {
        module: "host".into(),
        version: "1".into(),
        export: Some(export.into()),
    });
    let signatures = [
        signature(TypeHint::Int, CallEffect::MayWait),
        flow_signature(),
    ];
    let links = vec![
        modules[0]
            .external_bindings()
            .iter()
            .filter(|binding| binding.kind == hir::ExternalBindingKind::Reference)
            .filter_map(|binding| {
                let native = references.iter().position(|reference| {
                    binding.descriptor.reference.as_deref() == Some(reference)
                })?;
                let target = binding
                    .symbol
                    .map(lowering::ExternalTarget::Symbol)
                    .or(binding.node.map(lowering::ExternalTarget::Node))
                    .unwrap();
                Some(lowering::NativeLink {
                    target,
                    native: bc::NativeId(native as u32),
                    reference: &references[native],
                    source_signature: Some(&signatures[native]),
                })
            })
            .collect::<Vec<_>>(),
    ];
    lowering::compile_project(
        modules,
        &plan,
        &seed(version, effects),
        &[],
        &links,
        &[],
        lowering::Options {
            content_version: version,
            ..Default::default()
        },
    )
}
const HEADER: &str = "use std::resume::{flow,point}; use host; let mut prefixes=0; let mut boots=0; fn prefix_count(){prefixes} fn boot_count(){boots} fn bootstrap(value:int){boots+=1;host.run_flow(main,value)}";
const A: &str = r#"
fn main(seed:int) {
    flow("root"); prefixes += 1;
    let kept = seed + 1;
    point("child"); let answer = middle(kept);
    kept + answer
}
fn middle(value:int) {
    flow("middle"); prefixes += 1;
    let mut kept = value * 2;
    point("leaf"); let answer = leaf(kept);
    kept += 2; kept + answer
}
fn leaf(value:int) {
    flow("leaf"); prefixes += 1;
    let mut kept = value + 3;
    let read = || kept; read();
    point("line"); let answer = wait(kept + 2);
    kept += 4; kept + answer
}
"#;
const B: &str = r#"
fn leaf(value:int) {
    flow("leaf"); prefixes += 10;
    let unrelated = value + 700;
    let mut kept = 3000;
    let read = || kept; read();
    point("line"); let answer = wait(kept + 5);
    kept += 8; kept + answer
}
fn unrelated(value:int) { value + 900 }
fn main(seed:int) {
    flow("root"); prefixes += 10;
    let ignored = unrelated(seed);
    let kept = 1000;
    point("child"); let answer = middle(kept);
    kept + answer + 100
}
fn middle(value:int) {
    flow("middle"); prefixes += 10;
    let mut kept = 2000;
    point("leaf"); let answer = leaf(kept);
    kept += 7; kept + answer
}
"#;
fn next(vm: &mut Vm, task: TaskId) -> RunOutcome {
    loop {
        let outcome = vm.run_slice(task, SliceBudget::default(), || 0).unwrap();
        if !matches!(outcome, RunOutcome::BudgetYield(_)) {
            return outcome;
        }
    }
}
fn function(vm: &Vm, name: &str) -> bc::FunctionId {
    bc::FunctionId(
        vm.program()
            .package()
            .functions
            .iter()
            .position(|function| vm.program().package().symbols[function.name.index()] == name)
            .unwrap() as u32,
    )
}
fn vm(compiled: &lowering::ProjectCompilation, epoch: u64) -> Vm {
    let package = compiled.program.package();
    let support = bc::RuntimeSupport {
        natives: package
            .natives
            .iter()
            .map(|native| bc::NativeSupport {
                standard_derived_commit: false,
                read_only_leaf: None,
                symbol: package.symbols[native.symbol.index()].clone(),
                abi_version: 1,
                signature: package.signatures[native.signature.index()].clone(),
                required_capabilities: vec![],
            })
            .collect(),
        ..Default::default()
    };
    let bytes = bc::encode(package, bc::Limits::default()).unwrap();
    let mut vm = Vm::load(
        bc::VerifiedProgram::decode(&bytes, bc::Limits::default()).unwrap(),
        support,
        VmLimits::default(),
        SessionEpoch(epoch),
    )
    .unwrap();
    let init = vm
        .start(compiled.initializer, &[], RootOwner(1), bc::Context::Init)
        .unwrap();
    let RunOutcome::Returned(delivery) = next(&mut vm, init) else {
        panic!("initializer");
    };
    vm.discard_delivery(delivery).unwrap();
    vm
}
fn continuation(values: &[Value]) -> Continuation<'_> {
    Continuation {
        version: 7,
        stage: 3,
        values,
        faults: FaultRetention::Preserve,
    }
}
fn flow_ticket(vm: &mut Vm, value: i64) -> (TaskId, HostTicket) {
    let task = vm
        .start(
            function(vm, "bootstrap"),
            &[Value::int(value)],
            RootOwner(2),
            bc::Context::Ordinary,
        )
        .unwrap();
    let RunOutcome::HostCall(ticket) = next(vm, task) else {
        panic!("run_flow entry")
    };
    let p = vm.program().package();
    assert_eq!(
        p.symbols[p.natives[ticket.native.index()].symbol.index()],
        "host::run_flow"
    );
    assert_eq!(ticket.input, NativeInput::Entry);
    (task, ticket)
}
fn start_flow(vm: &mut Vm, value: i64) -> (TaskId, FrameId) {
    let (task, ticket) = flow_ticket(vm, value);
    let args = vm.host_arguments(ticket).unwrap();
    let entry = args[0];
    let value = args[1];
    vm.respond(
        ticket,
        HostResponse::CallScript {
            closure: entry,
            arguments: &[value],
            continuation: continuation(&[Value::int(71)]),
        },
    )
    .unwrap();
    (task, ticket.frame)
}
fn finish_flow(vm: &mut Vm, task: TaskId, frame: FrameId) -> DeliveryId {
    let RunOutcome::HostCall(ticket) = next(vm, task) else {
        panic!("current native callback result")
    };
    assert_eq!(ticket.frame, frame);
    assert_eq!(ticket.input, NativeInput::CallbackReturned);
    assert_eq!(ticket.continuation.unwrap().version, 7);
    assert_eq!(ticket.continuation.unwrap().stage, 3);
    assert_eq!(vm.host_continuation(ticket).unwrap()[0].as_int(), Some(71));
    let value = vm.host_resume_value(ticket).unwrap();
    vm.respond(ticket, HostResponse::Ready(value)).unwrap();
    let RunOutcome::Returned(delivery) = next(vm, task) else {
        panic!("current bootstrap return")
    };
    delivery
}

#[test]
fn two_versions_restore_real_nested_frames_cells_and_current_return_code() {
    let a = compile(&format!("{HEADER}{A}"), 1).unwrap();
    let b = compile(&format!("{HEADER}{B}"), 2).unwrap();
    assert_ne!(
        a.resume
            .flows
            .iter()
            .find(|flow| flow.key.0 == "root")
            .unwrap()
            .function,
        b.resume
            .flows
            .iter()
            .find(|flow| flow.key.0 == "root")
            .unwrap()
            .function
    );
    let mut old = vm(&a, 1);
    let (task, boundary) = start_flow(&mut old, 10);
    let RunOutcome::HostCall(ticket) = next(&mut old, task) else {
        panic!("waiting operation");
    };
    assert_eq!(old.host_arguments(ticket).unwrap()[0].as_int(), Some(27));
    old.respond(ticket, HostResponse::Pending(ticket.wait_id()))
        .unwrap();
    let verified_a = old
        .program()
        .verify_resume(a.resume.clone(), &[], ResumeLimits::default())
        .unwrap();
    let mut capture = old.capture_resume(&verified_a, task, boundary).unwrap();
    let descriptor = capture.descriptor().clone();
    assert_eq!(descriptor.frames.len(), 3);
    let mut saved = vec![vec![]; 3];
    while let Some(binding) = capture.next_binding().unwrap() {
        saved[binding.frame].push((
            binding.key.clone(),
            binding.value.as_int().expect("business scalar"),
        ));
    }
    drop(capture);
    for movable in [false, true] {
        let mut current = vm(&b, 2);
        let (current_task, ticket) = flow_ticket(&mut current, 999);
        let current_boundary = ticket.frame;
        let verified_b = current
            .program()
            .verify_resume(b.resume.clone(), &[], ResumeLimits::default())
            .unwrap();
        let plan = verified_b.verify_plan(&descriptor).unwrap();
        let mut roots = Vec::new();
        let values: Vec<_> = saved
            .clone()
            .into_iter()
            .map(|bindings| ResumeFrameValues {
                bindings: bindings
                    .into_iter()
                    .map(|(key, value)| {
                        let root = current
                            .heap_mut()
                            .add_root(RootOwner(3), RootKind::Host, Value::int(value))
                            .unwrap();
                        roots.push(root);
                        ResumeValue { key, root }
                    })
                    .collect(),
            })
            .collect();
        let mut budget = SliceBudget {
            instructions: 0,
            work: 1,
            allocation_bytes: 0,
            deadline_ns: None,
        };
        let mut yields = 0;
        let resumed = if movable {
            let mut restore = current
                .restore_resume_cursor(ticket, plan, values, continuation(&[Value::int(71)]))
                .unwrap();
            let mut other_vm = vm(&b, 2);
            assert!(restore.step(&mut other_vm, budget, || 0).is_err());
            assert!(restore.completed_task().is_none());
            let resumed = loop {
                match restore.step(&mut current, budget, || 0).unwrap() {
                    ResumeProgress::BudgetYield(usage) => {
                        yields += 1;
                        budget.work = usage.required_work.max(1);
                        budget.allocation_bytes = usage.required_allocation;
                        let other = current
                            .start(
                                function(&current, "prefix_count"),
                                &[],
                                RootOwner(7),
                                bc::Context::Ordinary,
                            )
                            .unwrap();
                        let RunOutcome::Returned(delivery) = next(&mut current, other) else {
                            panic!()
                        };
                        current.discard_delivery(delivery).unwrap();
                    }
                    ResumeProgress::Complete(task) => break task,
                }
            };
            assert_eq!(restore.completed_task(), Some(resumed));
            restore.cancel(&mut current).unwrap();
            resumed
        } else {
            let mut restore = current
                .restore_resume(ticket, &plan, &values, continuation(&[Value::int(71)]))
                .unwrap();
            let resumed = loop {
                match restore.step(budget, || 0).unwrap() {
                    ResumeProgress::BudgetYield(usage) => {
                        yields += 1;
                        budget.work = usage.required_work.max(1);
                        budget.allocation_bytes = usage.required_allocation;
                    }
                    ResumeProgress::Complete(task) => break task,
                }
            };
            drop(restore);
            resumed
        };
        assert!(yields > 3);
        assert_eq!(resumed, current_task);
        for root in roots {
            current.heap_mut().release_root(root).unwrap();
        }
        let RunOutcome::HostCall(ticket) = next(&mut current, resumed) else {
            panic!("current waiting operation");
        };
        assert_eq!(
            current.host_arguments(ticket).unwrap()[0].as_int(),
            Some(30),
            "current prepare code must read preserved business state"
        );
        assert_eq!(ticket.owner, RootOwner(2));
        current
            .respond(ticket, HostResponse::Ready(Value::int(3)))
            .unwrap();
        let delivery = finish_flow(&mut current, resumed, current_boundary);
        assert_eq!(
            current.delivery_value(delivery).unwrap().as_int(),
            Some(176)
        );
        current.discard_delivery(delivery).unwrap();
        let prefix = current
            .start(
                function(&current, "prefix_count"),
                &[],
                RootOwner(5),
                bc::Context::Ordinary,
            )
            .unwrap();
        let RunOutcome::Returned(delivery) = next(&mut current, prefix) else {
            panic!()
        };
        assert_eq!(
            current.delivery_value(delivery).unwrap().as_int(),
            Some(0),
            "restoration cannot rerun business prefixes"
        );
        current.discard_delivery(delivery).unwrap();
        let boot = current
            .start(
                function(&current, "boot_count"),
                &[],
                RootOwner(5),
                bc::Context::Ordinary,
            )
            .unwrap();
        let RunOutcome::Returned(delivery) = next(&mut current, boot) else {
            panic!()
        };
        assert_eq!(current.delivery_value(delivery).unwrap().as_int(), Some(1));
        current.discard_delivery(delivery).unwrap();
    }
}

#[test]
fn forged_metadata_and_wrong_keys_never_produce_an_executable_plan() {
    let compiled = compile(&format!("{HEADER}{A}"), 1).unwrap();
    let mut forged = compiled.resume.clone();
    let leaf = forged
        .flows
        .iter_mut()
        .find(|flow| flow.key.0 == "leaf")
        .unwrap();
    leaf.points[0].call.0 += 1;
    assert!(
        compiled
            .program
            .verify_resume(forged, &[], ResumeLimits::default())
            .is_err()
    );
    let table = compiled
        .program
        .verify_resume(compiled.resume, &[], ResumeLimits::default())
        .unwrap();
    assert!(
        table
            .verify_plan(&ContinuationDescriptor {
                frames: vec![FrameDescriptor {
                    flow: "leaf".into(),
                    site: ResumeSite::Point("missing".into())
                }]
            })
            .is_err()
    );
    assert_eq!(ResumeTable::default().version, 1);
}

#[test]
fn point_after_for_closes_iterator_before_resume_boundary() {
    let source = r#"fn main(){
        flow("x");
        let options = [];
        for entry in [1, 2] { options.append(entry); }
        let count = options.len();
        point("choose");
        wait(count);
    }"#;
    let compiled = compile(&format!("{HEADER}{source}"), 1).unwrap();
    assert_eq!(compiled.resume.flows[0].points[0].key.0, "choose");
    compiled
        .program
        .verify_resume(compiled.resume, &[], ResumeLimits::default())
        .unwrap();

    let inside = r#"fn main(){
        flow("x");
        for entry in [1, 2] { point("choose"); wait(entry); }
    }"#;
    let error = compile(&format!("{HEADER}{inside}"), 1).unwrap_err();
    assert!(format!("{:?}", error.error).contains("active for loop"));
}

#[test]
fn unsupported_live_values_and_regions_report_source_diagnostics() {
    for body in [
        "fn main(){flow(\"x\");let read:fn()->int=||1;point(\"p\");let answer=wait(1);read()+answer}",
        "fn main(){flow(\"x\");try {point(\"p\");wait(1);}catch error {0}}",
        "fn helper(){prefixes+=1;1} fn main(){flow(\"x\");point(\"p\");let answer=wait(helper());answer}",
    ] {
        let error = compile(&format!("{HEADER}{body}"), 1).unwrap_err();
        assert!(!error.error.span.is_empty(), "{:?}", error.error);
    }
}

#[test]
fn single_module_static_calls_are_proved_from_the_real_closure_initializer() {
    let source = r#"use std::resume::{flow,point};
        fn child(value:int){flow("child");value+1}
        fn main(value:int){flow("root");point("child");let answer=child(value);answer+2}
    "#;
    let analyzed =
        aside_compiler::analyze_module(source, &AnalysisOptions::default(), &mut resolver())
            .unwrap();
    assert!(analyzed.is_ok(), "{:?}", analyzed.diagnostics);
    let compiled = lowering::compile(
        &analyzed.into_module().unwrap(),
        &Default::default(),
        &[],
        Default::default(),
    )
    .unwrap();
    assert!(
        compiled.program.package().modules[0]
            .globals
            .iter()
            .all(|global| global.linked_function.is_none())
    );
    let root = compiled
        .resume
        .flows
        .iter()
        .find(|flow| flow.key.0 == "root")
        .unwrap();
    let child = compiled
        .resume
        .flows
        .iter()
        .find(|flow| flow.key.0 == "child")
        .unwrap();
    assert_eq!(
        static_call_target(&compiled.program, root.function, root.returns[0].call, &[]).unwrap(),
        child.function
    );
    compiled
        .program
        .verify_resume(compiled.resume.clone(), &[], ResumeLimits::default())
        .unwrap();

    let dynamic = source
        .replace(
            "fn main(value:int){",
            "let mut selected=child; fn main(value:int){",
        )
        .replace("answer=child(value)", "answer=selected(value)");
    let analyzed =
        aside_compiler::analyze_module(&dynamic, &AnalysisOptions::default(), &mut resolver())
            .unwrap();
    assert!(analyzed.is_ok(), "{:?}", analyzed.diagnostics);
    let error = lowering::compile(
        &analyzed.into_module().unwrap(),
        &Default::default(),
        &[],
        Default::default(),
    )
    .unwrap_err();
    assert!(!error.span.is_empty());
    assert!(error.detail.contains("mutable global callee"), "{error}");
}

#[test]
fn one_point_marker_classifies_cross_module_flow_calls() {
    let inputs = [
        SourceModule {
            canonical_path: "chapter".into(),
            source_path: None,
            version: "1".into(),
            source: r#"use std::resume::flow;
                pub fn child(value:int){flow("child");value+1}"#
                .into(),
        },
        SourceModule {
            canonical_path: "game".into(),
            source_path: None,
            version: "1".into(),
            source: r#"use chapter::child as current; use std::resume::{flow,point};
                let mut calls=0; fn prepare(){calls+=1;7}
                fn main(){flow("parent");point("child-call");let result=current(prepare());result}"#
                .into(),
        },
    ];
    let mut analysis =
        analyze_project(&inputs, &AnalysisOptions::default(), &mut resolver()).unwrap();
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
    let plan = std::mem::take(&mut analysis.plan);
    let modules = analysis.into_modules().unwrap();
    let package: bc::BytecodePackage = Default::default();
    let seed = bc::verify(package, Default::default()).unwrap();
    let compiled = lowering::compile_project(
        modules,
        &plan,
        &seed,
        &[],
        &[vec![], vec![]],
        &[],
        Default::default(),
    )
    .unwrap();
    let parent = compiled
        .resume
        .flows
        .iter()
        .find(|flow| flow.key.0 == "parent")
        .unwrap();
    assert!(parent.points.is_empty());
    assert_eq!(parent.returns.len(), 1);
    assert_eq!(parent.returns[0].key.0, "child-call");
    let child = compiled
        .resume
        .flows
        .iter()
        .find(|flow| flow.key.0 == "child")
        .unwrap();
    assert_eq!(parent.returns[0].callee, child.function);
    compiled
        .program
        .verify_resume(compiled.resume, &[], Default::default())
        .unwrap();
}

#[test]
fn interaction_and_flow_calls_share_one_point_key_namespace() {
    let source = format!(
        r#"{HEADER}
        fn child(value:int){{flow("child");value+1}}
        fn main(){{flow("parent");point("interaction");wait(1);point("child-call");let result=child(2);result}}"#
    );
    let compiled = compile(&source, 1).unwrap();
    let mut forged = compiled.resume.clone();
    let parent = forged
        .flows
        .iter_mut()
        .find(|flow| flow.key.0 == "parent")
        .unwrap();
    assert_eq!((parent.points.len(), parent.returns.len()), (1, 1));
    parent.returns[0].key = ReturnKey(parent.points[0].key.0.clone());
    let error = compiled
        .program
        .verify_resume(forged, &[], Default::default())
        .unwrap_err();
    assert!(
        error.detail.contains("duplicate or empty point key"),
        "{error}"
    );

    let error = compile(&source.replace("child-call", "interaction"), 1).unwrap_err();
    assert!(!error.error.span.is_empty());
    assert!(
        error.error.detail.contains("point keys must be unique"),
        "{error}"
    );
}

#[test]
fn detached_capture_cursor_interleaves_roots_and_rejects_an_advanced_wait() {
    let compiled = compile(&format!("{HEADER}{A}"), 1).unwrap();
    let mut current = vm(&compiled, 1);
    let (task, boundary) = start_flow(&mut current, 10);
    let RunOutcome::HostCall(ticket) = next(&mut current, task) else {
        panic!()
    };
    current
        .respond(ticket, HostResponse::Pending(ticket.wait_id()))
        .unwrap();
    let table = current
        .program()
        .verify_resume(compiled.resume, &[], ResumeLimits::default())
        .unwrap();
    assert!(
        current
            .capture_resume_cursor(&table, task, FrameId(u64::MAX))
            .is_err()
    );
    let mut cursor = current
        .capture_resume_cursor(&table, task, boundary)
        .unwrap();
    assert!(matches!(
        cursor.step(&current, 0).unwrap(),
        CaptureProgress::BudgetYield
    ));
    let mut roots = Vec::new();
    loop {
        match cursor.step(&current, 1).unwrap() {
            CaptureProgress::Binding(binding) => {
                let value = binding.value;
                roots.push(
                    current
                        .heap_mut()
                        .add_root(RootOwner(3), RootKind::Host, value)
                        .unwrap(),
                );
            }
            CaptureProgress::BudgetYield => {}
            CaptureProgress::Complete => break,
        }
    }
    assert_eq!(roots.len(), 3);
    current
        .complete(ticket.wait_id(), Completion::Ready(Value::int(3)))
        .unwrap();
    assert!(cursor.step(&current, 1).is_err());
    for root in roots {
        current.heap_mut().release_root(root).unwrap();
    }
}

#[test]
fn partial_restoration_cancellation_and_bad_roots_retire_the_candidate_task() {
    let compiled = compile(&format!("{HEADER}{B}"), 2).unwrap();
    let descriptor = ContinuationDescriptor {
        frames: vec![
            FrameDescriptor {
                flow: "root".into(),
                site: ResumeSite::Return("child".into()),
            },
            FrameDescriptor {
                flow: "middle".into(),
                site: ResumeSite::Return("leaf".into()),
            },
            FrameDescriptor {
                flow: "leaf".into(),
                site: ResumeSite::Point("line".into()),
            },
        ],
    };
    for mode in 0..3 {
        let mut current = vm(&compiled, 2);
        let (_, ticket) = flow_ticket(&mut current, 999);
        let table = current
            .program()
            .verify_resume(compiled.resume.clone(), &[], ResumeLimits::default())
            .unwrap();
        let plan = table.verify_plan(&descriptor).unwrap();
        let roots: Vec<_> = (0..3)
            .map(|_| {
                current
                    .heap_mut()
                    .add_root(RootOwner(3), RootKind::Host, Value::int(1))
                    .unwrap()
            })
            .collect();
        let values: Vec<_> = roots
            .iter()
            .map(|&root| ResumeFrameValues {
                bindings: vec![ResumeValue {
                    key: "kept".into(),
                    root,
                }],
            })
            .collect();
        let zero = SliceBudget {
            instructions: 0,
            work: 0,
            allocation_bytes: 0,
            deadline_ns: None,
        };
        if mode == 2 {
            let mut restore = current
                .restore_resume(ticket, &plan, &values, continuation(&[]))
                .unwrap();
            let ResumeProgress::BudgetYield(usage) = restore.step(zero, || 0).unwrap() else {
                panic!()
            };
            assert!(matches!(
                restore
                    .step(
                        SliceBudget {
                            work: usage.required_work,
                            allocation_bytes: usage.required_allocation,
                            ..zero
                        },
                        || 0
                    )
                    .unwrap(),
                ResumeProgress::BudgetYield(_)
            ));
            drop(restore);
        } else {
            let mut restore = current
                .restore_resume_cursor(ticket, plan, values, continuation(&[]))
                .unwrap();
            if mode == 0 {
                let ResumeProgress::BudgetYield(usage) =
                    restore.step(&mut current, zero, || 0).unwrap()
                else {
                    panic!()
                };
                assert!(matches!(
                    restore
                        .step(
                            &mut current,
                            SliceBudget {
                                work: usage.required_work,
                                allocation_bytes: usage.required_allocation,
                                ..zero
                            },
                            || 0
                        )
                        .unwrap(),
                    ResumeProgress::BudgetYield(_)
                ));
                restore.cancel(&mut current).unwrap();
                assert!(
                    restore
                        .step(&mut current, SliceBudget::default(), || 0)
                        .is_err()
                );
            } else {
                current.heap_mut().release_root(roots[0]).unwrap();
                assert!(
                    restore
                        .step(&mut current, SliceBudget::default(), || 0)
                        .is_err()
                );
                assert!(
                    restore
                        .step(&mut current, SliceBudget::default(), || 0)
                        .is_err()
                );
            }
            assert!(restore.completed_task().is_none());
        }
        let partial = current.task_id().expect("the partial task was allocated");
        assert!(matches!(
            current.run_slice(partial, SliceBudget::default(), || 0),
            Err(VmError::Idle)
        ));
        for (index, root) in roots.into_iter().enumerate() {
            if mode == 1 && index == 0 {
                continue;
            }
            assert_eq!(current.heap().root_value(root).unwrap().as_int(), Some(1));
            current.heap_mut().release_root(root).unwrap();
        }
    }
}

#[test]
fn binding_keys_only_require_unique_names_among_live_values_at_each_site() {
    let allowed = r#"fn main(value:int){
        flow("one"); {let kept=1;kept;}
        let kept=value;
        point("first"); wait(kept);
        point("line"); let answer=wait(kept); kept+answer
    }"#;
    let compiled = compile(&format!("{HEADER}{allowed}"), 1).unwrap();
    let flow = compiled
        .resume
        .flows
        .iter()
        .find(|flow| flow.key.0 == "one")
        .unwrap();
    assert_eq!(flow.points[0].layout, flow.points[1].layout);
    assert_eq!(
        flow.layouts[flow.points[0].layout as usize]
            .bindings
            .iter()
            .filter(|binding| binding.key.0 == "kept")
            .count(),
        1
    );
    let ambiguous = r#"fn main(value:int){
        flow("one"); let kept=value;
        {let kept=2; point("line"); let answer=wait(kept); kept+answer;}
        kept
    }"#;
    let error = compile(&format!("{HEADER}{ambiguous}"), 1).unwrap_err();
    assert!(!error.error.span.is_empty());
    assert!(
        error.error.detail.contains("live resume binding names"),
        "{error:?}"
    );
}

#[test]
fn read_only_point_preparation_supports_real_text_and_scalar_opcodes() {
    let source = r#"fn main(value:int){flow("one");let kept=value;
        point("line");let answer=wait(f"value {+kept % 7}: {kept is int}");
        kept+answer
    }"#;
    let compiled = compile(&format!("{HEADER}{source}"), 1).unwrap();
    let flow = compiled
        .resume
        .flows
        .iter()
        .find(|flow| flow.key.0 == "one")
        .unwrap();
    let point = &flow.points[0];
    let layout = analyze_site(
        &compiled.program,
        flow.function,
        Some(point.prepare),
        point.call,
        &[],
        ResumeLimits::default(),
    )
    .unwrap();
    for limits in [
        ResumeLimits {
            work: 0,
            ..Default::default()
        },
        ResumeLimits {
            allocation_bytes: 0,
            ..Default::default()
        },
        ResumeLimits {
            allocation_bytes: layout.capacity_bytes - 1,
            ..Default::default()
        },
    ] {
        let error = analyze_site(
            &compiled.program,
            flow.function,
            Some(point.prepare),
            point.call,
            &[],
            limits,
        )
        .unwrap_err();
        assert!(error.detail.contains("limit"), "{error}");
        assert_eq!(error.function, Some(flow.function));
        assert_eq!(error.pc, Some(point.call));
    }
    let mut current = vm(&compiled, 1);
    let task = current
        .start(
            function(&current, "main"),
            &[Value::int(10)],
            RootOwner(2),
            bc::Context::Ordinary,
        )
        .unwrap();
    let RunOutcome::HostCall(ticket) = next(&mut current, task) else {
        panic!()
    };
    let value = current.host_arguments(ticket).unwrap()[0];
    let mut bytes = [0; 64];
    let count = current.heap().read_bytes(value, 0, &mut bytes).unwrap();
    assert_eq!(&bytes[..count], b"value 3: true");
    current
        .respond(ticket, HostResponse::Ready(Value::int(2)))
        .unwrap();
    let RunOutcome::Returned(delivery) = next(&mut current, task) else {
        panic!()
    };
    assert_eq!(current.delivery_value(delivery).unwrap().as_int(), Some(12));
}

fn sample_resume(
    vm: &mut Vm,
    compiled: &lowering::ProjectCompilation,
    ticket: HostTicket,
) -> (ResumeRestoreCursor, Vec<aside_vm::RootToken>) {
    let descriptor = ContinuationDescriptor {
        frames: vec![
            FrameDescriptor {
                flow: "root".into(),
                site: ResumeSite::Return("child".into()),
            },
            FrameDescriptor {
                flow: "middle".into(),
                site: ResumeSite::Return("leaf".into()),
            },
            FrameDescriptor {
                flow: "leaf".into(),
                site: ResumeSite::Point("line".into()),
            },
        ],
    };
    let table = vm
        .program()
        .verify_resume(compiled.resume.clone(), &[], ResumeLimits::default())
        .unwrap();
    let plan = table.verify_plan(&descriptor).unwrap();
    assert_eq!(plan.entry_function(), function(vm, "main"));
    let roots: Vec<_> = [11, 22, 25]
        .into_iter()
        .map(|value| {
            vm.heap_mut()
                .add_root(RootOwner(3), RootKind::Host, Value::int(value))
                .unwrap()
        })
        .collect();
    let values = roots
        .iter()
        .map(|&root| ResumeFrameValues {
            bindings: vec![ResumeValue {
                key: "kept".into(),
                root,
            }],
        })
        .collect();
    let cursor = vm
        .restore_resume_cursor(ticket, plan, values, continuation(&[Value::int(71)]))
        .unwrap();
    assert!(
        vm.host_arguments(ticket).is_err(),
        "the callback constructor consumed the issued ticket"
    );
    (cursor, roots)
}
fn complete_resume(vm: &mut Vm, cursor: &mut ResumeRestoreCursor) -> TaskId {
    loop {
        match cursor.step(vm, SliceBudget::default(), || 0).unwrap() {
            ResumeProgress::BudgetYield(_) => {}
            ResumeProgress::Complete(task) => return task,
        }
    }
}

#[test]
fn restored_callback_fault_cancel_and_finish_return_to_the_current_native() {
    for mode in 0..3 {
        let source = if mode == 0 {
            B.replace("kept += 8; kept + answer", "throw 19;")
        } else {
            B.into()
        };
        let compiled = compile(&format!("{HEADER}{source}"), 2).unwrap();
        let mut current = vm(&compiled, 2);
        let (task, boundary) = flow_ticket(&mut current, 999);
        let (mut restore, roots) = sample_resume(&mut current, &compiled, boundary);
        assert_eq!(complete_resume(&mut current, &mut restore), task);
        for root in roots {
            current.heap_mut().release_root(root).unwrap();
        }
        let RunOutcome::HostCall(waiting) = next(&mut current, task) else {
            panic!("current point")
        };
        assert_eq!(waiting.owner, boundary.owner);
        assert_eq!(waiting.context, boundary.context);
        assert_eq!(waiting.effects, boundary.effects);
        current
            .respond(waiting, HostResponse::Pending(waiting.wait_id()))
            .unwrap();
        match mode {
            0 => current
                .complete(waiting.wait_id(), Completion::Ready(Value::int(3)))
                .unwrap(),
            1 => assert!(current.cancel_host_callback(task, boundary.frame).unwrap()),
            _ => assert!(
                current
                    .finish_host_callback(task, boundary.frame, Value::int(9))
                    .unwrap()
            ),
        }
        assert!(matches!(
            current.complete(waiting.wait_id(), Completion::Ready(Value::int(3))),
            Err(VmError::InvalidWait)
        ));
        let RunOutcome::HostCall(callback) = next(&mut current, task) else {
            panic!("current callback terminal")
        };
        assert_eq!(callback.frame, boundary.frame);
        assert_eq!(callback.continuation.unwrap().version, 7);
        assert_eq!(
            current.host_continuation(callback).unwrap()[0].as_int(),
            Some(71)
        );
        if mode == 0 {
            assert_eq!(callback.input, NativeInput::CallbackFault);
            let fault = current.host_callback_fault(callback).unwrap().unwrap();
            assert_eq!(fault.kind, FaultKind::Thrown);
            assert_eq!(fault.location.code.version, 2);
            assert_eq!(
                current.heap().fault_value(fault.id).unwrap().as_int(),
                Some(19)
            );
            current
                .respond(callback, HostResponse::ForwardFault(fault.id))
                .unwrap();
        } else if mode == 1 {
            assert_eq!(callback.input, NativeInput::CallbackCancelled);
            current
                .respond_language_error(callback, FaultKind::Cancelled, Value::NULL)
                .unwrap();
        } else {
            assert_eq!(callback.input, NativeInput::CallbackReturned);
            let result = current.host_resume_value(callback).unwrap();
            assert_eq!(result.as_int(), Some(9));
            current
                .respond(callback, HostResponse::Ready(result))
                .unwrap();
        }
        let result = next(&mut current, task);
        if mode == 2 {
            let RunOutcome::Returned(delivery) = result else {
                panic!("{result:?}")
            };
            assert_eq!(current.delivery_value(delivery).unwrap().as_int(), Some(9));
            current.discard_delivery(delivery).unwrap();
        } else {
            let RunOutcome::Fault(delivery) = result else {
                panic!("{result:?}")
            };
            assert_eq!(
                current.delivery_fault(delivery).unwrap().unwrap().kind,
                if mode == 0 {
                    FaultKind::Thrown
                } else {
                    FaultKind::Cancelled
                }
            );
            current.discard_delivery(delivery).unwrap();
        }
    }
}

#[test]
fn callback_restore_inherits_the_current_effect_mask() {
    let effects = bc::Effects(bc::Effects::ALLOCATE | bc::Effects::HOST | bc::Effects::WAIT);
    let compiled = compile_with_effects(&format!("{HEADER}{B}"), 2, effects).unwrap();
    let mut current = vm(&compiled, 2);
    let (task, boundary) = flow_ticket(&mut current, 999);
    assert_eq!(boundary.effects, effects);
    let (mut restore, roots) = sample_resume(&mut current, &compiled, boundary);
    assert_eq!(complete_resume(&mut current, &mut restore), task);
    for root in roots {
        current.heap_mut().release_root(root).unwrap();
    }
    let RunOutcome::HostCall(waiting) = next(&mut current, task) else {
        panic!("current point")
    };
    assert_eq!(waiting.effects, effects);
    current
        .respond(waiting, HostResponse::Ready(Value::int(3)))
        .unwrap();
    let RunOutcome::HostCall(callback) = next(&mut current, task) else {
        panic!("effect fault")
    };
    assert_eq!(callback.input, NativeInput::CallbackFault);
    assert_eq!(
        current.host_callback_fault(callback).unwrap().unwrap().kind,
        FaultKind::Effect
    );
}
