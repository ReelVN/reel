use aside_host::{CallContext, Error, NativeContract, Properties, PropertySchema, native};
use aside_vm::{Heap, HeapError, RootOwner, SessionEpoch, Value, bytecode as bc, executor::*};

/// Add two integers, clamping a nonpositive result to zero.
///
/// ```aside
/// fn main() { add(3, right: 4) }
/// ```
#[native(symbol = "sample::add")]
fn add(#[aside(name = "left")] __aside_vm: i64, #[aside(default = 2)] right: i64) -> i64 {
    let Some(result) = __aside_vm.checked_add(right) else {
        return -1;
    };
    if result > 0 { result } else { 0 }
}

#[native(symbol = "sample::unavailable")]
#[cfg(any())]
fn unavailable() -> MissingType {
    unreachable!()
}
#[derive(Default)]
struct Host {
    calls: usize,
}
#[native(symbol = "sample::decorate", capabilities = ["sample.text"], effects = bc::Effects::ALL)]
fn decorate(
    #[aside(context)] context: &mut CallContext<'_, Host>,
    value: String,
    #[aside(default = "!")] suffix: String,
) -> Result<String, Error> {
    if value == "reject" {
        return Err(HeapError::TypeMismatch.into());
    }
    context.host.calls += 1;
    Ok(format!("{value}{suffix}"))
}

fn nonnegative(_: &Heap, value: Value) -> Result<(), HeapError> {
    if value.as_float().is_some_and(|v| v >= 0. && v.is_finite()) {
        Ok(())
    } else {
        Err(HeapError::TypeMismatch)
    }
}
#[derive(Properties)]
enum Field {
    /// Horizontal position in logical pixels.
    #[aside(ty = Float, default = 0.0, animatable, validate = nonnegative)]
    X,
    #[aside(ty = Bool, default = true)]
    Visible,
    #[aside(ty = String, name = "id", default = "stable", readonly)]
    Identity,
}

#[native(symbol = "sample::gauge")]
fn gauge(#[aside(context)] context: &mut CallContext<'_, Host>) -> Result<Value, Error> {
    let heap = context.vm.heap_mut();
    let fields = heap.alloc_table()?;
    for (index, property) in Field::PROPERTIES.iter().enumerate() {
        let key = heap.alloc_string(property.name)?;
        let value = match (property.default)() {
            bc::Const::FloatBits(bits) => Value::float(f64::from_bits(bits)),
            bc::Const::Bool(value) => Value::bool(value),
            bc::Const::Utf8(value) => heap.alloc_string(&value)?,
            _ => unreachable!(),
        };
        heap.table_store(fields, index as u32, key, value)?;
    }
    Ok(heap.alloc_host(fields, &[], Some(Field::validate_host))?)
}

#[native(symbol = "sample::collect")]
fn collect(#[aside(rest)] items: Value) -> Value {
    items
}

#[native(symbol = "sample::nullable")]
fn nullable(value: Option<String>) -> Option<String> {
    value
}

#[derive(Properties)]
enum PreciseField {
    /// A finite nonnegative floating-point value.
    #[aside(ty = Any, doc_type = aside_host::aside_compiler::semantic::TypeHint::Float,
        default = 0.0, validate = nonnegative)]
    Value,
}

#[test]
fn source_schema_preserves_precise_types_defaults_and_definition_ranges() {
    use aside_host::aside_compiler::semantic::TypeHint;
    use aside_host::source::{Api, Declaration, Member, NamedType};
    let binding = nullable::binding();
    let docs = binding.into_documentation();
    assert_eq!(docs.contract.parameters[0].ty, bc::BuiltinType::Any);
    assert_eq!(
        docs.signature.parameters[0].ty,
        TypeHint::Union(vec![TypeHint::String, TypeHint::Null])
    );
    assert_eq!(docs.signature.result, docs.signature.parameters[0].ty);
    let property = &PreciseField::PROPERTIES[PreciseField::Value.index()];
    assert_eq!(property.ty, bc::BuiltinType::Any);
    assert_eq!(property.source_type(), TypeHint::Float);
    let ty = NamedType::new(
        "sample::types::Gauge",
        "A real host value.",
        vec![Member::property(
            "sample::types::Gauge",
            aside_host::PropertyDocumentation {
                property,
                markdown: PreciseField::documentation(0),
            },
        )],
    );
    let api = Api {
        types: vec![ty],
        functions: vec![Declaration::native(
            &add::binding().contract,
            add::DOCUMENTATION,
        )],
        ..Default::default()
    };
    let generated = api.documents("sample");
    assert_eq!(generated.documents.len(), 2);
    assert!(
        generated.documents[0]
            .text
            .contains("let mut value: float = 0.0;")
    );
    assert!(
        generated.documents[1]
            .text
            .contains("fn add(left: int, right: int = 2) -> int;")
    );
    for location in &generated.locations {
        let doc = generated
            .documents
            .iter()
            .find(|doc| doc.uri == location.uri)
            .unwrap();
        assert_eq!(
            &doc.text[usize::from(location.range.start())..usize::from(location.range.end())],
            location.declaration.symbol.name
        );
    }
}

#[test]
fn declaration_documents_preserve_bound_and_inherited_identities() {
    use aside_compiler::semantic::TypeHint;
    use aside_host::source::{Api, Declaration, Member, NamedType, named, type_name};
    assert_eq!(
        type_name(&TypeHint::Callable(
            vec![TypeHint::Int],
            Box::new(TypeHint::Bool)
        )),
        "fn(int) -> bool"
    );
    assert_eq!(type_name(&TypeHint::Tuple(vec![TypeHint::Int])), "(int,)");
    assert_eq!(
        type_name(&TypeHint::Union(vec![
            TypeHint::Callable(vec![], Box::new(TypeHint::Int)),
            TypeHint::Null
        ])),
        "(fn() -> int) | null"
    );
    let native = Declaration::native(&add::binding().contract, add::DOCUMENTATION);
    let base = NamedType::new(
        "sample::types::Node",
        "Base node.",
        vec![Member::field(
            "sample::types::Node",
            "x",
            TypeHint::Float,
            Some(TypeHint::Float),
            "Position.",
            None,
        )],
    );
    let derived = NamedType::new(
        "sample::types::Speech",
        "Speech.",
        vec![Member::method(native.clone().bound("append"))],
    )
    .extends(&base);
    let api = Api {
        types: vec![
            base,
            derived,
            NamedType::new("other::types::Node", "Other node.", Vec::new()),
        ],
        functions: vec![native],
        ..Default::default()
    };
    let documents = api.documents("sample");
    let speech = documents
        .documents
        .iter()
        .find(|document| document.uri.ends_with("sample/types/Speech.aside"))
        .unwrap();
    assert!(speech.text.contains("class Speech: sample::types::Node"));
    assert!(speech.text.contains("fn append(right: int = 2) -> int;"));
    assert!(!speech.text.contains("let mut x"));
    let bound = documents
        .locations
        .iter()
        .find(|location| location.declaration.symbol.name == "append")
        .unwrap();
    assert_eq!(bound.uri, speech.uri);
    let declarations = documents
        .locations
        .iter()
        .filter(|location| {
            location.declaration.symbol.reference == bound.declaration.symbol.reference
        })
        .collect::<Vec<_>>();
    assert_eq!(declarations.len(), 2);
    assert!(declarations.iter().any(|location| {
        location.declaration.symbol.name == "append"
            && location
                .declaration
                .symbol
                .signature
                .as_ref()
                .unwrap()
                .parameters
                .len()
                == 1
    }));
    assert!(declarations.iter().any(|location| {
        location.declaration.symbol.name == "add"
            && location
                .declaration
                .symbol
                .signature
                .as_ref()
                .unwrap()
                .parameters
                .len()
                == 2
    }));
    let inherited = documents
        .locations
        .iter()
        .find(|location| location.declaration.symbol.name == "x")
        .unwrap();
    assert!(inherited.uri.ends_with("sample/types/Node.aside"));
    assert!(
        documents
            .documents
            .iter()
            .any(|document| document.uri.ends_with("other/types/Node.aside"))
    );
    assert_eq!(
        api.is_subtype("sample::types::Speech", "sample::types::Node"),
        Some(true)
    );
    assert_eq!(
        api.is_subtype("sample::types::Speech", "other::types::Node"),
        Some(false)
    );
    assert_eq!(
        api.member("sample::types::Speech", "x").unwrap().symbol.ty,
        TypeHint::Float
    );
    assert_eq!(named("sample::types::Speech"), api.types[1].symbol().ty);
    let base_reference = speech
        .references
        .iter()
        .find(|reference| {
            &speech.text[usize::from(reference.range.start())..usize::from(reference.range.end())]
                == "Node"
        })
        .unwrap();
    let target = &documents.locations[base_reference.declaration];
    assert_eq!(target.declaration.symbol.ty, named("sample::types::Node"));
    assert!(target.declaration.symbol.type_only);
    assert_eq!(target.uri, inherited.uri);
}

