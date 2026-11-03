//! Dynamic names reuse class member contracts through source, ASB and the VM.
use super::*;

#[test]
fn dynamic_members_share_fields_inheritance_methods_and_evaluation_order() {
    run(r#"
        class Base {
            let mut value:int = 40;
            let fixed:int;
            static let mut total:int = 0;
            fn init(self) { self["fixed"] = 2; }
            fn read(self, extra=0) { self["value"] + extra }
            static fn count() { Base["total"] += 1; Base.total }
        }
        class Child: Base {
            fn read(self, extra=0) { super["read"](extra:extra) }
        }
        fn main() {
            let item=Child(); let alias=item;
            let key="va"+"lue";
            let mut order="";
            let object=||{order+="o";item};
            let name=||{order+="k";key};
            let value=||{order+="v";2};
            object()[name()] += value();
            if order!="okv" || alias.value!=42 {throw "location evaluation";}
            object()[name()] = value();
            if order!="okvokv" || alias.value!=2 {throw "assignment evaluation";}
            item[key]=40;
            let method=item["read"];
            item[key]+=1;
            if method(extra:1)!=42 || item["fixed"]!=2 {throw "bound receiver";}
            let static_method=Base["count"];
            if static_method()!=1 || Child["total"]!=1 {throw "static member";}
            let table={[(1,2)]:3, [1]:4};
            table[1]+=1;
            if table[(1,2)]!=3 || table[1]!=5 {throw "table key semantics";}
            let absent=null;
            if absent?[name()]!=null || order!="okvokv" {throw "optional short circuit";}
            42
        }
    "#);
}

#[test]
fn dynamic_names_preserve_fault_kinds_and_source_locations() {
    for (operation, expected) in [
        ("item[\"missing\"]", FaultKind::MissingKey),
        ("item[\"known_but_absent\"]", FaultKind::MissingKey),
        ("item[\"missing\"]=1", FaultKind::MissingKey),
        ("item[\"fixed\"]=1", FaultKind::ReadOnly),
        ("item[\"read\"]=1", FaultKind::ReadOnly),
        ("item[\"value\"]=\"wrong\"", FaultKind::Type),
        ("item[key]", FaultKind::Type),
        ("item[key]=1", FaultKind::Type),
    ] {
        let source = format!(
            r#"
            class Item {{ let mut value:int=40; let fixed=2; fn read(self){{self.value}} }}
            fn access(item,key) {{ {operation}; }}
            fn main() {{ let known_but_absent=0; access(Item(),0); }}
        "#
        );
        let module = analyze_module(&source, &AnalysisOptions::default(), &mut NoModules)
            .unwrap()
            .into_module()
            .unwrap();
        let compiled = lowering::compile(
            &module,
            &lowering::Linkage::default(),
            lowering::Options::default(),
        )
        .unwrap();
        let p = compiled.program.package();
        let main = bc::FunctionId(
            p.functions
                .iter()
                .position(|f| p.symbols[f.name.index()] == "main")
                .unwrap() as u32,
        );
        let bytes = bc::encode(p, bc::Limits::default()).unwrap();
        for tiny in [false, true] {
            let mut vm = Vm::load(
                bc::VerifiedProgram::decode(&bytes, bc::Limits::default()).unwrap(),
                bc::RuntimeSupport::default(),
                VmLimits::default(),
                SessionEpoch(1),
            )
            .unwrap();
            let init = vm
                .start(compiled.entry, &[], RootOwner(1), bc::Context::Init)
                .unwrap();
            finish(&mut vm, init, tiny);
            let task = vm
                .start(main, &[], RootOwner(1), bc::Context::Ordinary)
                .unwrap();
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
                let result = vm.run_slice(task, budget, || 0).unwrap();
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
                match result {
                    RunOutcome::BudgetYield(usage) => {
                        budget.work = usage.required_work.max(1);
                        budget.allocation_bytes = usage.required_allocation;
                    }
                    RunOutcome::Fault(delivery) => {
                        let fault = vm.delivery_fault(delivery).unwrap().unwrap();
                        assert_eq!(fault.kind, expected, "tiny={tiny}, {operation}");
                        let span = fault.location.source.unwrap();
                        let text = &vm.program().package().sources[span.file.index()].text;
                        assert_eq!(
                            &text[span.start as usize..span.end as usize],
                            operation.split('=').next().unwrap(),
                            "tiny={tiny}"
                        );
                        vm.discard_delivery(delivery).unwrap();
                        break;
                    }
                    other => panic!("unexpected dynamic member outcome: {other:?}"),
                }
            }
        }
    }
}

