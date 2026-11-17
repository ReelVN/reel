use aside_host::{CallContext, Error, NativeContract, NativeParameter, native};
use aside_vm::{GcBudget, GcPhase, RootOwner, SessionEpoch, Value, bytecode as bc, executor::*};

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
            standard_derived_commit: false,
            read_only_leaf: None,
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
                        let value = vm.host_resume_value(ticket).unwrap();
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
                        source[span.start as usize..span.end as usize].contains("method(receiver:")
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