#[test]
fn standalone_macro_host_compiles_encodes_admits_and_calls_rust() {
    let add = add::binding();
    assert!(add.documentation.contains("Add two integers"));
    assert!(add.documentation.contains("add(3, right: 4)"));
    assert_eq!(
        Field::documentation(Field::X.index()),
        " Horizontal position in logical pixels.\n"
    );
    assert_eq!(Field::documentation(Field::Visible.index()), "");
    let decorate = decorate::binding();
    let gauge = gauge::binding();
    let collect = collect::binding();
    let contracts = [
        add.contract.clone(),
        decorate.contract.clone(),
        gauge.contract.clone(),
        collect.contract.clone(),
    ];
    let capabilities = ["sample.text".to_string()];
    let module = aside_compiler::analyze_module(r#"fn dynamic(value) { value } fn main() {
        let f = add;
        if f(right: 8, left: 4) != 12 || add(3) != 5 { throw "binding"; }
        if f(...{right: 8, left: 4}) != 12 || add(...{left: 3}) != 5 { throw "named forwarding"; }
        if decorate("你好") != "你好!" || decorate(suffix: "?", value: "为什么") != "为什么?" { throw "text"; }
        let node = gauge();
        node.x = 2.5;
        node.visible = false;
        let mut rejected = 0;
        try { node.x = -1.0; } catch error { rejected += 1; }
        try { node.id = "new"; } catch error { rejected += 1; }
        try { decorate("reject"); } catch error { rejected += 1; }
        if rejected != 3 || node.x != 2.5 || node.visible || node.id != "stable" { throw "property schema"; }
        let nested = [7];
        let source = [1, nested];
        let packed = collect(...source, 3);
        if packed.len() != 3 || packed[0] != 1 || packed[2] != 3 { throw "native spread"; }
        packed[0] = 9;
        nested[0] = 8;
        if source[0] != 1 || packed[1][0] != 8 { throw "fresh outer array, shared elements"; }
        let a = collect();
        let b = collect();
        a.append(4);
        if b.len() != 0 { throw "shared empty rest"; }
        let gather = dynamic(collect);
        if gather(5, ...[6, 7]).len() != 3 { throw "dynamic native rest"; }
        let mut bad_rest = false;
        try { gather(items: [1]); } catch error { bad_rest = true; }
        if !bad_rest { throw "named rest admitted"; }
        true
    }"#, &aside_compiler::AnalysisOptions {
        hosts: contracts.iter().map(NativeContract::external).collect(), ..Default::default()
    }, &mut aside_compiler::semantic::NoModules).unwrap().into_module().unwrap();
    let compiled = aside_host::lower(
        &module,
        &contracts,
        &[],
        &[],
        &capabilities,
        Default::default(),
    )
    .unwrap();
    let encoded = bc::encode(compiled.program.package(), bc::Limits::default()).unwrap();
    let program = bc::VerifiedProgram::decode(&encoded, bc::Limits::default()).unwrap();
    let support = aside_host::runtime_support(&program, &contracts, &[], &capabilities).unwrap();
    let mut wrong_rest = contracts.to_vec();
    wrong_rest[3].rest = None;
    let wrong_support =
        aside_host::runtime_support(&program, &wrong_rest, &[], &capabilities).unwrap();
    assert!(
        Vm::load(
            bc::VerifiedProgram::decode(&encoded, bc::Limits::default()).unwrap(),
            wrong_support,
            VmLimits::default(),
            SessionEpoch(2)
        )
        .is_err(),
        "runtime admission must reject a different rest signature"
    );
    let entry = bc::FunctionId(
        program
            .package()
            .functions
            .iter()
            .position(|f| program.package().symbols[f.name.index()] == "main")
            .unwrap() as u32,
    );
    let mut vm = Vm::load(program, support, VmLimits::default(), SessionEpoch(1)).unwrap();
    let mut host = Host::default();
    for function in [compiled.entry, entry] {
        let task = vm
            .start(function, &[], RootOwner(1), bc::Context::Ordinary)
            .unwrap();
        loop {
            match vm.run_slice(task, SliceBudget::default(), || 0).unwrap() {
                RunOutcome::HostCall(ticket) => match ticket.native.0 {
                    0 => (add.invoke)(&mut (), &mut vm, ticket).unwrap(),
                    1 => {
                        if let Err(error) = (decorate.invoke)(&mut host, &mut vm, ticket) {
                            assert!(matches!(error, Error::Heap(HeapError::TypeMismatch)));
                            assert!(ticket.location.source.is_some());
                            vm.respond(
                                ticket,
                                HostResponse::Error(aside_vm::NewFault {
                                    value: Value::NULL,
                                    code: aside_vm::FaultCode::core(FaultKind::Type),
                                    cause: None,
                                }),
                            )
                            .unwrap();
                        }
                    }
                    2 => (gauge.invoke)(&mut host, &mut vm, ticket).unwrap(),
                    3 => (collect.invoke)(&mut (), &mut vm, ticket).unwrap(),
                    _ => unreachable!(),
                },
                RunOutcome::BudgetYield(_) => {}
                RunOutcome::Returned(delivery) => {
                    vm.discard_delivery(delivery).unwrap();
                    break;
                }
                RunOutcome::Fault(delivery) => panic!("{:?}", vm.delivery_fault(delivery).unwrap()),
                other => panic!("{other:?}"),
            }
        }
    }
    assert_eq!(host.calls, 2);
    assert_eq!(self::add(3, 2), 5, "the Rust function body is preserved");
    assert_eq!(Field::X.index(), 0);
    assert_eq!(Field::Visible.index(), 1);
    assert_eq!(Field::Identity.index(), 2);
    assert_eq!(Field::PROPERTIES[2].name, "id");
    assert!(Field::PROPERTIES[0].animatable);
    assert!(Field::validate(vm.heap(), 0, Value::float(2.)).is_ok());
    assert!(Field::validate(vm.heap(), 0, Value::float(-1.)).is_err());
    let id = vm.heap_mut().alloc_string("replacement").unwrap();
    assert!(Field::validate(vm.heap(), 2, id).is_err());
    let mut changed = contracts.to_vec();
    changed[0].parameters[1].default = Some(bc::Const::Int(99));
    assert!(aside_host::runtime_support(vm.program(), &changed, &[], &capabilities).is_err());
}

mod bound_native {
    use aside_host::{CallContext, Error, NativeContract, NativeParameter, native};
    use aside_vm::{
        GcBudget, GcPhase, RootOwner, SessionEpoch, Value, bytecode as bc, executor::*,
    };