#[test]
fn class_indices_require_string_names_when_the_class_is_known() {
    for source in [
        "class Item {let value=1;} fn main(){Item()[0]}",
        "class Item {static let value=1;} fn main(){Item[false]}",
    ] {
        let analysis = analyze_module(source, &AnalysisOptions::default(), &mut NoModules).unwrap();
        assert!(
            analysis
                .diagnostics
                .iter()
                .any(|d| d.code == aside_compiler::DiagnosticCode::TypeMismatch),
            "{source}: {:?}",
            analysis.diagnostics
        );
        assert!(analysis.module().is_none());
    }
}

#[test]
fn dynamic_field_updates_reuse_storage_and_charge_lookup_work() {
    let source = "class Counter{let mut value:int=0;}let counter=Counter();let key=\"value\";fn main(){for i in 0..10000{counter[key]+=1;}counter[key]}fn direct(){for i in 0..10000{counter.value+=1;}counter.value}";
    let module = analyze_module(source, &AnalysisOptions::default(), &mut NoModules)
        .unwrap()
        .into_module()
        .unwrap();
    let compiled = lowering::compile(
        &module,
        &lowering::Linkage::default(),
        lowering::Options::default(),
    )
    .unwrap();
    let p = compiled.program.package();
    let main = bc::FunctionId(
        p.functions
            .iter()
            .position(|f| p.symbols[f.name.index()] == "main")
            .unwrap() as u32,
    );
    let direct = bc::FunctionId(
        p.functions
            .iter()
            .position(|f| p.symbols[f.name.index()] == "direct")
            .unwrap() as u32,
    );
    let lookup_work = u64::from(p.symbols.len().ilog2() + 1) * 6;
    let mut vm = Vm::load(
        compiled.program,
        bc::RuntimeSupport::default(),
        VmLimits::default(),
        SessionEpoch(1),
    )
    .unwrap();
    let init = vm
        .start(compiled.entry, &[], RootOwner(1), bc::Context::Init)
        .unwrap();
    finish(&mut vm, init, false);
    let chunks = vm.heap().stats().chunk_allocations;
    let task = vm
        .start(main, &[], RootOwner(1), bc::Context::Ordinary)
        .unwrap();
    let started = std::time::Instant::now();
    let (answer, indexed_work) = measured(&mut vm, task);
    assert_eq!(answer.as_int(), Some(10000));
    let indexed_elapsed = started.elapsed();
    let task = vm
        .start(direct, &[], RootOwner(1), bc::Context::Ordinary)
        .unwrap();
    let (answer, direct_work) = measured(&mut vm, task);
    assert_eq!(answer.as_int(), Some(20000));
    assert!(
        indexed_work >= direct_work + 20001 * lookup_work,
        "each dynamic read/write charges its symbol lookup: indexed={indexed_work}, direct={direct_work}, lookup={lookup_work}"
    );
    assert_eq!(
        vm.heap().stats().chunk_allocations,
        chunks,
        "dynamic names and typed field updates allocate no payload blocks"
    );
    println!(
        "10000 dynamic typed field increments: chunks=0, work={indexed_work}, direct_work={direct_work}, elapsed={indexed_elapsed:?}"
    );

    fn measured(vm: &mut Vm, task: TaskId) -> (Value, u64) {
        let mut budget = SliceBudget::default();
        let mut work = 0;
        loop {
            let outcome = vm.run_slice(task, budget, || 0).unwrap();
            work += vm.stats().usage.work;
            match outcome {
                RunOutcome::BudgetYield(usage) => {
                    budget.work = budget.work.max(usage.required_work);
                    budget.allocation_bytes =
                        budget.allocation_bytes.max(usage.required_allocation);
                }
                RunOutcome::Returned(delivery) => {
                    let value = vm.delivery_value(delivery).unwrap();
                    vm.discard_delivery(delivery).unwrap();
                    return (value, work);
                }
                other => panic!("unexpected dynamic field measurement: {other:?}"),
            }
        }
    }
}

