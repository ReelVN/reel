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
                        if vm.program().package().symbols[native.symbol.index()] == "sample::record"
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
                        VmSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
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
