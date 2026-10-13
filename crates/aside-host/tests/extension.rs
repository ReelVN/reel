use aside_host::{CallContext, Error, NativeContract, Properties, PropertySchema, native};
use aside_vm::{Heap, HeapError, RootOwner, SessionEpoch, Value, bytecode as bc, executor::*};

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
    Ok(heap.alloc_host(fields, &[], Some(Field::validate))?)
}

#[test]
fn standalone_macro_host_compiles_encodes_admits_and_calls_rust() {
    let add = add::binding();
    let decorate = decorate::binding();
    let gauge = gauge::binding();
    let contracts = [
        add.contract.clone(),
        decorate.contract.clone(),
        gauge.contract.clone(),
    ];
    let capabilities = ["sample.text".to_string()];
    let module = aside_compiler::analyze_module(r#"fn main() {
        let f = add;
        if f(right: 8, left: 4) != 12 || add(3) != 5 { throw "binding"; }
        if decorate("你好") != "你好!" || decorate(suffix: "?", value: "为什么") != "为什么?" { throw "text"; }
        let node = gauge();
        node.x = 2.5;
        node.visible = false;
        let mut rejected = 0;
        try { node.x = -1.0; } catch error { rejected += 1; }
        try { node.id = "new"; } catch error { rejected += 1; }
        try { decorate("reject"); } catch error { rejected += 1; }
        if rejected != 3 || node.x != 2.5 || node.visible || node.id != "stable" { throw "property schema"; }
        true
    }"#, &aside_compiler::AnalysisOptions {
        hosts: contracts.iter().map(NativeContract::external).collect(), ..Default::default()
    }, &mut aside_compiler::semantic::NoModules).unwrap().into_module().unwrap();
    let compiled =
        aside_host::lower(&module, &contracts, &capabilities, Default::default()).unwrap();
    let encoded = bc::encode(compiled.program.package(), bc::Limits::default()).unwrap();
    let program = bc::VerifiedProgram::decode(&encoded, bc::Limits::default()).unwrap();
    let support =
        aside_host::runtime_support(program.package(), &contracts, &capabilities).unwrap();
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
                    _ => unreachable!(),
                },
                RunOutcome::BudgetYield(_) => {}
                RunOutcome::Returned(delivery) => {
                    vm.discard_delivery(delivery).unwrap();
                    break;
                }
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
    assert!(aside_host::runtime_support(vm.program().package(), &changed, &capabilities).is_err());
}