    #[native(symbol = "sample::decorate")]
    fn decorate(
        receiver: Value,
        #[aside(default = "!")] suffix: String,
        #[aside(rest)] extras: Value,
        #[aside(context)] context: &mut CallContext<'_, ()>,
    ) -> Result<String, Error> {
        let receiver = if context.vm.heap().info(receiver)?.kind == aside_vm::ObjectKind::Host {
            context.vm.heap().host_payload(receiver, 0)?
        } else {
            receiver
        };
        let receiver = <String as aside_host::FromValue>::from_value(context.vm.heap(), receiver)?;
        let count = context.vm.heap().sequence_len(extras)?;
        Ok(if count == 0 {
            format!("{receiver}{suffix}")
        } else {
            format!("{receiver}{suffix}{count}")
        })
    }
    struct Host {
        decorate: bc::FunctionId,
        callable: bool,
    }
    #[native(symbol = "sample::bind")]
    fn bind(
        #[aside(context)] context: &mut CallContext<'_, Host>,
        receiver: String,
    ) -> Result<Value, Error> {
        let value = context.vm.heap_mut().alloc_string(&receiver)?;
        make_callable(
            context.vm,
            value,
            context.host.decorate,
            context.host.callable,
        )
    }
    fn make_callable(
        vm: &mut Vm,
        value: Value,
        function: bc::FunctionId,
        host: bool,
    ) -> Result<Value, Error> {
        if host {
            let fields = vm.heap_mut().alloc_table()?;
            let value = vm.heap_mut().alloc_host(fields, &[value], None)?;
            vm.bind_callable(value, function)?;
            Ok(value)
        } else {
            Ok(vm.bind_native(value, function)?)
        }
    }

    fn collect(vm: &mut Vm) {
        if vm.heap().phase() == GcPhase::Idle {
            vm.begin_collection().unwrap();
        }
        vm.gc_step(GcBudget {
            units: 1,
            ..Default::default()
        })
        .unwrap();
    }

    #[test]
    fn bound_native_shares_script_entry_and_callback_binding_across_gc() {
        for callable in [false, true] {
            exercise_callable(callable);
        }
    }
    fn exercise_callable(callable: bool) {
        let decorate = decorate::binding();
        let bind = bind::binding();
        let contracts = [
            decorate.contract.clone(),
            bind.contract.clone(),
            NativeContract {
                rest: None,
                symbol: "sample::relay".into(),
                abi_version: 1,
                parameters: vec![NativeParameter {
                    name: "callback".into(),
                    ty: bc::BuiltinType::Any,
                    default: None,
                }],
                result: bc::BuiltinType::String,
                effects: bc::Effects::ALL,
                required_capabilities: vec![],
            },
        ];
        let source = r#"fn main() {
        let method = bind("captured");
        if method() != "captured!" || method(suffix: "?") != "captured?" { throw "method arguments"; }
        if method("?", ...[1, 2]) != "captured?2" { throw "bound native rest"; }
        if method(...{suffix: "?"}) != "captured?" { throw "bound named spread"; }
        fn forward(callback, positional, named) { callback(...positional, ...named) }
        if forward(method, [], {suffix: "."}) != "captured." { throw "forwarded native"; }
        let mut order_rejected = false;
        try { method(...{suffix: "?"}, ...[1]); } catch error { order_rejected = true; }
        if !order_rejected { throw "rest after named"; }
        if relay(method) != "captured!" { throw "host callback"; }
        if relay(|| method(...{suffix: "."})) != "captured." { throw "callback named spread"; }
        let mut rejected = false;
        try { method(receiver: "replacement"); } catch error { rejected = true; }
        if !rejected { throw "overridden receiver"; }
        rejected = false;
        try { method(...{receiver: "replacement"}); } catch error { rejected = true; }
        if !rejected { throw "spread receiver"; }
        "ok"
    }
    fn invalid() { let method = bind("captured"); method(receiver: "replacement"); }
    "#;
        let module = aside_compiler::analyze_module(
            source,
            &aside_compiler::AnalysisOptions {
                hosts: contracts.iter().map(NativeContract::external).collect(),
                ..Default::default()
            },
            &mut aside_compiler::semantic::NoModules,
        )
        .unwrap()
        .into_module()
        .unwrap();
        let compiled =
            aside_host::lower(&module, &contracts, &[], &[], &[], Default::default()).unwrap();
        let bytes = bc::encode(compiled.program.package(), bc::Limits::default()).unwrap();
        let program = bc::VerifiedProgram::decode(&bytes, bc::Limits::default()).unwrap();
        let function = |name: &str| {
            bc::FunctionId(
                program
                    .package()
                    .functions
                    .iter()
                    .position(|f| program.package().symbols[f.name.index()] == name)
                    .unwrap() as u32,
            )
        };
        let main = function("main");
        let invalid = function("invalid");
        let mut host = Host {
            decorate: function("sample::decorate"),
            callable,
        };
        let support = aside_host::runtime_support(&program, &contracts, &[], &[]).unwrap();
        let mut vm = Vm::load(program, support, VmLimits::default(), SessionEpoch(1)).unwrap();
        let receiver = vm.heap_mut().alloc_string("entry").unwrap();
        let entry_method = make_callable(&mut vm, receiver, host.decorate, callable).unwrap();
        let method_root = vm
            .heap_mut()
            .add_root(RootOwner(1), aside_vm::RootKind::Host, entry_method)
            .unwrap();
        if !callable {
            assert!(
                vm.heap_mut()
                    .set_field(entry_method, 3, Value::NULL)
                    .is_err()
            );
        }
        assert!(
            vm.bind_callable(entry_method, main).is_err(),
            "only admitted native functions are callable adapters"
        );
        for entry in [Some(compiled.entry), Some(main), None, Some(invalid)] {
            let task = match entry {
                Some(function) => vm.start(function, &[], RootOwner(1), bc::Context::Ordinary),
                None => {
                    let suffix = vm.heap_mut().alloc_string("?").unwrap();
                    vm.start_closure(
                        entry_method,
                        &[suffix, Value::int(11)],
                        RootOwner(1),
                        bc::Context::Ordinary,
                    )
                }
            }
            .unwrap();
            loop {
                let result = vm
                    .run_slice(
                        task,
                        SliceBudget {
                            instructions: 1,
                            ..Default::default()
                        },
                        || 0,
                    )
                    .unwrap();
                collect(&mut vm);
                match result {
                    RunOutcome::BudgetYield(_) => {}
                    RunOutcome::HostCall(ticket) => match ticket.native.0 {
                        0 => (decorate.invoke)(&mut (), &mut vm, ticket).unwrap(),
                        1 => (bind.invoke)(&mut host, &mut vm, ticket).unwrap(),
                        2 if ticket.input == NativeInput::Entry => {
                            let callback = vm.host_arguments(ticket).unwrap()[0];
                            vm.respond(
                                ticket,
                                HostResponse::CallScript {
                                    closure: callback,
                                    arguments: &[],
                                    continuation: Continuation {
                                        version: 1,
                                        stage: 0,
                                        values: &[],
                                        faults: FaultRetention::Preserve,
                                    },
                                },
                            )
                            .unwrap();
                        }
                        2 => {
                            let value = vm.host_callback_value(ticket).unwrap();
                            vm.respond(ticket, HostResponse::Ready(value)).unwrap();
                        }
                        _ => unreachable!(),
                    },
                    RunOutcome::Returned(delivery) => {
                        assert_ne!(entry, Some(invalid));
                        if entry != Some(compiled.entry) {
                            let value = vm.delivery_value(delivery).unwrap();
                            let actual =
                                <String as aside_host::FromValue>::from_value(vm.heap(), value)
                                    .unwrap();
                            assert_eq!(actual, if entry.is_none() { "entry?1" } else { "ok" });
                        }
                        vm.discard_delivery(delivery).unwrap();
                        break;
                    }
                    RunOutcome::Fault(delivery) => {
                        assert_eq!(entry, Some(invalid));
                        let fault = vm.delivery_fault(delivery).unwrap().unwrap();
                        let span = fault.location.source.unwrap();
                        assert!(
                            source[span.start as usize..span.end as usize]
                                .contains("method(receiver:")
                        );
                        vm.discard_delivery(delivery).unwrap();
                        break;
                    }
                    other => panic!("{other:?}"),
                }
            }
        }
        vm.heap_mut().release_root(method_root).unwrap();
    }
}

mod editor_types {
    //! Editor-retained identities must remain executable through the project linker.
    use aside_compiler::{
        AnalysisOptions,
        semantic::{NoModules, SourceModule, analyze_project},
    };
    use aside_vm::{GcBudget, GcPhase, RootOwner, SessionEpoch, bytecode as bc, executor::*};

