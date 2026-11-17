use aside_compiler::{
    AnalysisOptions, analyze_module,
    semantic::{ExternalResultParameter, MemoryResolver, ModuleInterface, TypeHint},
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
                            assert_eq!(vm.delivery_value(delivery).unwrap().as_bool(), Some(true));
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
            5 => signature.result_parameter = Some(ExternalResultParameter::Value("text".into())),
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