#[test]
fn model_indices_share_bind_locations_and_replace_derived_dependencies() {
    use aside_compiler::semantic::{ExternalSymbol, Intrinsic, SymbolReference};
    let source = r#"
        class State {
            let mut key:string="left";
            let mut left:int=40;
            let mut right:int=7;
            let mut projected=0;
        }
        fn expect(state,value) {
            let output=derive(||state["projected"]+1);
            if output.value!=value {throw "nested indexed refresh";}
        }
        fn main() {
            let state=model(State());
            let fixed=bind(state[state.key]);
            let selected=derive(||state[state.key]);
            state.projected=selected;
            expect(state,41);
            if fixed.value!=40 || selected.value!=40 {throw "initial reads";}
            state["left"]+=1;
            expect(state,42);
            if fixed.value!=41 || selected.value!=41 {throw "typed indexed source";}
            state["key"]="right";
            expect(state,8);
            if selected.value!=7 || fixed.value!=41 {throw "dynamic dependency";}
            state["left"]=0;
            expect(state,8);
            if selected.value!=7 {throw "detached dependency";}
            state["right"]=41;
            expect(state,42);
            let mut caught=false;
            try {state["right"]="wrong";} catch error {caught=true;}
            if !caught || state.right!=41 {throw "model field type";}
            42
        }
    "#;
    let references = ["model", "derive"].map(|name| SymbolReference {
        module: "test::reactive".into(),
        version: "1".into(),
        export: Some(name.into()),
    });
    let options = AnalysisOptions {
        hosts: vec![
            ExternalSymbol::dynamic("model").with_reference(references[0].clone()),
            ExternalSymbol::intrinsic("derive", Intrinsic::Derive)
                .with_reference(references[1].clone()),
            ExternalSymbol::intrinsic("bind", Intrinsic::Bind),
        ],
        ..Default::default()
    };
    let module = analyze_module(source, &options, &mut NoModules)
        .unwrap()
        .into_module()
        .unwrap();
    let signature = bc::Signature {
        parameters: vec![bc::Parameter {
            name: bc::SymbolId(2),
            ty: bc::TypeId(0),
            mutable: false,
            default: None,
        }],
        rest: None,
        return_type: bc::TypeId(0),
        effects: bc::Effects(bc::Effects::ALLOCATE),
        property_bag_tail: None,
    };
    let seed = bc::verify(
        bc::BytecodePackage {
            symbols: vec![
                "model".into(),
                "derive".into(),
                "value".into(),
                "test::reactive".into(),
            ],
            types: vec![bc::TypeDef::Builtin(bc::BuiltinType::Any)],
            signatures: vec![signature.clone()],
            natives: (0..2)
                .map(|index| bc::NativeDef {
                    symbol: bc::SymbolId(index),
                    signature: bc::SignatureId(0),
                    abi_version: 1,
                    capabilities: vec![],
                })
                .collect(),
            modules: vec![bc::ModuleDef {
                path: bc::SymbolId(3),
                imports: vec![],
                exports: vec![],
                globals: vec![],
                initializer: None,
            }],
            functions: (0..2)
                .map(|index| bc::FunctionDef {
                    module: bc::ModuleId(0),
                    name: bc::SymbolId(index),
                    kind: bc::FunctionKind::Native(bc::NativeId(index)),
                    signature: bc::SignatureId(0),
                    context: bc::Context::Ordinary,
                    captures: vec![],
                    register_count: 0,
                    bindings: vec![],
                    descriptors: vec![],
                    code: vec![],
                    regions: vec![],
                    source_map: vec![],
                })
                .collect(),
            ..Default::default()
        },
        bc::Limits::default(),
    )
    .unwrap();
    let links: Vec<_> = module
        .external_bindings()
        .iter()
        .filter_map(|binding| {
            let symbol = binding.symbol?;
            let index = references
                .iter()
                .position(|reference| binding.descriptor.reference.as_deref() == Some(reference))?;
            Some(lowering::NativeLink {
                target: lowering::ExternalTarget::Symbol(symbol),
                native: bc::NativeId(index as u32),
                reference: &references[index],
                source_signature: None,
            })
        })
        .collect();
    let compiled = lowering::compile(
        &module,
        &lowering::Linkage {
            package: Some(&seed),
            natives: &links,
            expansions: &[],
        },
        lowering::Options::default(),
    )
    .unwrap();
    let p = compiled.program.package();
    let main = bc::FunctionId(
        p.functions
            .iter()
            .position(|f| p.symbols[f.name.index()] == "main")
            .unwrap() as u32,
    );
    let bytes = bc::encode(p, bc::Limits::default()).unwrap();
    let support = bc::RuntimeSupport {
        natives: ["model", "derive"]
            .map(|name| bc::NativeSupport {
                standard_derived_commit: false,
                read_only_leaf: None,
                symbol: name.into(),
                abi_version: 1,
                signature: signature.clone(),
                required_capabilities: vec![],
            })
            .to_vec(),
        ..Default::default()
    };
    for tiny in [false, true] {
        let mut vm = Vm::load(
            bc::VerifiedProgram::decode(&bytes, bc::Limits::default()).unwrap(),
            support.clone(),
            VmLimits::default(),
            SessionEpoch(1),
        )
        .unwrap();
        let init = vm
            .start(compiled.entry, &[], RootOwner(1), bc::Context::Init)
            .unwrap();
        finish(&mut vm, init, tiny);
        let task = vm
            .start(main, &[], RootOwner(1), bc::Context::Ordinary)
            .unwrap();
        let mut evaluations = 0;
        assert_eq!(
            drive(&mut vm, task, tiny, &mut evaluations).as_int(),
            Some(42),
            "tiny={tiny}"
        );
        assert_eq!(
            evaluations, 9,
            "five downstream reads and four selected-field evaluations; tiny={tiny}"
        );
    }

    fn drive(vm: &mut Vm, task: TaskId, tiny: bool, evaluations: &mut usize) -> Value {
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
            match outcome {
                RunOutcome::HostCall(ticket) => {
                    if ticket.native == bc::NativeId(0) {
                        vm.respond_model(ticket, 1).unwrap();
                    } else {
                        *evaluations += usize::from(ticket.input == NativeInput::CallbackReturned);
                        vm.respond_derive(ticket).unwrap();
                    }
                }
                RunOutcome::BudgetYield(usage) => {
                    budget.work = usage.required_work.max(1);
                    budget.allocation_bytes = usage.required_allocation;
                }
                RunOutcome::Returned(delivery) => {
                    let value = vm.delivery_value(delivery).unwrap();
                    vm.discard_delivery(delivery).unwrap();
                    return value;
                }
                RunOutcome::Fault(delivery) => {
                    panic!("reactive index fault: {:?}", vm.delivery_fault(delivery))
                }
                other => panic!("unexpected reactive index outcome: {other:?}"),
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
        }
    }
}