    #[test]
    fn imported_class_reexports_and_bound_static_aliases_execute_in_source_and_asb() {
        let source = r#"
        use sample::api::{Counter as Imported};
        use sample::model::singleton;
        fn main() {
            let construct = Imported;
            let counter: Imported = construct(start: 5);
            let add = counter.add;
            let zero = Imported.zero;
            if counter.value != 5 || add(amount: 3) != 8 || zero() != 0 || singleton.value != 9 {
                throw "imported class identity";
            }
            counter.add() + Imported.zero()
        }
    "#;
        let modules = [
            SourceModule {
                canonical_path: "sample::model".into(),
                source_path: Some("model.aside".into()),
                version: "1".into(),
                source: r#"
            pub class Counter {
                let value: int;
                fn init(self, start: int = 4) { self.value = start; }
                fn add(self, amount: int = 2) -> int { self.value + amount }
                static fn zero() -> int { 0 }
            }
            pub let singleton: Counter = Counter(start: 9);
        "#
                .into(),
            },
            SourceModule {
                canonical_path: "sample::api".into(),
                source_path: Some("api.aside".into()),
                version: "1".into(),
                source: "pub use sample::model::Counter;".into(),
            },
            SourceModule {
                canonical_path: "sample::main".into(),
                source_path: Some("main.aside".into()),
                version: "1".into(),
                source: source.into(),
            },
        ];
        let mut analysis =
            analyze_project(&modules, &AnalysisOptions::default(), &mut NoModules).unwrap();
        assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
        let plan = std::mem::take(&mut analysis.plan);
        let modules = analysis.into_modules().unwrap();
        for encoded in [false, true] {
            let compiled = aside_host::lower_project(
                modules.clone(),
                &plan,
                &[],
                &[],
                &[],
                &[],
                Default::default(),
            )
            .unwrap();
            let initializer = compiled.initializer;
            let program = if encoded {
                let bytes = bc::encode(compiled.program.package(), bc::Limits::default()).unwrap();
                bc::VerifiedProgram::decode(&bytes, bc::Limits::default()).unwrap()
            } else {
                compiled.program
            };
            let main = bc::FunctionId(
                program
                    .package()
                    .functions
                    .iter()
                    .position(|function| program.package().symbols[function.name.index()] == "main")
                    .unwrap() as u32,
            );
            let mut vm = Vm::load(
                program,
                bc::RuntimeSupport::default(),
                VmLimits::default(),
                SessionEpoch(1),
            )
            .unwrap();
            for function in [initializer, main] {
                let task = vm
                    .start(function, &[], RootOwner(1), bc::Context::Ordinary)
                    .unwrap();
                loop {
                    match vm
                        .run_slice(
                            task,
                            SliceBudget {
                                instructions: 1,
                                ..Default::default()
                            },
                            || 0,
                        )
                        .unwrap()
                    {
                        RunOutcome::BudgetYield(_) => {
                            if vm.heap().phase() == GcPhase::Idle {
                                vm.begin_collection().unwrap();
                            }
                            vm.gc_step(GcBudget {
                                units: 1,
                                ..Default::default()
                            })
                            .unwrap();
                        }
                        RunOutcome::Returned(delivery) => {
                            if function == main {
                                assert_eq!(vm.delivery_value(delivery).unwrap().as_int(), Some(7));
                            }
                            vm.discard_delivery(delivery).unwrap();
                            break;
                        }
                        RunOutcome::Fault(delivery) => {
                            panic!("{:?}", vm.delivery_fault(delivery).unwrap())
                        }
                        other => panic!("{other:?}"),
                    }
                }
            }
        }
    }
}

mod nominal_host {
    use aside_compiler::{
        AnalysisOptions, analyze_module,
        semantic::{MemoryResolver, ModuleInterface, TypeHint},
    };
    use aside_host::{
        CallContext, Error, NativeContract, native,
        source::{Api, Declaration, NamedType, named},
    };
    use aside_vm::{HostTypeId, RootOwner, SessionEpoch, Value, bytecode as bc, executor::*};

    struct Types {
        text: HostTypeId,
        image: HostTypeId,
    }

    #[native(symbol = "sample::make")]
    fn make(
        #[aside(context)] context: &mut CallContext<'_, Types>,
        #[aside(default = true)] text: bool,
    ) -> Result<Value, Error> {
        let heap = context.vm.heap_mut();
        let fields = heap.alloc_table()?;
        let value = heap.alloc_host(fields, &[], None)?;
        heap.set_host_type(
            value,
            if text {
                context.host.text
            } else {
                context.host.image
            },
        )?;
        Ok(value)
    }

    fn api(contract: &NativeContract) -> Api {
        let node = NamedType::new("sample::types::Node", "A host node.", vec![]);
        let text = NamedType::new("sample::types::Text", "A text node.", vec![]).extends(&node);
        let image = NamedType::new("sample::types::Image", "An image node.", vec![]).extends(&node);
        let mut make = Declaration::native(contract, "Construct a real host value.").types(
            TypeHint::Union(vec![named(&text.name), named(&image.name)]),
            &[],
        );
        make.result_cases(&[("text", bc::Const::Bool(true), named(&text.name))]);
        Api {
            types: vec![node, text, image],
            functions: vec![make],
            ..Default::default()
        }
    }

    fn resolver(api: &Api) -> MemoryResolver {
        let mut resolver = MemoryResolver::default();
        resolver.insert(ModuleInterface {
            canonical_path: "sample::types".into(),
            version: "1".into(),
            exports: api.types.iter().map(NamedType::symbol).collect(),
        });
        resolver
    }

    fn analyzed(source: &str, api: &Api) -> aside_compiler::hir::AnalyzedModule {
        let analysis = analyze_module(
            source,
            &AnalysisOptions {
                hosts: api
                    .functions
                    .iter()
                    .map(|declaration| declaration.symbol.clone())
                    .collect(),
                ..Default::default()
            },
            &mut resolver(api),
        )
        .unwrap();
        assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
        analysis.into_module().unwrap()
    }

    #[test]
    fn nominal_host_annotations_execute_in_source_and_asb_without_erasing_native_contracts() {
        let binding = make::binding();
        let contracts = [binding.contract.clone()];
        let api = api(&binding.contract);
        let types = api.registrations();
        let module = analyzed(
            r#"
        use sample::types::{Node, Text, Image};
        fn erase(value) { value }
        fn identity(value: Node) -> Node { value }
        class Local {}
        fn main() {
            let text: Text = make();
            let node: Node = identity(text);
            if !(node is Text) || !(node is Node) || node is Image { throw "host identity"; }
            let image = make(false);
            if !(image is Image) || !(image is Node) || image is Text { throw "other host"; }
            let mut retained: Text = text;
            let mut rejected = 0;
            try { retained = erase(image); } catch error { rejected += 1; }
            try { identity(erase(Local())); } catch error { rejected += 1; }
            if rejected != 2 || retained != text { throw "failed contract publishes a value"; }
            let local: Local = Local();
            if !(local is Local) || local is Node { throw "ordinary class"; }
            true
        }
    "#,
            &api,
        );
        for encoded in [false, true] {
            let compiled = aside_host::lower(
                &module,
                &contracts,
                &api.functions,
                &types,
                &[],
                Default::default(),
            )
            .unwrap();
            let initializer = compiled.entry;
            let program = if encoded {
                let bytes = bc::encode(compiled.program.package(), bc::Limits::default()).unwrap();
                bc::VerifiedProgram::decode(&bytes, bc::Limits::default()).unwrap()
            } else {
                compiled.program
            };
            let main = bc::FunctionId(
                program
                    .package()
                    .functions
                    .iter()
                    .position(|function| program.package().symbols[function.name.index()] == "main")
                    .unwrap() as u32,
            );
            let support = aside_host::runtime_support(&program, &contracts, &types, &[]).unwrap();
            let mut vm = Vm::load(program, support, VmLimits::default(), SessionEpoch(1)).unwrap();
            let mut state = Types {
                text: vm.heap().host_type("sample::types::Text").unwrap(),
                image: vm.heap().host_type("sample::types::Image").unwrap(),
            };
            for function in [initializer, main] {
                let task = vm
                    .start(function, &[], RootOwner(1), bc::Context::Ordinary)
                    .unwrap();
                loop {
                    match vm.run_slice(task, SliceBudget::default(), || 0).unwrap() {
                        RunOutcome::HostCall(ticket) => {
                            (binding.invoke)(&mut state, &mut vm, ticket).unwrap()
                        }
                        RunOutcome::BudgetYield(_) => {}
                        RunOutcome::Returned(delivery) => {
                            if function == main {
                                assert_eq!(
                                    vm.delivery_value(delivery).unwrap().as_bool(),
                                    Some(true)
                                );
                            }
                            vm.discard_delivery(delivery).unwrap();
                            break;
                        }
                        RunOutcome::Fault(delivery) => {
                            panic!("{:?}", vm.delivery_fault(delivery).unwrap())
                        }
                        other => panic!("{other:?}"),
                    }
                }
            }
        }
    }

