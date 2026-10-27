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

mod bound_native;

mod editor_types;

mod nominal_host;

mod provided_arguments;

mod shared_defaults;

mod shared_methods;

mod source_optional;