    #[test]
    fn host_type_declarations_are_rejected_at_their_runtime_value_use() {
        let api = api(&make::binding().contract);
        for expression in ["Node", "Node()", "{let factory = Node; factory()}"] {
            let source = format!("use sample::types::Node; fn main() {{ {expression} }}");
            let analysis =
                analyze_module(&source, &AnalysisOptions::default(), &mut resolver(&api)).unwrap();
            assert!(!analysis.is_ok(), "{source}");
            assert!(
                analysis.diagnostics.iter().any(|diagnostic| diagnostic.code
                    == aside_compiler::DiagnosticCode::InvalidType
                    && &source[aside_compiler::byte_range(diagnostic.range)] == "Node"),
                "{:?}",
                analysis.diagnostics
            );
        }
    }

    #[test]
    fn trusted_source_signature_checks_every_result_case_and_retains_exact_native_abi() {
        let binding = make::binding();
        let api = api(&binding.contract);
        let types = api.registrations();
        let original = &api.functions;
        for mutation in 0..7 {
            let mut changed = api.clone();
            let signature = changed.functions[0].symbol.signature.as_mut().unwrap();
            match mutation {
                0 => signature.result_cases[0].parameter = "different".into(),
                1 => signature.result_cases[0].equals = bc::Const::Bool(false),
                2 => signature.result_cases[0].result = named("sample::types::Image"),
                3 => signature.result_cases[0].when_omitted = false,
                4 => signature.result_cases.clear(),
                5 => signature.result_parameter = Some("text".into()),
                _ => changed.functions[0].symbol.signature = None,
            }
            let module = analyzed("fn main() { make() }", &changed);
            let error = aside_host::lower(
                &module,
                std::slice::from_ref(&binding.contract),
                original,
                &types,
                &[],
                Default::default(),
            )
            .unwrap_err();
            assert!(
                matches!(error, aside_host::BuildError::Lowering(ref error) if error.kind == aside_compiler::lowering::ErrorKind::LinkContract),
                "{error:?}"
            );
        }
        let module = analyzed("fn main() { make() }", &api);
        let mut coarse = api.clone();
        coarse.functions[0]
            .symbol
            .signature
            .as_mut()
            .unwrap()
            .result = TypeHint::Any;
        let coarse = analyzed("fn main() { make() }", &coarse);
        assert!(
            aside_host::lower(
                &coarse,
                std::slice::from_ref(&binding.contract),
                &[],
                &types,
                &[],
                Default::default()
            )
            .is_err(),
            "a coarse ABI cannot authorize unregistered conditional source results"
        );
        assert!(
            aside_host::lower(
                &module,
                std::slice::from_ref(&binding.contract),
                &[],
                &types,
                &[],
                Default::default()
            )
            .is_err(),
            "precise source types require their trusted declaration"
        );
        let mut changed = binding.contract.clone();
        changed.parameters[0].ty = bc::BuiltinType::Int;
        changed.parameters[0].default = Some(bc::Const::Int(1));
        assert!(
            aside_host::lower(
                &module,
                &[changed],
                original,
                &types,
                &[],
                Default::default()
            )
            .is_err(),
            "source precision does not bypass a non-Any runtime ABI"
        );
    }

    #[native(symbol = "sample::reference")]
    fn reference(
        #[aside(context)] context: &mut CallContext<'_, ()>,
        #[aside(default = true)] writable: bool,
    ) -> Result<Value, Error> {
        let heap = context.vm.heap_mut();
        let fields = heap.alloc_table()?;
        let key = heap.alloc_string("value")?;
        heap.table_store(fields, 0, key, Value::int(1))?;
        let host = heap.alloc_host(fields, &[], None)?;
        heap.observe_host_properties(host, 1, 0)?;
        Ok(heap.alloc_property_ref(aside_vm::PropertyId { host, field: 0 }, writable)?)
    }

    #[test]
    fn binding_annotations_use_the_vm_reference_kind_and_retain_permissions() {
        let binding = reference::binding();
        let api = Api {
            functions: vec![
                Declaration::native(&binding.contract, "A property reference.")
                    .types(named("Binding"), &[]),
            ],
            ..Default::default()
        };
        let module = analyzed(
            r#"
        fn erase(value) { value }
        fn keep(value: Binding) -> Binding { value }
        fn main() {
            let writable: Binding = keep(reference());
            let readonly: Binding = keep(reference(false));
            if !(writable is Binding) || !(readonly is Binding) { throw "reference kind"; }
            let mut retained: Binding = writable;
            let mut rejected = false;
            try { retained = erase(true); } catch error { rejected = true; }
            if !rejected || retained != writable { throw "binding contract"; }
            readonly
        }
    "#,
            &api,
        );
        let contracts = [binding.contract.clone()];
        for encoded in [false, true] {
            let compiled = aside_host::lower(
                &module,
                &contracts,
                &api.functions,
                &[],
                &[],
                Default::default(),
            )
            .unwrap();
            let initializer = compiled.entry;
            let program = if encoded {
                let bytes = bc::encode(compiled.program.package(), bc::Limits::default()).unwrap();
                bc::VerifiedProgram::decode(&bytes, bc::Limits::default()).unwrap()
            } else {
                compiled.program
            };
            assert!(
                program
                    .package()
                    .types
                    .contains(&bc::TypeDef::Builtin(bc::BuiltinType::PropertyRef))
            );
            let main = bc::FunctionId(
                program
                    .package()
                    .functions
                    .iter()
                    .position(|function| program.package().symbols[function.name.index()] == "main")
                    .unwrap() as u32,
            );
            let support = aside_host::runtime_support(&program, &contracts, &[], &[]).unwrap();
            let mut vm = Vm::load(program, support, VmLimits::default(), SessionEpoch(1)).unwrap();
            for function in [initializer, main] {
                let task = vm
                    .start(function, &[], RootOwner(1), bc::Context::Ordinary)
                    .unwrap();
                loop {
                    match vm.run_slice(task, SliceBudget::default(), || 0).unwrap() {
                        RunOutcome::HostCall(ticket) => {
                            (binding.invoke)(&mut (), &mut vm, ticket).unwrap();
                        }
                        RunOutcome::BudgetYield(_) => {}
                        RunOutcome::Returned(delivery) => {
                            if function == main {
                                let reference = vm.delivery_value(delivery).unwrap();
                                let (_, writable) = vm.heap().property_ref(reference).unwrap();
                                assert!(
                                    !writable,
                                    "a Binding annotation preserves the readonly reference permission"
                                );
                            }
                            vm.discard_delivery(delivery).unwrap();
                            break;
                        }
                        RunOutcome::Fault(delivery) => {
                            panic!("{:?}", vm.delivery_fault(delivery).unwrap())
                        }
                        other => panic!("{other:?}"),
                    }
                }
            }
        }
    }
}

mod provided_arguments {
    use aside_host::{CallContext, Error, NativeContract, native};
    use aside_vm::{RootOwner, SessionEpoch, Value, bytecode as bc, executor::*};

    struct Host {
        method: bc::FunctionId,
        calls: Vec<Vec<(u32, Option<i64>)>>,
    }

    #[native(symbol = "sample::record")]
    fn record(
        receiver: i64,
        #[aside(default = null)] width: Value,
        #[aside(default = 41)] height: Value,
        #[aside(default = "stable")] label: Value,
        #[aside(context)] context: &mut CallContext<'_, Host>,
    ) -> Result<i64, Error> {
        let provided = context.vm.host_provided_arguments(context.ticket)?;
        assert_eq!(provided.len(), provided.iter().len());
        context.host.calls.push(
            provided
                .iter()
                .map(|(index, value)| (index, value.as_int()))
                .collect(),
        );
        // The split view permits heap access without constructing an argument table.
        context
            .vm
            .with_host_provided_arguments(context.ticket, |heap, provided| {
                assert!(provided.get(provided.len()).is_none());
                let copied = heap.alloc_array(&[]).unwrap();
                for (_, value) in provided.iter() {
                    heap.array_push(copied, value).unwrap();
                }
                assert_eq!(heap.sequence_len(copied).unwrap() as usize, provided.len());
            })?;
        let _ = (width, height);
        let mut label_bytes = [0; 6];
        context.vm.heap().read_bytes(label, 0, &mut label_bytes)?;
        assert_eq!(&label_bytes, b"stable");
        Ok(receiver)
    }

    #[native(symbol = "sample::bind")]
    fn bind(
        receiver: i64,
        #[aside(context)] context: &mut CallContext<'_, Host>,
    ) -> Result<Value, Error> {
        Ok(context
            .vm
            .bind_native(Value::int(receiver), context.host.method)?)
    }

    #[test]
    fn provided_arguments_preserve_presence_order_spreads_and_bound_receivers() {
        let record = record::binding();
        let bind = bind::binding();
        let contracts = [record.contract.clone(), bind.contract.clone()];
        let source = r#"fn main() {
        record(7);
        record(7,width:null);
        record(height:5,receiver:7);
        record(...{height:9,receiver:7});
        let method=bind(7);
        method(); method(width:null); method(...{height:11});
        99
    }"#;
        let module = aside_compiler::analyze_module(
            source,
            &aside_compiler::AnalysisOptions {
                hosts: contracts.iter().map(NativeContract::external).collect(),
                ..Default::default()
            },
            &mut aside_compiler::semantic::NoModules,
        )
        .unwrap()
        .into_module()
        .unwrap();
        let compiled =
            aside_host::lower(&module, &contracts, &[], &[], &[], Default::default()).unwrap();
        let bytes = bc::encode(compiled.program.package(), bc::Limits::default()).unwrap();
        let program = bc::VerifiedProgram::decode(&bytes, bc::Limits::default()).unwrap();
        let function = |name: &str| {
            bc::FunctionId(
                program
                    .package()
                    .functions
                    .iter()
                    .position(|function| program.package().symbols[function.name.index()] == name)
                    .unwrap() as u32,
            )
        };
        let main = function("main");
        let method = function("sample::record");
        let support = aside_host::runtime_support(&program, &contracts, &[], &[]).unwrap();
        for restore in [false, true] {
            let mut vm = Vm::load(
                program.clone(),
                support.clone(),
                VmLimits::default(),
                SessionEpoch(1),
            )
            .unwrap();
            let mut host = Host {
                method,
                calls: Vec::new(),
            };
            let mut epochs = 1;
            for entry in [compiled.entry, main] {
                let mut task = vm
                    .start(entry, &[], RootOwner(1), bc::Context::Ordinary)
                    .unwrap();
                let mut budget = SliceBudget {
                    instructions: 1,
                    work: 1,
                    allocation_bytes: 0,
                    deadline_ns: None,
                };
                loop {
                    match vm.run_slice(task, budget, || 0).unwrap() {
                        RunOutcome::BudgetYield(usage) => {
                            budget.work = usage.required_work.max(1);
                            budget.allocation_bytes = usage.required_allocation;
                        }
                        RunOutcome::HostCall(ticket) => {
                            let native = &vm.program().package().natives[ticket.native.index()];
                            if vm.program().package().symbols[native.symbol.index()]
                                == "sample::record"
                            {
                                (record.invoke)(&mut host, &mut vm, ticket).unwrap();
                            } else {
                                (bind.invoke)(&mut host, &mut vm, ticket).unwrap();
                            }
                        }
                        RunOutcome::Returned(id) => {
                            vm.discard_delivery(id).unwrap();
                            break;
                        }
                        other => panic!("unexpected native call outcome {other:?}"),
                    }
                    if restore {
                        use aside_vm::snapshot::{SnapshotBudget, SnapshotRegistry, VmSnapshot};
                        let registry = SnapshotRegistry::default();
                        let mut capture = vm.snapshot_begin(&registry).unwrap();
                        while !capture.step(SnapshotBudget::default()).unwrap().complete {}
                        let image =
                            VmSnapshot::decode(&capture.finish().unwrap().encode().unwrap())
                                .unwrap();
                        epochs += 1;
                        let mut loading = Vm::restore_begin(
                            &image,
                            program.clone(),
                            support.clone(),
                            VmLimits::default(),
                            SessionEpoch(epochs),
                            &registry,
                        )
                        .unwrap();
                        while !loading.step(SnapshotBudget::default()).unwrap().complete {}
                        let (loaded, map) = loading.finish().unwrap();
                        vm = loaded;
                        task = map.task(task).unwrap();
                    }
                }
            }
            assert_eq!(
                host.calls,
                vec![
                    vec![(0, Some(7))],
                    vec![(0, Some(7)), (1, None)],
                    vec![(2, Some(5)), (0, Some(7))],
                    vec![(2, Some(9)), (0, Some(7))],
                    vec![],
                    vec![(1, None)],
                    vec![(2, Some(11))],
                ]
            );
        }
    }

    #[test]
    fn default_slots_remain_measured_separately_from_borrowed_provided_arguments() {
        let mut costs = Vec::new();
        for parameters in [3, 64] {
            let contract = NativeContract {
                symbol: "sample::record".into(),
                abi_version: 1,
                parameters: (0..parameters)
                    .map(|index| aside_host::NativeParameter {
                        name: format!("p{index}"),
                        ty: bc::BuiltinType::Any,
                        default: Some(bc::Const::Null),
                    })
                    .collect(),
                rest: None,
                result: bc::BuiltinType::Any,
                effects: bc::Effects(bc::Effects::HOST),
                required_capabilities: vec![],
            };
            let source = "fn main(){record(p1:7)}";
            let module = aside_compiler::analyze_module(
                source,
                &aside_compiler::AnalysisOptions {
                    hosts: vec![contract.external()],
                    ..Default::default()
                },
                &mut aside_compiler::semantic::NoModules,
            )
            .unwrap()
            .into_module()
            .unwrap();
            let compiled = aside_host::lower(
                &module,
                std::slice::from_ref(&contract),
                &[],
                &[],
                &[],
                Default::default(),
            )
            .unwrap();
            let program = compiled.program;
            let main = bc::FunctionId(
                program
                    .package()
                    .functions
                    .iter()
                    .position(|function| program.package().symbols[function.name.index()] == "main")
                    .unwrap() as u32,
            );
            let support = aside_host::runtime_support(&program, &[contract], &[], &[]).unwrap();
            let mut vm = Vm::load(program, support, VmLimits::default(), SessionEpoch(1)).unwrap();
            for entry in [compiled.entry, main] {
                let task = vm
                    .start(entry, &[], RootOwner(1), bc::Context::Ordinary)
                    .unwrap();
                let before = vm.heap().stats();
                let mut work = 0;
                loop {
                    let outcome = vm.run_slice(task, SliceBudget::default(), || 0).unwrap();
                    work += vm.stats().usage.work;
                    match outcome {
                        RunOutcome::BudgetYield(_) => {}
                        RunOutcome::HostCall(ticket) => {
                            let provided = vm.host_provided_arguments(ticket).unwrap();
                            assert_eq!(provided.iter().collect::<Vec<_>>().len(), 1);
                            assert_eq!(provided.get(0).unwrap().0, 1);
                            assert_eq!(provided.get(0).unwrap().1.as_int(), Some(7));
                            let after = vm.heap().stats();
                            costs.push(work);
                            eprintln!(
                                "native binder: parameters={parameters} provided=1 work={work} charged_growth={} chunks_growth={}",
                                after.charged_bytes - before.charged_bytes,
                                after.chunk_allocations - before.chunk_allocations
                            );
                            vm.respond(ticket, HostResponse::Ready(Value::NULL))
                                .unwrap();
                        }
                        RunOutcome::Returned(id) => {
                            vm.discard_delivery(id).unwrap();
                            break;
                        }
                        other => panic!("unexpected binder outcome {other:?}"),
                    }
                }
            }
        }
        assert!(
            costs[1] < costs[0] * 8,
            "constant default work must grow with slots, not with every earlier prefix: {costs:?}"
        );
    }
}

mod shared_defaults {
    use aside_host::{NativeContract, NativeParameter, native};
    use aside_vm::{RootOwner, SessionEpoch, bytecode as bc, executor::*};

    fn parameter(name: &str, ty: bc::BuiltinType, default: Option<bc::Const>) -> NativeParameter {
        NativeParameter {
            name: name.into(),
            ty,
            default,
        }
    }
    fn contract(name: &str, parameters: Vec<NativeParameter>) -> NativeContract {
        NativeContract {
            symbol: format!("sample::{name}"),
            abi_version: 1,
            parameters,
            rest: None,
            result: bc::BuiltinType::Any,
            effects: bc::Effects(0),
            required_capabilities: vec![],
        }
    }
    fn default(program: &bc::VerifiedProgram, native: usize, index: usize) -> bc::ParameterDefault {
        let package = program.package();
        package.signatures[package.natives[native].signature.index()].parameters[index]
            .default
            .unwrap()
    }

    #[test]
    fn literal_sharing_uses_pool_values_independently_of_prefix_and_preserves_native_types() {
        use bc::{BuiltinType as T, Const as C};
        let base = vec![
            parameter("x", T::Any, None),
            parameter("value", T::Any, Some(C::Null)),
        ];
        let mut contracts = vec![
            contract("first", base.clone()),
            contract("second", base.clone()),
        ];
        let mut changed = base.clone();
        changed[0].name = "other".into();
        contracts.push(contract("name", changed));
        let mut changed = base.clone();
        changed[0].ty = T::Int;
        contracts.push(contract("prefix_type", changed));
        let mut changed = base.clone();
        changed[1].ty = T::Null;
        contracts.push(contract("result_type", changed));
        let mut changed = base.clone();
        changed[1].default = Some(C::Int(7));
        contracts.push(contract("value", changed));
        let mut changed = base;
        changed[0].default = Some(C::Int(3));
        contracts.push(contract("earlier_default", changed));
        let seed = aside_host::seed(&contracts, &[], &[], 1).unwrap();
        let shared = default(&seed, 0, 1);
        assert_eq!(shared, default(&seed, 1, 1));
        assert_eq!(
            shared,
            default(&seed, 6, 1),
            "earlier defaults do not change an immutable literal"
        );
        for native in 2..5 {
            assert_eq!(shared, default(&seed, native, 1));
        }
        assert_ne!(shared, default(&seed, 5, 1));
        assert!(matches!(shared, bc::ParameterDefault::Constant(_)));
        assert_eq!(seed.package().functions.len(), contracts.len());
        assert_eq!(seed.package().constants, [C::Null, C::Int(7), C::Int(3)]);
        let support = aside_host::runtime_support(&seed, &contracts, &[], &[]).unwrap();
        Vm::load(seed.clone(), support, Default::default(), SessionEpoch(1)).unwrap();
        let mut forged = seed.to_unpacked().unwrap();
        let other = default(&seed, 5, 1);
        let signature = forged.natives[0].signature.index();
        forged.signatures[signature].parameters[1].default = Some(other);
        let forged = bc::verify(forged, Default::default()).unwrap();
        assert!(
            aside_host::runtime_support(&forged, &contracts, &[], &[]).is_err(),
            "a different registered constant cannot be substituted"
        );
        let mut forged = seed.to_unpacked().unwrap();
        let signature = forged.natives[0].signature.index();
        let prefix = forged.signatures[signature].parameters[..1].to_vec();
        let helper_signature = bc::SignatureId(forged.signatures.len() as u32);
        forged.signatures.push(bc::Signature {
            parameters: prefix,
            rest: None,
            return_type: forged.signatures[signature].return_type,
            effects: bc::Effects(bc::Effects::ALLOCATE),
            property_bag_tail: None,
        });
        let bc::ParameterDefault::Constant(id) = shared else {
            unreachable!()
        };
        let function = bc::FunctionId(forged.functions.len() as u32);
        forged.functions.push(bc::FunctionDef {
            module: bc::ModuleId(0),
            name: forged.natives[0].symbol,
            kind: bc::FunctionKind::Plain,
            signature: helper_signature,
            context: bc::Context::NoWait,
            captures: vec![],
            register_count: 2,
            bindings: vec![],
            descriptors: vec![],
            regions: vec![],
            source_map: vec![],
            code: vec![
                bc::Instruction::new(bc::Opcode::LoadConst, 1, id.0, 0),
                bc::Instruction::new(bc::Opcode::Return, 1, 0, 0),
            ],
        });
        forged.signatures[signature].parameters[1].default =
            Some(bc::ParameterDefault::Function(function));
        let forged = bc::verify(forged, Default::default()).unwrap();
        assert!(
            aside_host::runtime_support(&forged, &contracts, &[], &[]).is_err(),
            "an executable default cannot replace a registered literal"
        );
    }

    #[native(symbol = "sample::first")]
    fn first(receiver: i64, #[aside(default = "shared")] label: String) -> String {
        format!("{receiver}:{label}")
    }
    #[native(symbol = "sample::second")]
    fn second(receiver: i64, #[aside(default = "shared")] label: String) -> String {
        format!("{receiver}:{label}")
    }

    #[test]
    fn shared_literal_calls_and_dependent_script_defaults_survive_asb_gc_and_every_slice_restore() {
        let first = first::binding();
        let second = second::binding();
        let contracts = [first.contract.clone(), second.contract.clone()];
        let code = r#"fn dependent(a=4,b=a+1){a+b}
        fn mutate(mut a=4,b=if true {a+=2;a} else {0}){a+b}
        fn fresh(a=[],b={value:0}){a.append(1);b.value+=1;a.len()+b.value}
        fn main(){
            if first(7)!="7:shared" || second(receiver:8)!="8:shared" {throw "shared default";}
            let alias=first;
            if alias(9)!="9:shared" || second(10,"explicit")!="10:explicit" {throw "alias/default binding";}
            if dependent()!=9 || dependent(10)!=21 {throw "dependent script default";}
            if mutate()!=12 || fresh()!=2 || fresh()!=2 {throw "default mutation/freshness";}
            42
        }"#;
        let module = aside_compiler::analyze_module(
            code,
            &aside_compiler::AnalysisOptions {
                hosts: contracts.iter().map(NativeContract::external).collect(),
                ..Default::default()
            },
            &mut aside_compiler::semantic::NoModules,
        )
        .unwrap()
        .into_module()
        .unwrap();
        let compilation =
            aside_host::lower(&module, &contracts, &[], &[], &[], Default::default()).unwrap();
        let bytes = bc::encode(compilation.program.package(), Default::default()).unwrap();
        let program = bc::VerifiedProgram::decode(&bytes, Default::default()).unwrap();
        assert_eq!(default(&program, 0, 1), default(&program, 1, 1));
        let main = bc::FunctionId(
            program
                .package()
                .functions
                .iter()
                .position(|function| program.package().symbols[function.name.index()] == "main")
                .unwrap() as u32,
        );
        let support = aside_host::runtime_support(&program, &contracts, &[], &[]).unwrap();
        for restore in [false, true] {
            let mut vm = Vm::load(
                program.clone(),
                support.clone(),
                Default::default(),
                SessionEpoch(1),
            )
            .unwrap();
            let mut epoch = 1;
            for entry in [compilation.entry, main] {
                let mut task = vm
                    .start(entry, &[], RootOwner(1), bc::Context::Ordinary)
                    .unwrap();
                let mut budget = SliceBudget {
                    instructions: 1,
                    work: 1,
                    allocation_bytes: 0,
                    deadline_ns: None,
                };
                loop {
                    match vm.run_slice(task, budget, || 0).unwrap() {
                        RunOutcome::BudgetYield(usage) => {
                            budget.work = usage.required_work.max(1);
                            budget.allocation_bytes = usage.required_allocation;
                        }
                        RunOutcome::HostCall(ticket) => match ticket.native.0 {
                            0 => (first.invoke)(&mut (), &mut vm, ticket).unwrap(),
                            1 => (second.invoke)(&mut (), &mut vm, ticket).unwrap(),
                            _ => unreachable!(),
                        },
                        RunOutcome::Returned(delivery) => {
                            if entry == main {
                                assert_eq!(vm.delivery_value(delivery).unwrap().as_int(), Some(42));
                            }
                            vm.discard_delivery(delivery).unwrap();
                            break;
                        }
                        other => panic!("shared defaults failed: {other:?}"),
                    }
                    vm.begin_collection().unwrap();
                    while vm.heap().phase() != aside_vm::GcPhase::Idle {
                        vm.gc_step(Default::default()).unwrap();
                    }
                    if restore {
                        use aside_vm::snapshot::{SnapshotBudget, SnapshotRegistry, VmSnapshot};
                        let registry = SnapshotRegistry::default();
                        let mut capture = vm.snapshot_begin(&registry).unwrap();
                        while !capture.step(SnapshotBudget::default()).unwrap().complete {}
                        let image =
                            VmSnapshot::decode(&capture.finish().unwrap().encode().unwrap())
                                .unwrap();
                        epoch += 1;
                        let mut loading = Vm::restore_begin(
                            &image,
                            program.clone(),
                            support.clone(),
                            Default::default(),
                            SessionEpoch(epoch),
                            &registry,
                        )
                        .unwrap();
                        while !loading.step(SnapshotBudget::default()).unwrap().complete {}
                        let (restored, map) = loading.finish().unwrap();
                        vm = restored;
                        task = map.task(task).unwrap();
                    }
                }
            }
        }
    }

    #[test]
    fn wide_registered_defaults_retain_linear_metadata_and_use_no_default_frames() {
        let mut last_owned = 0;
        for width in [32, 64, 128] {
            let contract = contract(
                "wide",
                (0..width)
                    .map(|i| {
                        parameter(
                            &format!("p{i}"),
                            bc::BuiltinType::Any,
                            Some(bc::Const::Null),
                        )
                    })
                    .collect(),
            );
            let program = aside_host::seed(std::slice::from_ref(&contract), &[], &[], 1).unwrap();
            assert_eq!(program.package().functions.len(), 1);
            assert_eq!(program.package().signatures.len(), 1);
            assert_eq!(program.package().constants, [bc::Const::Null]);
            let owned = program.stats().owned_capacity_bytes;
            if last_owned != 0 {
                assert!(
                    owned < last_owned * 2,
                    "literal metadata must scale linearly"
                );
            }
            last_owned = owned;
            let support =
                aside_host::runtime_support(&program, std::slice::from_ref(&contract), &[], &[])
                    .unwrap();
            let mut vm = Vm::load(program, support, Default::default(), SessionEpoch(1)).unwrap();
            let task = vm
                .start(bc::FunctionId(0), &[], RootOwner(1), bc::Context::Ordinary)
                .unwrap();
            let roots = vm.stats().slots.active_ranges;
            let mut budget = SliceBudget {
                instructions: 1,
                work: 1,
                allocation_bytes: 0,
                ..Default::default()
            };
            loop {
                assert_eq!(
                    vm.stats().frames,
                    1,
                    "literal defaults do not execute helper frames"
                );
                assert_eq!(
                    vm.stats().slots.active_ranges,
                    roots,
                    "literal defaults do not add temporary root windows"
                );
                match vm.run_slice(task, budget, || 0).unwrap() {
                    RunOutcome::BudgetYield(usage) => {
                        budget.work = usage.required_work.max(1);
                        budget.allocation_bytes = usage.required_allocation;
                    }
                    RunOutcome::HostCall(ticket) => {
                        assert_eq!(vm.host_arguments(ticket).unwrap().len(), width);
                        assert!(
                            vm.host_arguments(ticket)
                                .unwrap()
                                .iter()
                                .all(|value| value.same_bits(aside_vm::Value::NULL))
                        );
                        assert_eq!(vm.host_provided_arguments(ticket).unwrap().len(), 0);
                        vm.respond(ticket, HostResponse::Ready(aside_vm::Value::NULL))
                            .unwrap();
                        break;
                    }
                    result => panic!("{result:?}"),
                }
            }
            eprintln!("wide_constant_defaults,width={width},owned={owned},roots={roots}");
        }
    }
}

mod shared_methods {
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
            NamedType::new("sample::types::Inherited", "An inherited receiver.", vec![])
                .extends(&base);
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
            aside_host::runtime_support(&compiled.program, &contracts, &registrations, &[])
                .unwrap();
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
                    let result =
                        (fixture.bindings[ticket.native.index()].invoke)(&mut (), vm, ticket);
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
                let image =
                    VmSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
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
                    if usage.instructions == 0 && usage.work > 0 && usage.required_work > usage.work
                    {
                        let RunOutcome::BudgetYield(retry) =
                            vm.run_slice(task, budget, || 0).unwrap()
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
                    (fixture.bindings[ticket.native.index()].invoke)(&mut (), &mut vm, ticket)
                        .unwrap();
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
            println!(
                "10000 direct host method calls: round={round}, chunks=0, live_after_gc={live}"
            );
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
        let minimal = bc::verify(
            bc::BytecodePackage::<Vec<bc::Instruction>>::default(),
            bc::Limits::default(),
        )
        .unwrap();
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
}

mod source_optional {
    use aside_vm::bytecode as bc;

    #[test]
    fn supplied_only_declarations_preserve_optional_presence_and_explicit_value_types() {
        use aside_compiler::semantic::{NoModules, TypeHint};
        use aside_host::source::{Api, Declaration, ParameterDefault};
        let contract = aside_host::NativeContract {
            symbol: "sample::record".into(),
            abi_version: 1,
            parameters: [
                ("receiver", bc::BuiltinType::Int, None),
                ("width", bc::BuiltinType::Any, Some(bc::Const::Null)),
                ("height", bc::BuiltinType::Any, Some(bc::Const::Int(41))),
                (
                    "label",
                    bc::BuiltinType::Any,
                    Some(bc::Const::Utf8("stable".into())),
                ),
            ]
            .into_iter()
            .map(|(name, ty, default)| aside_host::NativeParameter {
                name: name.into(),
                ty,
                default,
            })
            .collect(),
            rest: None,
            result: bc::BuiltinType::Int,
            effects: bc::Effects(bc::Effects::HOST),
            required_capabilities: Vec::new(),
        };
        let mut declaration = Declaration::native(&contract, "Record supplied properties.").types(
            TypeHint::Int,
            &[
                ("width", TypeHint::Int),
                ("height", TypeHint::Int),
                ("label", TypeHint::String),
            ],
        );
        declaration.supplied_only(&["width"]);
        assert_eq!(contract.parameters[1].default, Some(bc::Const::Null));
        assert!(matches!(
            declaration.parameter_defaults.get("width"),
            Some(ParameterDefault::Omitted)
        ));
        assert_eq!(
            declaration.parameter_defaults["height"].value(),
            Some(&bc::Const::Int(41))
        );
        let api = Api {
            functions: vec![declaration.clone()],
            ..Default::default()
        };
        let documents = api.documents("sample");
        assert!(documents.documents.iter().any(|document| {
            document
                .text
                .contains("width?: int, height: int = 41, label: string = \"stable\"")
        }));
        let options = aside_compiler::AnalysisOptions {
            hosts: vec![declaration.symbol],
            ..Default::default()
        };
        let valid = aside_compiler::analyze_module(
            "fn main(){record(7);record(7,width:3);}",
            &options,
            &mut NoModules,
        )
        .unwrap();
        assert!(valid.is_ok(), "{:?}", valid.diagnostics);
        let source = "fn main(){record(7,width:null);}";
        let invalid = aside_compiler::analyze_module(source, &options, &mut NoModules).unwrap();
        assert!(
            invalid.diagnostics.iter().any(|diagnostic| diagnostic.code
                == aside_compiler::DiagnosticCode::TypeMismatch
                && &source
                    [usize::from(diagnostic.range.start())..usize::from(diagnostic.range.end())]
                    == "width:null"),
            "{:?}",
            invalid.diagnostics
        );
    }
}
